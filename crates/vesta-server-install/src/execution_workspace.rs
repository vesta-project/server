//! Prepared-job file validation, not syscall/network/process-tree confinement.
//! No production recipe can construct an execution invocation for this workspace yet.
use super::{
    execution::{ready, Budget},
    install::{self, InstallRoots, Reservation},
    model::*,
    transfer::controlled,
    ServerInstaller,
};
use crate::progress::ProgressReporter;
use sha1::Sha1;
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
    sync::OnceLock,
};
use tokio::{io::AsyncReadExt, sync::OwnedSemaphorePermit, time::Instant};

pub(super) struct DeclaredFile {
    path: String,
    bytes: u64,
    digest: PublishedDigest,
}
impl DeclaredFile {
    pub(super) fn new(path: &str, bytes: u64, digest: PublishedDigest) -> Result<Self> {
        relative(path)?;
        digest.validate()?;
        Ok(Self {
            path: path.into(),
            bytes,
            digest,
        })
    }
}
fn relative(path: &str) -> Result<()> {
    super::paths::relative(path, |_, _| false)
}

fn declarations(
    inputs: &[DeclaredFile],
    outputs: &[DeclaredFile],
    limits: Limits,
) -> Result<(u64, BTreeSet<String>)> {
    let limits = limits.validate()?;
    if outputs.is_empty()
        || inputs
            .len()
            .checked_add(outputs.len())
            .is_none_or(|n| n > limits.max_entries)
    {
        return Err(InstallError::LimitExceeded);
    }
    let mut bytes = 0u64;
    let mut paths = Vec::new();
    for (tree, entries) in [("inputs", inputs), ("outputs", outputs)] {
        for entry in entries {
            relative(&entry.path)?;
            entry.digest.validate()?;
            if entry.bytes > limits.artifact_bytes {
                return Err(InstallError::LimitExceeded);
            }
            bytes = bytes
                .checked_add(entry.bytes)
                .ok_or(InstallError::LimitExceeded)?;
            if bytes > limits.total_bytes {
                return Err(InstallError::LimitExceeded);
            }
            paths.push(format!("{}/{}", tree, entry.path));
        }
    }
    // The fixed tree roots are created separately and are not declared entries.
    let roots = [inputs.is_empty(), outputs.is_empty()]
        .iter()
        .filter(|empty| !**empty)
        .count();
    let mut directories = super::paths::declarations(
        paths.iter().map(String::as_str),
        limits.max_entries.saturating_add(roots),
    )?;
    directories.remove("inputs");
    directories.remove("outputs");
    Ok((bytes, directories))
}

struct Protection {
    slot: Option<OwnedSemaphorePermit>,
    reservation: Option<Reservation>,
    quarantined: bool,
}
impl Drop for Protection {
    fn drop(&mut self) {
        if self.quarantined {
            if let Some(slot) = self.slot.take() {
                slot.forget();
            }
            if let Some(reservation) = self.reservation.take() {
                std::mem::forget(reservation);
            }
        }
    }
}
pub(super) struct VerifiedFile {
    pub(super) path: String,
    pub(super) bytes: u64,
    pub(super) sha256: String,
}
pub(super) struct Workspace {
    roots: InstallRoots,
    directory: PathBuf,
    work: PathBuf,
    inputs: Vec<DeclaredFile>,
    outputs: Vec<DeclaredFile>,
    declared_directories: HashSet<String>,
    known_files: Vec<PathBuf>,
    known_directories: Vec<PathBuf>,
    limits: Limits,
    protection: Protection,
    execution_started: bool,
    verified: OnceLock<Vec<VerifiedFile>>,
}
impl Workspace {
    pub(super) async fn create(
        installer: &ServerInstaller,
        inputs: Vec<DeclaredFile>,
        outputs: Vec<DeclaredFile>,
        budget: &Budget,
        reporter: &dyn ProgressReporter,
    ) -> Result<Self> {
        Self::create_inner(
            installer,
            inputs,
            outputs,
            budget,
            reporter,
            &format!("job_{}", uuid::Uuid::new_v4().simple()),
        )
        .await
    }
    async fn create_inner(
        installer: &ServerInstaller,
        inputs: Vec<DeclaredFile>,
        outputs: Vec<DeclaredFile>,
        budget: &Budget,
        reporter: &dyn ProgressReporter,
        id: &str,
    ) -> Result<Self> {
        let (bytes, directories) = declarations(&inputs, &outputs, installer.limits)?;
        ready(budget.deadline, reporter).await?;
        if budget.deadline.saturating_duration_since(Instant::now()) > installer.limits.duration() {
            return Err(InstallError::InvalidInput);
        }
        installer.roots.recheck()?;
        let slot = installer.coordinator.slot()?;
        let (volume, free) = installer.space()?;
        let reservation =
            installer
                .coordinator
                .reserve(volume, free, bytes, installer.limits.reserve_bytes)?;
        ready(budget.deadline, reporter).await?;
        let root = installer.roots.base.join(".staging");
        install::guard(&root)?;
        match std::fs::create_dir(&root) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(InstallError::PublishFailed),
        }
        plain_directory(&root)?;
        let directory = root.join(id);
        std::fs::create_dir(&directory).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                InstallError::DestinationExists
            } else {
                InstallError::PublishFailed
            }
        })?;
        let mut workspace = Self {
            roots: installer.roots.clone(),
            work: directory.join("work"),
            directory: directory.clone(),
            inputs,
            outputs,
            declared_directories: directories.iter().cloned().collect(),
            known_files: Vec::new(),
            known_directories: vec![directory],
            limits: installer.limits,
            protection: Protection {
                slot: Some(slot),
                reservation: Some(reservation),
                quarantined: false,
            },
            execution_started: false,
            verified: OnceLock::new(),
        };
        workspace.recheck()?;
        for relative in ["inputs", "work", "outputs"]
            .into_iter()
            .chain(directories.iter().map(String::as_str))
        {
            ready(budget.deadline, reporter).await?;
            let path = workspace.directory.join(relative);
            std::fs::create_dir(&path).map_err(|_| InstallError::PublishFailed)?;
            workspace.known_directories.push(path.clone());
            plain_directory(&path)?;
        }
        workspace.recheck()?;
        Ok(workspace)
    }
    fn recheck(&self) -> Result<()> {
        self.roots.recheck()?;
        plain_directory(&self.directory)?;
        if !self.directory.starts_with(self.roots.base.join(".staging")) {
            return Err(InstallError::PathRefused);
        }
        Ok(())
    }
    pub(super) fn cwd(&self) -> &Path {
        &self.work
    }
    pub(super) fn started(&mut self) {
        self.execution_started = true;
    }
    pub(super) fn spawn_failed(&mut self) {
        // Only a failed spawn, never a child error or kill request, disarms pre-start retention.
        self.execution_started = false;
    }
    pub(super) fn quarantine(&mut self) {
        self.execution_started = true;
        self.protection.quarantined = true;
    }
    pub(super) fn refuses_runtime(&self, path: &Path) -> bool {
        install::overlap(&self.roots.base, path)
    }
    pub(super) fn verified(&self) -> Option<&[VerifiedFile]> {
        self.verified.get().map(Vec::as_slice)
    }
    pub(super) async fn verify_inputs(
        &self,
        deadline: Instant,
        reporter: &dyn ProgressReporter,
    ) -> Result<()> {
        super::execution::check(deadline, reporter)?;
        self.recheck()?;
        plain_directory(&self.work)?;
        plain_directory(&self.directory.join("outputs"))?;
        self.scan_tree("inputs", &self.inputs, deadline, reporter)?;
        self.verify_tree("inputs", &self.inputs, deadline, reporter)
            .await?;
        Ok(())
    }
    pub(super) async fn verify_after(
        &self,
        deadline: Instant,
        reporter: &dyn ProgressReporter,
    ) -> Result<()> {
        self.verify_inputs(deadline, reporter).await?;
        self.scan_tree("outputs", &self.outputs, deadline, reporter)?;
        let files = self
            .verify_tree("outputs", &self.outputs, deadline, reporter)
            .await?;
        self.verified
            .set(files)
            .map_err(|_| InstallError::InvalidInput)
    }
    fn scan_tree(
        &self,
        tree: &str,
        entries: &[DeclaredFile],
        deadline: Instant,
        reporter: &dyn ProgressReporter,
    ) -> Result<()> {
        let root = self.directory.join(tree);
        plain_directory(&root)?;
        let files: HashSet<_> = entries.iter().map(|f| f.path.as_str()).collect();
        let mut pending = vec![root.clone()];
        let mut seen = 0usize;
        while let Some(directory) = pending.pop() {
            check(deadline, reporter)?;
            plain_directory(&directory)?;
            for entry in std::fs::read_dir(directory).map_err(|_| InstallError::PathRefused)? {
                check(deadline, reporter)?;
                seen = seen.checked_add(1).ok_or(InstallError::LimitExceeded)?;
                if seen > self.limits.max_entries {
                    return Err(InstallError::LimitExceeded);
                }
                let path = entry.map_err(|_| InstallError::PathRefused)?.path();
                install::guard(&path)?;
                let name = path
                    .strip_prefix(&root)
                    .map_err(|_| InstallError::PathRefused)?
                    .to_str()
                    .ok_or(InstallError::PathRefused)?
                    .replace('\\', "/");
                let metadata =
                    std::fs::symlink_metadata(&path).map_err(|_| InstallError::PathRefused)?;
                if metadata.is_dir()
                    && self
                        .declared_directories
                        .contains(&format!("{}/{}", tree, name))
                {
                    plain_directory(&path)?;
                    pending.push(path);
                } else if metadata.is_file() && files.contains(name.as_str()) {
                    install::plain_file(&path)?;
                } else {
                    return Err(InstallError::PathRefused);
                }
            }
        }
        Ok(())
    }
    async fn verify_tree(
        &self,
        tree: &str,
        entries: &[DeclaredFile],
        deadline: Instant,
        reporter: &dyn ProgressReporter,
    ) -> Result<Vec<VerifiedFile>> {
        let mut total = 0u64;
        let mut verified = Vec::new();
        for entry in entries {
            check(deadline, reporter)?;
            let path = self.directory.join(tree).join(&entry.path);
            let file = controlled(
                verify_file(
                    &path,
                    entry,
                    &mut total,
                    self.limits.total_bytes,
                    #[cfg(test)]
                    None,
                ),
                deadline,
                reporter,
            )
            .await?;
            verified.push(file);
        }
        Ok(verified)
    }
    #[cfg(test)]
    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }
    #[cfg(test)]
    pub(super) async fn seed_input(
        &mut self,
        index: usize,
        bytes: &[u8],
        budget: &Budget,
        reporter: &dyn ProgressReporter,
    ) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        ready(budget.deadline, reporter).await?;
        self.recheck()?;
        let entry = self.inputs.get(index).ok_or(InstallError::InvalidInput)?;
        if bytes.len() as u64 != entry.bytes {
            return Err(InstallError::ArtifactChanged);
        }
        let path = self.directory.join("inputs").join(&entry.path);
        install::guard(&path)?;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
            .map_err(|_| InstallError::DestinationExists)?;
        self.known_files.push(path.clone());
        controlled(
            async {
                file.write_all(bytes)
                    .await
                    .map_err(|_| InstallError::PublishFailed)?;
                file.sync_all()
                    .await
                    .map_err(|_| InstallError::PublishFailed)
            },
            budget.deadline,
            reporter,
        )
        .await?;
        install::plain_file(&path)
    }
    #[cfg(test)]
    pub(super) async fn collision(
        installer: &ServerInstaller,
        inputs: Vec<DeclaredFile>,
        outputs: Vec<DeclaredFile>,
        budget: &Budget,
        reporter: &dyn ProgressReporter,
        id: &str,
    ) -> Result<Self> {
        if id.len() != 36
            || !id.starts_with("job_")
            || !id[4..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(InstallError::InvalidInput);
        }
        Self::create_inner(installer, inputs, outputs, budget, reporter, id).await
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        // Unknown descendants/processor files are never swept, even after a direct-child exit.
        if self.execution_started || self.protection.quarantined || self.recheck().is_err() {
            return;
        }
        for path in self.known_files.iter().rev() {
            if install::plain_file(path).is_ok() {
                let _ = std::fs::remove_file(path);
            }
        }
        for path in self.known_directories.iter().rev() {
            if plain_directory(path).is_ok() {
                let _ = std::fs::remove_dir(path);
            }
        }
    }
}
fn plain_directory(path: &Path) -> Result<()> {
    install::guard(path)?;
    let metadata = std::fs::symlink_metadata(path).map_err(|_| InstallError::PathRefused)?;
    if !metadata.is_dir()
        || dunce::canonicalize(path).map_err(|_| InstallError::PathRefused)? != path
    {
        return Err(InstallError::PathRefused);
    }
    Ok(())
}
fn check(deadline: Instant, reporter: &dyn ProgressReporter) -> Result<()> {
    if reporter.is_cancelled() {
        return Err(InstallError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(InstallError::Deadline);
    }
    Ok(())
}
#[derive(PartialEq, Eq)]
pub(super) struct FileIdentity {
    volume: u64,
    index: u128,
}
fn file_identity(file: &std::fs::File) -> Result<FileIdentity> {
    file_identity_with_links(file, 1)
}
#[cfg_attr(windows, allow(unsafe_code))]
pub(super) fn file_identity_with_links(file: &std::fs::File, links: u64) -> Result<FileIdentity> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
            BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO,
        };
        // SAFETY: this plain Win32 output record admits zero initialization; the file's
        // borrowed handle and writable record remain live for the read-only call.
        let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0
            || u64::from(information.nNumberOfLinks) != links
        {
            return Err(InstallError::PathRefused);
        }
        // ReFS uses 128-bit identities; the legacy 64-bit index is not sufficient.
        // SAFETY: zeroable output record, live borrowed handle and exact buffer size.
        let mut identity: FILE_ID_INFO = unsafe { std::mem::zeroed() };
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (&mut identity as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            return Err(InstallError::PathRefused);
        }
        Ok(FileIdentity {
            volume: identity.VolumeSerialNumber,
            index: u128::from_le_bytes(identity.FileId.Identifier),
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().map_err(|_| InstallError::PathRefused)?;
        if metadata.nlink() != links {
            return Err(InstallError::PathRefused);
        }
        Ok(FileIdentity {
            volume: metadata.dev(),
            index: u128::from(metadata.ino()),
        })
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = file;
        Err(InstallError::PathRefused)
    }
}
#[cfg(test)]
pub(super) async fn verify_replacement_fixture(
    path: &Path,
    entry: &DeclaredFile,
    after_hash: &(dyn Fn(&Path) + Send + Sync),
) -> Result<VerifiedFile> {
    verify_file(path, entry, &mut 0, entry.bytes, Some(after_hash)).await
}

async fn verify_file(
    path: &Path,
    entry: &DeclaredFile,
    total: &mut u64,
    limit: u64,
    #[cfg(test)] after_hash: Option<&(dyn Fn(&Path) + Send + Sync)>,
) -> Result<VerifiedFile> {
    install::plain_file(path)?;
    let opened = std::fs::File::open(path).map_err(|_| InstallError::PathRefused)?;
    let identity = file_identity(&opened)?;
    let before = opened.metadata().map_err(|_| InstallError::PathRefused)?;
    if before.len() != entry.bytes {
        return Err(InstallError::ArtifactChanged);
    }
    let mut file = tokio::fs::File::from_std(opened);
    let mut sha256 = Sha256::new();
    let mut sha1 = Sha1::new();
    let mut md5 = md5::Md5::new();
    let mut bytes = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|_| InstallError::PathRefused)?;
        if n == 0 {
            break;
        }
        bytes = bytes
            .checked_add(n as u64)
            .ok_or(InstallError::LimitExceeded)?;
        *total = total
            .checked_add(n as u64)
            .ok_or(InstallError::LimitExceeded)?;
        if bytes > entry.bytes || *total > limit {
            return Err(InstallError::LimitExceeded);
        }
        sha256.update(&buffer[..n]);
        sha1.update(&buffer[..n]);
        md5.update(&buffer[..n]);
    }
    let observed = format!("{:x}", sha256.finalize());
    let expected = match entry.digest.algorithm() {
        DigestAlgorithm::Sha256 => observed.clone(),
        DigestAlgorithm::Sha1 => format!("{:x}", sha1.finalize()),
        DigestAlgorithm::Md5 => format!("{:x}", md5.finalize()),
    };
    if bytes != entry.bytes || expected != entry.digest.hex() {
        return Err(InstallError::ArtifactChanged);
    }
    #[cfg(test)]
    if let Some(hook) = after_hash {
        hook(path);
    }
    let opened = file.into_std().await;
    install::plain_file(path)?;
    let current = std::fs::File::open(path).map_err(|_| InstallError::PathRefused)?;
    if file_identity(&opened)? != identity || file_identity(&current)? != identity {
        return Err(InstallError::ArtifactChanged);
    }
    for handle in [&opened, &current] {
        let after = handle.metadata().map_err(|_| InstallError::PathRefused)?;
        if after.len() != before.len() || after.modified().ok() != before.modified().ok() {
            return Err(InstallError::ArtifactChanged);
        }
    }
    install::plain_file(path)?;
    Ok(VerifiedFile {
        path: entry.path.clone(),
        bytes,
        sha256: observed,
    })
}
