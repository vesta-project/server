use super::{
    model::*,
    transfer::{Job, Received, Role, Source},
};
use crate::progress::ProgressReporter;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{OwnedSemaphorePermit, Semaphore},
};

/// Existing trusted server-only base; callers supply actual client/profile/state exclusions.
#[derive(Clone)]
pub struct InstallRoots {
    pub(crate) base: PathBuf,
    pub(crate) forbidden: Vec<PathBuf>,
}
impl InstallRoots {
    pub fn new(base: &Path, forbidden: &[PathBuf]) -> Result<Self> {
        guard(base)?;
        let base = canonical_directory(base)?;
        let mut roots = Vec::new();
        for path in forbidden {
            guard(path)?;
            roots.push(canonical_directory(path)?);
        }
        if roots.iter().any(|p| overlap(p, &base)) {
            return Err(InstallError::PathRefused);
        }
        roots.sort();
        roots.dedup();
        Ok(Self {
            base,
            forbidden: roots,
        })
    }
    pub(crate) fn recheck(&self) -> Result<()> {
        let checked = Self::new(&self.base, &self.forbidden)?;
        if checked.base != self.base || checked.forbidden != self.forbidden {
            return Err(InstallError::PathRefused);
        }
        Ok(())
    }
}
fn canonical_directory(p: &Path) -> Result<PathBuf> {
    if !p.is_absolute() {
        return Err(InstallError::PathRefused);
    }
    let canonical = dunce::canonicalize(p).map_err(|_| InstallError::PathRefused)?;
    if !std::fs::symlink_metadata(&canonical)
        .map_err(|_| InstallError::PathRefused)?
        .is_dir()
        || canonical
            .to_str()
            .is_none_or(|s| s.chars().any(char::is_control))
    {
        return Err(InstallError::PathRefused);
    }
    Ok(canonical)
}
fn comparable(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(
            p.to_string_lossy()
                .trim_start_matches(r"\\?\")
                .to_lowercase(),
        )
    }
    #[cfg(not(windows))]
    {
        p.to_path_buf()
    }
}
pub(crate) fn overlap(a: &Path, b: &Path) -> bool {
    let a = comparable(a);
    let b = comparable(b);
    a.starts_with(&b) || b.starts_with(&a)
}
pub(crate) fn guard(p: &Path) -> Result<()> {
    if !p.is_absolute()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(InstallError::PathRefused);
    }
    for ancestor in p.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(m) if redirected(&m) => return Err(InstallError::PathRefused),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(InstallError::PathRefused),
        }
    }
    Ok(())
}
pub(crate) fn redirected(m: &std::fs::Metadata) -> bool {
    if m.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}
pub(crate) fn plain_file(p: &Path) -> Result<()> {
    guard(p)?;
    let m = std::fs::symlink_metadata(p).map_err(|_| InstallError::PathRefused)?;
    if !m.is_file() || redirected(&m) {
        return Err(InstallError::PathRefused);
    }
    Ok(())
}
pub(super) fn child_directory(p: &Path) -> Result<()> {
    guard(p)?;
    match std::fs::create_dir(p) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if !std::fs::symlink_metadata(p)
                .map_err(|_| InstallError::PathRefused)?
                .is_dir()
            {
                return Err(InstallError::PathRefused);
            }
        }
        Err(_) => return Err(InstallError::PublishFailed),
    }
    guard(p)
}

pub(crate) struct Coordinator {
    slots: Arc<Semaphore>,
    reservations: Mutex<HashMap<PathBuf, u64>>,
}
impl Coordinator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            slots: Arc::new(Semaphore::new(2)),
            reservations: Mutex::new(HashMap::new()),
        })
    }
    pub fn shared() -> Arc<Self> {
        static VALUE: OnceLock<Arc<Coordinator>> = OnceLock::new();
        VALUE.get_or_init(Self::new).clone()
    }
    pub fn slot(&self) -> Result<OwnedSemaphorePermit> {
        self.slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| InstallError::Busy)
    }
    pub fn reserve(
        self: &Arc<Self>,
        volume: PathBuf,
        available: u64,
        bytes: u64,
        floor: u64,
    ) -> Result<Reservation> {
        let mut values = self
            .reservations
            .lock()
            .map_err(|_| InstallError::StorageUnknown)?;
        let reserved = values.get(&volume).copied().unwrap_or(0);
        let needed = bytes
            .checked_add(floor)
            .ok_or(InstallError::LimitExceeded)?;
        if available.checked_sub(reserved).is_none_or(|n| n < needed) {
            return Err(InstallError::StorageLow);
        }
        values.insert(
            volume.clone(),
            reserved
                .checked_add(bytes)
                .ok_or(InstallError::LimitExceeded)?,
        );
        Ok(Reservation {
            coordinator: self.clone(),
            volume,
            bytes,
        })
    }
}
pub(crate) struct Reservation {
    coordinator: Arc<Coordinator>,
    volume: PathBuf,
    bytes: u64,
}
impl Reservation {
    pub(super) fn recheck(&self, volume: &Path, available: u64, floor: u64) -> Result<()> {
        if volume != self.volume {
            return Err(InstallError::StorageUnknown);
        }
        let values = self
            .coordinator
            .reservations
            .lock()
            .map_err(|_| InstallError::StorageUnknown)?;
        let other = values
            .get(&self.volume)
            .copied()
            .unwrap_or(0)
            .checked_sub(self.bytes)
            .ok_or(InstallError::StorageUnknown)?;
        if available.checked_sub(other).is_none_or(|n| n < floor) {
            return Err(InstallError::StorageLow);
        }
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut values) = self.coordinator.reservations.lock() {
            if let Some(value) = values.get_mut(&self.volume) {
                *value = value.saturating_sub(self.bytes);
                if *value == 0 {
                    values.remove(&self.volume);
                }
            }
        }
    }
}

pub(crate) fn local_filesystem(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    #[cfg(windows)]
    {
        matches!(name.as_str(), "ntfs" | "refs")
    }
    #[cfg(not(windows))]
    {
        unix_local_filesystem(&name)
    }
}
#[cfg(any(not(windows), test))]
fn unix_local_filesystem(name: &str) -> bool {
    matches!(
        name,
        "ext2" | "ext3" | "ext4" | "xfs" | "btrfs" | "zfs" | "apfs" | "hfs" | "hfsplus"
    )
}
#[cfg_attr(windows, allow(unsafe_code))]
fn local_mount(mount: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::{
            Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW},
            System::SystemServices::{FILE_PERSISTENT_ACLS, FILE_SUPPORTS_HARD_LINKS},
            System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE},
        };
        let root: Vec<u16> = mount.as_os_str().encode_wide().chain(Some(0)).collect();
        // Read-only volume classification, not path-handle confinement or alias certification.
        // SAFETY: the owned UTF-16 root is NUL-terminated and live throughout this read-only call.
        if !matches!(
            unsafe { GetDriveTypeW(root.as_ptr()) },
            DRIVE_FIXED | DRIVE_REMOVABLE
        ) {
            return Err(InstallError::StorageUnknown);
        }
        let mut flags = 0;
        // SAFETY: live NUL-terminated root and writable flags; unused outputs are null.
        if unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut flags,
                std::ptr::null_mut(),
                0,
            )
        } == 0
            || flags & (FILE_PERSISTENT_ACLS | FILE_SUPPORTS_HARD_LINKS)
                != (FILE_PERSISTENT_ACLS | FILE_SUPPORTS_HARD_LINKS)
        {
            return Err(InstallError::StorageUnknown);
        }
    }
    #[cfg(not(windows))]
    let _ = mount;
    Ok(())
}
pub(crate) fn select_volume(
    base: &Path,
    volumes: impl IntoIterator<Item = (PathBuf, u64, bool)>,
) -> Result<(PathBuf, u64)> {
    let normalized = comparable(base);
    let mut best = None;
    for (mount, free, local) in volumes {
        let mount = comparable(&mount);
        if normalized.starts_with(&mount) {
            let depth = mount.components().count();
            if best.as_ref().is_none_or(|(d, _, _, _, _)| depth > *d) {
                best = Some((depth, mount, free, local, false));
            } else if let Some((d, _, _, _, ambiguous)) = best.as_mut() {
                if depth == *d {
                    *ambiguous = true;
                }
            }
        }
    }
    let (_, mount, free, local, ambiguous) = best.ok_or(InstallError::StorageUnknown)?;
    if !local || ambiguous {
        return Err(InstallError::StorageUnknown);
    }
    Ok((mount, free))
}
#[cfg(any(target_os = "linux", test))]
const MOUNTINFO_BYTES: usize = 1024 * 1024;
#[cfg(any(target_os = "linux", test))]
const MOUNTINFO_ENTRIES: usize = 10_000;

#[cfg(any(target_os = "linux", test))]
fn mountinfo_path(raw: &str) -> Result<PathBuf> {
    let mut bytes = Vec::with_capacity(raw.len());
    let mut rest = raw.as_bytes();
    while let Some((&first, tail)) = rest.split_first() {
        if first == b'\\' {
            let value = match rest.get(..4) {
                Some(b"\\040") => b' ',
                Some(b"\\011") => b'\t',
                Some(b"\\012") => b'\n',
                Some(b"\\134") => b'\\',
                _ => return Err(InstallError::StorageUnknown),
            };
            bytes.push(value);
            rest = &rest[4..];
        } else {
            bytes.push(first);
            rest = tail;
        }
    }
    let path = std::str::from_utf8(&bytes).map_err(|_| InstallError::StorageUnknown)?;
    if !path.starts_with('/')
        || Path::new(path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(InstallError::StorageUnknown);
    }
    Ok(PathBuf::from(path))
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn linux_mountinfo(base: &Path, data: &[u8]) -> Result<PathBuf> {
    if data.len() > MOUNTINFO_BYTES {
        return Err(InstallError::StorageUnknown);
    }
    let text = std::str::from_utf8(data).map_err(|_| InstallError::StorageUnknown)?;
    let mut volumes = Vec::new();
    for line in text.lines() {
        if volumes.len() == MOUNTINFO_ENTRIES {
            return Err(InstallError::StorageUnknown);
        }
        let (before, after) = line.split_once(" - ").ok_or(InstallError::StorageUnknown)?;
        let mut fields = before.split_ascii_whitespace();
        for _ in 0..2 {
            fields
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or(InstallError::StorageUnknown)?;
        }
        let device = fields.next().ok_or(InstallError::StorageUnknown)?;
        let (major, minor) = device.split_once(':').ok_or(InstallError::StorageUnknown)?;
        for value in [major, minor] {
            value
                .parse::<u64>()
                .map_err(|_| InstallError::StorageUnknown)?;
        }
        mountinfo_path(fields.next().ok_or(InstallError::StorageUnknown)?)?;
        let mount = mountinfo_path(fields.next().ok_or(InstallError::StorageUnknown)?)?;
        fields.next().ok_or(InstallError::StorageUnknown)?;
        let mut fields = after.split_ascii_whitespace();
        let filesystem = fields.next().ok_or(InstallError::StorageUnknown)?;
        fields.next().ok_or(InstallError::StorageUnknown)?;
        fields.next().ok_or(InstallError::StorageUnknown)?;
        if fields.next().is_some() {
            return Err(InstallError::StorageUnknown);
        }
        volumes.push((mount, 0, unix_local_filesystem(filesystem)));
    }
    select_volume(base, volumes).map(|(mount, _)| mount)
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn read_linux_mountinfo(base: &Path, reader: impl std::io::Read) -> Result<PathBuf> {
    use std::io::Read;
    let mut data = Vec::new();
    reader
        .take((MOUNTINFO_BYTES + 1) as u64)
        .read_to_end(&mut data)
        .map_err(|_| InstallError::StorageUnknown)?;
    linux_mountinfo(base, &data)
}

pub(crate) fn confirmed_volume(
    base: &Path,
    volumes: impl IntoIterator<Item = (PathBuf, u64, bool)>,
    expected: Option<&Path>,
) -> Result<(PathBuf, u64)> {
    let (mount, free) = select_volume(base, volumes)?;
    if expected.is_some_and(|p| comparable(p) != mount) {
        return Err(InstallError::StorageUnknown);
    }
    Ok((mount, free))
}

pub(crate) fn disk_space(base: &Path) -> Result<(PathBuf, u64)> {
    // sysinfo omits some Linux mounts, including network filesystems and /run.
    // An omitted mount must not fall back to capacity from its visible parent.
    #[cfg(target_os = "linux")]
    let expected = {
        let file = std::fs::File::open("/proc/self/mountinfo")
            .map_err(|_| InstallError::StorageUnknown)?;
        Some(read_linux_mountinfo(base, file)?)
    };
    #[cfg(not(target_os = "linux"))]
    let expected: Option<PathBuf> = None;
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let (mount, free) = confirmed_volume(
        base,
        disks.list().iter().map(|disk| {
            (
                disk.mount_point().to_path_buf(),
                disk.available_space(),
                disk.file_system().to_str().is_some_and(local_filesystem),
            )
        }),
        expected.as_deref(),
    )?;
    local_mount(&mount)?;
    Ok((mount, free))
}

#[derive(Debug, Clone)]
pub struct Receipt {
    pub built: Option<super::providers::buildtools::BuiltReceipt>,
    pub schema: u32,
    pub install_id: String,
    pub plan_hash: String,
    pub software: Software,
    pub version: String,
    pub build: Option<u32>,
    pub published_digest: PublishedDigest,
    pub published_size: Option<u64>,
    pub observed_sha256: String,
    pub actual_size: u64,
    pub java_requirement: JavaRequirement,
    pub launch: LaunchRecipe,
    pub eula: String,
    pub warnings: Vec<Warning>,
    pub completed_at: u64,
    pub bundle: Option<super::bundle::BundleReceipt>,
    pub processed: Option<super::providers::forge::ProcessedReceipt>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyReceipt {
    pub schema: u32,
    pub install_id: String,
    pub plan_hash: String,
    pub software: Software,
    pub version: String,
    pub build: Option<u32>,
    pub published_digest: PublishedDigest,
    pub published_size: Option<u64>,
    pub observed_sha256: String,
    pub actual_size: u64,
    pub java_requirement: JavaRequirement,
    pub launch: LaunchRecipe,
    pub eula: String,
    pub warnings: Vec<Warning>,
    pub completed_at: u64,
}
impl Serialize for Receipt {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        if let Some(built) = &self.built {
            return built.serialize(serializer);
        }
        if let Some(processed) = &self.processed {
            return processed.serialize(serializer);
        }
        if let Some(bundle) = &self.bundle {
            return bundle.serialize(serializer);
        }
        LegacyReceipt {
            schema: self.schema,
            install_id: self.install_id.clone(),
            plan_hash: self.plan_hash.clone(),
            software: self.software,
            version: self.version.clone(),
            build: self.build,
            published_digest: self.published_digest.clone(),
            published_size: self.published_size,
            observed_sha256: self.observed_sha256.clone(),
            actual_size: self.actual_size,
            java_requirement: self.java_requirement.clone(),
            launch: self.launch.clone(),
            eula: self.eula.clone(),
            warnings: self.warnings.clone(),
            completed_at: self.completed_at,
        }
        .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Receipt {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        // LegacyReceipt has exactly the original fields; even a null bundle is unknown.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Built(Box<super::providers::buildtools::BuiltReceipt>),
            Processed(super::providers::forge::ProcessedReceipt),
            Single(LegacyReceipt),
            Bundle(super::bundle::BundleReceipt),
        }
        match Wire::deserialize(deserializer)? {
            Wire::Built(r) => (*r)
                .into_receipt()
                .map_err(|_| serde::de::Error::custom("invalid source receipt")),
            Wire::Processed(r) => r
                .into_receipt()
                .map_err(|_| serde::de::Error::custom("invalid processed receipt")),
            Wire::Single(r) if r.schema == 1 => Ok(Self {
                schema: r.schema,
                install_id: r.install_id,
                plan_hash: r.plan_hash,
                software: r.software,
                version: r.version,
                build: r.build,
                published_digest: r.published_digest,
                published_size: r.published_size,
                observed_sha256: r.observed_sha256,
                actual_size: r.actual_size,
                java_requirement: r.java_requirement,
                launch: r.launch,
                eula: r.eula,
                warnings: r.warnings,
                completed_at: r.completed_at,
                bundle: None,
                processed: None,
                built: None,
            }),
            Wire::Single(_) => Err(serde::de::Error::custom("invalid receipt schema")),
            Wire::Bundle(r) => r
                .into_receipt()
                .map_err(|_| serde::de::Error::custom("invalid receipt")),
        }
    }
}
// Preserve the existing public unboxed receipt payload at the compatibility boundary.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum InstallOutcome {
    Installed(Receipt),
    DryRun,
}

// Staging owns only two known files. Never recursively delete an unexpected tree.
struct Stage {
    directory: PathBuf,
    artifact: PathBuf,
    receipt: PathBuf,
}
impl Stage {
    fn create(base: &Path) -> Result<Self> {
        let root = base.join(".staging");
        child_directory(&root)?;
        let directory = root.join(format!("job_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&directory).map_err(|_| InstallError::PublishFailed)?;
        let stage = Self {
            artifact: directory.join("artifact.part"),
            receipt: directory.join("receipt.part"),
            directory,
        };
        guard(&stage.directory)?;
        Ok(stage)
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        if guard(&self.directory).is_ok() {
            for path in [&self.artifact, &self.receipt] {
                if plain_file(path).is_ok() {
                    let _ = std::fs::remove_file(path);
                }
            }
            let _ = std::fs::remove_dir(&self.directory);
        }
    }
}

pub(crate) async fn execute(
    installer: &super::ServerInstaller,
    plan: &InstallPlan,
    approval: Approval,
    reporter: &dyn ProgressReporter,
) -> Result<InstallOutcome> {
    if approval.hash != plan.hash()
        || plan.base != installer.roots.base
        || plan.forbidden != installer.roots.forbidden
        || plan.limits != installer.limits
        || plan.connection_policy != installer.http.policy_id
    {
        return Err(InstallError::ApprovalMismatch);
    }
    if super::now()? >= plan.expires {
        return Err(InstallError::Expired);
    }
    installer.roots.recheck()?;
    plan.runtime.recheck()?;
    let destination = plan.destination();
    guard(&destination)?;
    if std::fs::symlink_metadata(&destination).is_ok() {
        return Err(InstallError::DestinationExists);
    }
    if reporter.is_cancelled() {
        return Err(InstallError::Cancelled);
    }
    if plan.schema == 3 {
        if reporter.is_dry_run() {
            return Ok(InstallOutcome::DryRun);
        }
        return super::providers::forge::execute(installer, plan, reporter).await;
    }
    if plan.processed.is_some() {
        return Err(InstallError::InvalidInput);
    }
    if plan.schema == 2 {
        if plan.artifact.is_some() || plan.build.is_some() {
            return Err(InstallError::InvalidInput);
        }
        super::bundle::validate(
            plan.software,
            plan.bundle.as_ref().ok_or(InstallError::InvalidInput)?,
            plan.limits,
        )?;
    }
    if reporter.is_dry_run() {
        return Ok(InstallOutcome::DryRun);
    }
    if plan.schema == 2 {
        return super::bundle::execute(installer, plan, reporter).await;
    }
    if !matches!(plan.schema, 1 | 4)
        || plan.bundle.is_some()
        || (plan.schema == 4) != plan.source_build.is_some()
    {
        return Err(InstallError::InvalidInput);
    }
    let artifact = plan.artifact.as_ref().ok_or(InstallError::InvalidInput)?;
    let _slot = installer.coordinator.slot()?;
    let (volume, free) = installer.space()?;
    let bytes = if plan.source_build.is_some() {
        plan.limits
            .total_bytes
            .checked_add(plan.limits.artifact_bytes)
            .ok_or(InstallError::LimitExceeded)?
    } else {
        artifact.size.unwrap_or(plan.limits.artifact_bytes)
    };
    let reservation =
        installer
            .coordinator
            .reserve(volume, free, bytes, plan.limits.reserve_bytes)?;
    let mut job = Job::new(plan.limits, plan.transferred);
    let stage = Stage::create(&plan.base)?;
    let (received, built_input) = if plan.source_build.is_some() {
        let output = Box::pin(super::providers::buildtools::build(
            installer,
            plan,
            &stage.artifact,
            reporter,
        ))
        .await?;
        // Compilation has its own reviewed budget. Publication keeps the normal
        // short deadline instead of inheriting an expired download deadline.
        job.deadline = tokio::time::Instant::now() + plan.limits.duration();
        (output.received, Some(output.input))
    } else {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage.artifact)
            .await
            .map_err(|_| InstallError::PublishFailed)?;
        reporter.start_step("Downloading server software", Some(1));
        let received = installer
            .http
            .receive(
                Source {
                    url: &artifact.url,
                    software: plan.software,
                },
                Role::Artifact,
                Some(artifact),
                &mut job,
                reporter,
                Some(&mut file),
            )
            .await?;
        drop(file);
        (received, None)
    };
    let Received {
        size,
        observed_sha256,
        ..
    } = received;
    job.ready(reporter).await?;
    if super::now()? >= plan.expires {
        return Err(InstallError::Expired);
    }
    installer.roots.recheck()?;
    plan.runtime.recheck()?;
    let (volume, free) = installer.space()?;
    reservation.recheck(&volume, free, plan.limits.reserve_bytes)?;
    plain_file(&stage.artifact)?;
    let receipt = if let Some(input) = built_input {
        let recipe = plan
            .source_build
            .as_ref()
            .ok_or(InstallError::InvalidInput)?;
        let (sources, source_url, source_sha256) = recipe.source_evidence(plan)?;
        super::providers::buildtools::BuiltReceipt {
            schema: 4,
            install_id: plan.install_id.clone(),
            plan_hash: plan.hash(),
            software: plan.software,
            version: plan.version.clone(),
            build: plan.build.ok_or(InstallError::InvalidInput)?,
            sources,
            source_url,
            source_sha256,
            selected_java_major: plan.runtime.major,
            reference_sha256: recipe.output_reference(),
            input,
            output: GeneratedFile {
                path: plan.software.jar_path(),
                size,
                sha256: observed_sha256,
            },
            java_requirement: plan.required_java.clone(),
            launch: plan.launch_recipe(),
            warnings: plan.warnings.clone(),
            completed_at: super::now()?,
        }
        .into_receipt()?
    } else {
        Receipt {
            built: None,
            schema: plan.schema,
            install_id: plan.install_id.clone(),
            plan_hash: plan.hash(),
            software: plan.software,
            version: plan.version.clone(),
            build: plan.build,
            published_digest: artifact.digest.clone(),
            published_size: artifact.size,
            observed_sha256,
            actual_size: size,
            java_requirement: plan.required_java.clone(),
            launch: plan.launch_recipe(),
            eula: "not_accepted".into(),
            warnings: plan.warnings.clone(),
            completed_at: super::now()?,
            bundle: None,
            processed: None,
        }
    };
    let data = serde_json::to_vec(&receipt).map_err(|_| InstallError::ReceiptInvalid)?;
    if data.len() > 65_536 {
        return Err(InstallError::LimitExceeded);
    }
    let mut r = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stage.receipt)
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    r.write_all(&data)
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    r.sync_all()
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    drop(r);
    job.ready(reporter).await?;
    if super::now()? >= plan.expires {
        return Err(InstallError::Expired);
    }
    installer.roots.recheck()?;
    plan.runtime.recheck()?;
    let parent = plan.base.join("instances");
    child_directory(&parent)?;
    guard(&destination)?;
    // A collision, including an empty directory, is a refusal. No replacing rename.
    std::fs::create_dir(&destination).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            InstallError::DestinationExists
        } else {
            InstallError::PublishFailed
        }
    })?;
    // Interrupted/failed publication leaves this new bundle for explicit reconciliation.
    // Never erase a bundle which could now contain user data; absence of receipt means incomplete.
    for name in ["software", "game", "control"] {
        guard(&destination)?;
        std::fs::create_dir(destination.join(name)).map_err(|_| InstallError::PublishFailed)?;
    }
    job.ready(reporter).await?;
    guard(&destination)?;
    std::fs::hard_link(&stage.artifact, destination.join(plan.software.jar_path()))
        .map_err(|_| InstallError::PublishFailed)?;
    #[cfg(test)]
    if installer.fail_after_artifact {
        return Err(InstallError::PublishFailed);
    }
    job.ready(reporter).await?;
    if super::now()? >= plan.expires {
        return Err(InstallError::Expired);
    }
    installer.roots.recheck()?;
    plan.runtime.recheck()?;
    guard(&destination)?;
    plain_file(&stage.receipt)?;
    std::fs::hard_link(&stage.receipt, destination.join("control/receipt.json"))
        .map_err(|_| InstallError::PublishFailed)?;
    // Receipt is the commit marker. Cancellation afterward cannot roll back a completed install.
    reporter.done(
        true,
        Some("Server software installed; EULA not accepted; server not started"),
    );
    Ok(InstallOutcome::Installed(receipt))
}

pub(crate) async fn verify(plan: &InstallPlan) -> Result<Receipt> {
    let deadline = tokio::time::Instant::now() + plan.limits.duration();
    tokio::time::timeout_at(deadline, verify_inner(plan, deadline))
        .await
        .map_err(|_| InstallError::Deadline)?
}
async fn verify_inner(plan: &InstallPlan, deadline: tokio::time::Instant) -> Result<Receipt> {
    if plan.schema == 4 {
        return super::providers::buildtools::verify(plan).await;
    }
    if plan.schema == 3 {
        return super::providers::forge::verify(plan, deadline).await;
    }
    if plan.processed.is_some() {
        return Err(InstallError::ReceiptInvalid);
    }
    if plan.schema == 2 {
        return super::bundle::verify(plan, deadline).await;
    }
    if !matches!(plan.schema, 1 | 4)
        || plan.bundle.is_some()
        || (plan.schema == 4) != plan.source_build.is_some()
    {
        return Err(InstallError::ReceiptInvalid);
    }
    let artifact = plan.artifact.as_ref().ok_or(InstallError::ReceiptInvalid)?;
    let destination = plan.destination();
    let path = destination.join("control/receipt.json");
    plain_file(&path)?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| InstallError::ReceiptInvalid)?;
    let mut bytes = Vec::new();
    file.take(65_537)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| InstallError::ReceiptInvalid)?;
    if bytes.len() > 65_536 {
        return Err(InstallError::ReceiptInvalid);
    }
    let r: Receipt = serde_json::from_slice(&bytes).map_err(|_| InstallError::ReceiptInvalid)?;
    r.published_digest
        .validate()
        .map_err(|_| InstallError::ReceiptInvalid)?;
    if r.bundle.is_some()
        || r.schema != plan.schema
        || r.plan_hash != plan.hash()
        || r.install_id != plan.install_id
        || r.software != plan.software
        || r.version != plan.version
        || r.build != plan.build
        || r.published_digest != artifact.digest
        || r.published_size != artifact.size
        || r.java_requirement != plan.required_java
        || r.launch != plan.launch_recipe()
        || r.eula != "not_accepted"
        || r.warnings != plan.warnings
        || r.completed_at == 0
        || r.completed_at >= plan.expires
        || r.completed_at > super::now()?
        || r.actual_size == 0
        || r.actual_size > plan.limits.artifact_bytes
        || r.published_size.is_some_and(|n| n != r.actual_size)
        || r.observed_sha256.len() != 64
        || !r.observed_sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(InstallError::ReceiptInvalid);
    }
    let artifact_path = destination.join(plan.software.jar_path());
    plain_file(&artifact_path)?;
    let mut file = tokio::fs::File::open(artifact_path)
        .await
        .map_err(|_| InstallError::ReceiptInvalid)?;
    let mut h = Sha256::new();
    let mut legacy = sha1::Sha1::new();
    let mut weak = md5::Md5::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|_| InstallError::ReceiptInvalid)?;
        if n == 0 {
            break;
        }
        size = size
            .checked_add(n as u64)
            .ok_or(InstallError::ReceiptInvalid)?;
        if size > r.actual_size {
            return Err(InstallError::ReceiptInvalid);
        }
        h.update(&buffer[..n]);
        legacy.update(&buffer[..n]);
        weak.update(&buffer[..n]);
    }
    let observed = format!("{:x}", h.finalize());
    let published = match artifact.digest.algorithm {
        DigestAlgorithm::Sha256 => observed.clone(),
        DigestAlgorithm::Sha1 => format!("{:x}", legacy.finalize()),
        DigestAlgorithm::Md5 => format!("{:x}", weak.finalize()),
    };
    if size != r.actual_size || observed != r.observed_sha256 || published != artifact.digest.hex {
        return Err(InstallError::ReceiptInvalid);
    }
    Ok(r)
}
