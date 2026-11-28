//! Read-only copied-export admission and copying through the existing per-file
//! mutation journal. Source roots are selected by local composition, not HTTP.
use super::{
    FileBrowser, FileError, RelativePath, Result, io_error, native,
    root::{CheckedPath, Root},
};
use crate::identity::Subject;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

#[derive(Clone)]
pub(crate) struct CopiedExport {
    root: Root,
    limits: CopyLimits,
}
/// Inventory bounds. The default keeps the pack catalogue bounds used by
/// onboarding; host-scope imports use [`CopyLimits::IMPORT`], where the real
/// bound is the destination free-space preflight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CopyLimits {
    /// Files plus directories.
    pub entries: usize,
    pub file_bytes: u64,
    pub total_bytes: u64,
}
impl Default for CopyLimits {
    fn default() -> Self {
        Self {
            entries: super::MAX_DIRECTORY_ENTRIES,
            file_bytes: super::mutation::MAX_INTENT_STAGED_BYTES,
            total_bytes: vesta_protocol::pack::catalogue::MAX_TOTAL_SIZE,
        }
    }
}
impl CopyLimits {
    /// Large modded servers: 2 million entries, 16 GiB per file, 16 TiB total.
    pub const IMPORT: Self = Self {
        entries: 2_000_000,
        file_bytes: 16 * 1024 * 1024 * 1024,
        total_bytes: 16 * 1024 * 1024 * 1024 * 1024,
    };
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CopyEntry {
    pub path: String,
    pub directory: bool,
    pub size: u64,
    pub sha256: String,
    pub identity: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) version: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CopySnapshot {
    pub entries: Vec<CopyEntry>,
    pub bytes: u64,
    pub sha256: String,
}
impl CopySnapshot {
    pub(crate) fn same_content(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self.entries.iter().zip(&other.entries).all(|(a, b)| {
                (a.path.as_str(), a.directory, a.size, a.sha256.as_str())
                    == (b.path.as_str(), b.directory, b.size, b.sha256.as_str())
            })
    }
}
impl CopiedExport {
    pub(crate) fn open(source: &Path, excluded: &[&Path]) -> Result<Self> {
        Ok(Self {
            root: Root::open(source, excluded)?,
            limits: CopyLimits::default(),
        })
    }
    pub(crate) fn with_limits(mut self, limits: CopyLimits) -> Self {
        self.limits = limits;
        self
    }
    fn file(&self, entry: &CopyEntry) -> Result<File> {
        let relative = RelativePath::parse(&entry.path)?;
        let checked = self.root.resolve(Some(&relative))?;
        let file = self.admitted_file(&checked)?;
        if file.metadata().map_err(io_error)?.len() != entry.size
            || (!entry.identity.is_empty() && native::file_identity(&file)? != entry.identity)
        {
            return Err(FileError::Corrupt);
        }
        Ok(file)
    }

    fn admitted_file(&self, checked: &CheckedPath) -> Result<File> {
        let file = native::open_regular(&checked.path)?;
        if self
            .root
            .resource(&native::actual_path(&file, &checked.path)?)?
            != checked.resource
        {
            return Err(FileError::LinkedPath);
        }
        Ok(file)
    }
    pub(crate) fn snapshot(&self) -> Result<CopySnapshot> {
        self.snapshot_observed(|_| {})
    }

    /// Snapshot with running totals reported after every hashed file.
    pub(crate) fn snapshot_observed(
        &self,
        mut observe: impl FnMut(CopyProgress),
    ) -> Result<CopySnapshot> {
        let mut progress = CopyProgress::default();
        let mut entries = Vec::new();
        self.walk(|path, checked| {
            let mut entry = CopyEntry {
                path,
                directory: checked.metadata.is_dir(),
                size: 0,
                sha256: String::new(),
                identity: String::new(),
                version: String::new(),
            };
            if !entry.directory {
                let mut file = self.admitted_file(checked)?;
                entry.size = checked.metadata.len();
                entry.identity = native::file_identity(&file)?;
                let version = native::file_version(&file).ok();
                entry.sha256 = digest_observed(&mut file, entry.size, version.as_ref())?;
                entry.version = reusable_key(version);
                progress.files += 1;
                progress.bytes += entry.size;
                observe(progress);
            }
            entries.push(entry);
            Ok(())
        })?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let encoded = serde_json::to_vec(&entries).map_err(|_| FileError::Corrupt)?;
        Ok(CopySnapshot {
            entries,
            bytes: progress.bytes,
            sha256: hex::encode(Sha256::digest(encoded)),
        })
    }

    /// Check exact membership, file kind, and bytes. Reuse a reviewed digest
    /// only when the opened file has the same change-aware version. Legacy
    /// reviews and filesystems without a change time verify by reading bytes.
    pub(crate) fn validate_snapshot(
        &self,
        reviewed: &CopySnapshot,
        mut observe: impl FnMut(CopyProgress),
    ) -> Result<()> {
        let mut matched = 0;
        let mut progress = CopyProgress::default();
        self.walk(|path, checked| {
            let index = reviewed
                .entries
                .binary_search_by(|entry| entry.path.cmp(&path))
                .map_err(|_| FileError::Corrupt)?;
            let entry = &reviewed.entries[index];
            if checked.metadata.is_dir() != entry.directory {
                return Err(FileError::Corrupt);
            }
            if !entry.directory {
                if checked.metadata.len() != entry.size {
                    return Err(FileError::Corrupt);
                }
                let mut file = self.admitted_file(checked)?;
                let version = native::file_version(&file).ok();
                if !version
                    .as_ref()
                    .is_some_and(|current| current.reusable && current.key == entry.version)
                    && (native::file_identity(&file)? != entry.identity
                        || digest_observed(&mut file, entry.size, version.as_ref())?
                            != entry.sha256)
                {
                    return Err(FileError::Corrupt);
                }
                progress.files += 1;
                progress.bytes += entry.size;
                observe(progress);
            }
            matched += 1;
            Ok(())
        })?;
        if matched != reviewed.entries.len() {
            return Err(FileError::Corrupt);
        }
        Ok(())
    }

    fn walk(&self, mut observe: impl FnMut(String, &CheckedPath) -> Result<()>) -> Result<()> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(unix)]
        let device = self.root.resolve(None)?.metadata.dev();
        let mut queue = vec![String::new()];
        let mut count = 0;
        let mut bytes = 0u64;
        while let Some(directory) = queue.pop() {
            let relative = if directory.is_empty() {
                None
            } else {
                Some(RelativePath::parse(&directory)?)
            };
            let checked = self.root.resolve(relative.as_ref())?;
            for item in fs::read_dir(&checked.path).map_err(io_error)? {
                if count == self.limits.entries {
                    return Err(FileError::TooLarge);
                }
                count += 1;
                let item = item.map_err(io_error)?;
                let name = item
                    .file_name()
                    .into_string()
                    .map_err(|_| FileError::InvalidPath)?;
                // Vesta's own secrets and state never travel in a copy. They sit at the top of a
                // Vesta folder; plugins keep files with the same names deeper down (Floodgate's
                // key.pem), so only the state database is refused at any depth.
                let vesta_secret = ["setup.json", "host.toml", "key.pem", "certificate.pem"]
                    .iter()
                    .any(|n| name.eq_ignore_ascii_case(n));
                if name.eq_ignore_ascii_case("vesta-state.db")
                    || (directory.is_empty() && vesta_secret)
                {
                    return Err(FileError::ProtectedPath);
                }
                let path = if directory.is_empty() {
                    name
                } else {
                    format!("{directory}/{name}")
                };
                let relative = RelativePath::parse(&path)?;
                let checked = self.root.resolve(Some(&relative))?;
                #[cfg(unix)]
                if checked.metadata.dev() != device {
                    return Err(FileError::LinkedPath);
                }
                if checked.metadata.is_dir() {
                    queue.push(path.clone());
                } else if checked.metadata.is_file() {
                    let size = checked.metadata.len();
                    if size > self.limits.file_bytes {
                        return Err(FileError::TooLarge);
                    }
                    bytes = bytes.checked_add(size).ok_or(FileError::TooLarge)?;
                    if bytes > self.limits.total_bytes {
                        return Err(FileError::TooLarge);
                    }
                } else {
                    return Err(FileError::NotRegular);
                }
                observe(path, &checked)?;
            }
        }
        Ok(())
    }
    pub(crate) fn copy_into(
        &self,
        snapshot: &CopySnapshot,
        destination: &FileBrowser,
        subject: &Subject,
        mut boundary: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        let source = self.root.resolve(None)?.path;
        let target = destination.root.resolve(None)?.path;
        if source.starts_with(&target) || target.starts_with(&source) {
            return Err(FileError::InvalidRoot);
        }
        self.validate_snapshot(snapshot, |_| {})?;
        super::require_space(&target, snapshot.bytes)?;
        // Snapshot ordering is lexical: each parent precedes its descendants.
        destination.reconcile_pending()?;
        for (index, entry) in snapshot.entries.iter().enumerate() {
            boundary()?;
            if index > 0 && index % 64 == 0 {
                destination.reconcile_pending()?;
            }
            let path = RelativePath::parse(&entry.path)?;
            match destination.root.resolve(Some(&path)) {
                Ok(existing) if entry.directory && existing.metadata.is_dir() => continue,
                Ok(existing) if !entry.directory && existing.metadata.is_file() => {
                    let mut file = native::open_regular(&existing.path)?;
                    destination
                        .root
                        .resource(&native::actual_path(&file, &existing.path)?)?;
                    if digest(&mut file, entry.size)? != entry.sha256 {
                        return Err(FileError::Corrupt);
                    }
                    continue;
                }
                Ok(_) => return Err(FileError::Corrupt),
                Err(FileError::NotFound) => (),
                Err(error) => return Err(error),
            }
            if entry.directory {
                destination.mkdir(subject, &path)?;
                continue;
            }
            let mut file = self.file(entry)?;
            destination.copy_admitted_file(subject, &path, &mut file, entry.size, &entry.sha256)?;
        }
        destination.reconcile_pending()?;
        // Membership and content must still match the reviewed source. A new file
        // or a changed payload after a partial copy leaves setup incomplete.
        self.validate_snapshot(snapshot, |_| {})?;
        let copied = Self {
            root: destination.root.clone(),
            limits: self.limits,
        }
        .snapshot()?;
        if !copied.same_content(snapshot) {
            return Err(FileError::Corrupt);
        }
        Ok(())
    }
}
/// Running totals reported to [`CopiedExport::copy_to_directory`] boundaries.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CopyProgress {
    pub files: u64,
    pub bytes: u64,
}
pub(crate) const PART_SUFFIX: &str = ".vesta-part";
const SYNC_BATCH_FILES: usize = 64;
const SYNC_WORKERS: usize = 8;

impl CopiedExport {
    /// Copy into private, unregistered staging. A restart hashes any files
    /// already there, discards torn writes, and syncs reused files again. Whole
    /// batches are flushed concurrently before the tree can be published.
    pub(crate) fn copy_to_directory(
        &self,
        snapshot: &CopySnapshot,
        target: &Path,
        boundary: impl FnMut(&CopyEntry, CopyProgress) -> Result<()>,
    ) -> Result<CopyProgress> {
        self.copy_to_directory_with(snapshot, target, boundary, |_, _| Ok(()))
    }

    pub(crate) fn copy_to_directory_with(
        &self,
        snapshot: &CopySnapshot,
        target: &Path,
        mut boundary: impl FnMut(&CopyEntry, CopyProgress) -> Result<()>,
        mut finalize_file: impl FnMut(&File, &File) -> std::io::Result<()>,
    ) -> Result<CopyProgress> {
        let source = self.root.resolve(None)?.path;
        let target = fs::canonicalize(target).map_err(io_error)?;
        if source.starts_with(&target) || target.starts_with(&source) {
            return Err(FileError::InvalidRoot);
        }
        let mut progress = CopyProgress::default();
        let mut staged = snapshot.clone();
        let mut batch = Vec::with_capacity(SYNC_BATCH_FILES);
        let mut buffer = [0u8; 65_536];
        for (entry, verified) in snapshot.entries.iter().zip(&mut staged.entries) {
            boundary(entry, progress)?;
            let relative = RelativePath::parse(&entry.path)?;
            let mut path = target.clone();
            path.extend(relative.as_str().split('/'));
            let existing = match fs::symlink_metadata(&path) {
                Ok(meta) => {
                    super::root::plain(&meta)?;
                    if meta.is_dir() != entry.directory {
                        return Err(FileError::Corrupt);
                    }
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(io_error(error)),
            };
            if entry.directory {
                if !existing {
                    fs::create_dir(&path).map_err(write_error)?;
                }
                continue;
            }
            let mut completed = None;
            if existing {
                let mut file = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .map_err(io_error)?;
                if native::actual_path(&file, &path)? != path {
                    return Err(FileError::LinkedPath);
                }
                let version = native::file_version(&file).ok();
                if file.metadata().map_err(io_error)?.len() == entry.size
                    && digest_observed(&mut file, entry.size, version.as_ref())? == entry.sha256
                {
                    completed = Some((file, version));
                } else {
                    drop(file);
                    fs::remove_file(&path).map_err(io_error)?;
                }
            }
            let (file, version) = match completed {
                Some(completed) => completed,
                None => {
                    let mut reader = self.file(entry)?;
                    let mut writer = File::create_new(&path).map_err(write_error)?;
                    copy_verified(&mut reader, &mut writer, entry, &mut buffer)?;
                    finalize_file(&reader, &writer).map_err(write_error)?;
                    let version = native::file_version(&writer).ok();
                    (writer, version)
                }
            };
            verified.identity = native::file_identity(&file)?;
            verified.version = reusable_key(version);
            batch.push(file);
            if batch.len() == SYNC_BATCH_FILES {
                sync_batch(&mut batch)?;
            }
            progress.files += 1;
            progress.bytes += entry.size;
        }
        sync_batch(&mut batch)?;
        self.validate_snapshot(snapshot, |_| {})?;
        Self::open(&target, &[])?
            .with_limits(self.limits)
            .validate_snapshot(&staged, |_| {})?;
        for entry in snapshot
            .entries
            .iter()
            .rev()
            .filter(|entry| entry.directory)
        {
            native::sync_directory(&target.join(&entry.path))?;
        }
        native::sync_directory(&target)?;
        Ok(progress)
    }
}

fn sync_batch(batch: &mut Vec<File>) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    std::thread::scope(|scope| -> Result<()> {
        let workers = batch
            .chunks(SYNC_BATCH_FILES / SYNC_WORKERS)
            .map(|files| scope.spawn(|| files.iter().try_for_each(File::sync_all)))
            .collect::<Vec<_>>();
        for worker in workers {
            worker
                .join()
                .map_err(|_| FileError::Unavailable)?
                .map_err(write_error)?;
        }
        Ok(())
    })?;
    batch.clear();
    Ok(())
}

fn copy_verified(
    reader: &mut File,
    writer: &mut File,
    entry: &CopyEntry,
    buffer: &mut [u8],
) -> Result<()> {
    let mut hash = Sha256::new();
    let mut written = 0u64;
    loop {
        let n = reader.read(buffer).map_err(io_error)?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > entry.size {
            return Err(FileError::Corrupt);
        }
        hash.update(&buffer[..n]);
        writer.write_all(&buffer[..n]).map_err(write_error)?;
    }
    if written != entry.size || hex::encode(hash.finalize()) != entry.sha256 {
        return Err(FileError::Corrupt);
    }
    Ok(())
}
fn write_error(error: std::io::Error) -> FileError {
    if matches!(
        error.kind(),
        std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded
    ) {
        FileError::InsufficientSpace
    } else {
        io_error(error)
    }
}
fn digest(file: &mut File, size: u64) -> Result<String> {
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let mut buffer = [0u8; 65536];
    let mut bytes = 0u64;
    let mut hash = Sha256::new();
    loop {
        let n = file.read(&mut buffer).map_err(io_error)?;
        if n == 0 {
            break;
        }
        bytes = bytes.checked_add(n as u64).ok_or(FileError::TooLarge)?;
        if bytes > size {
            return Err(FileError::Corrupt);
        }
        hash.update(&buffer[..n]);
    }
    if bytes != size {
        return Err(FileError::Corrupt);
    }
    Ok(hex::encode(hash.finalize()))
}

fn reusable_key(version: Option<native::FileVersion>) -> String {
    version
        .filter(|version| version.reusable)
        .map_or_else(String::new, |version| version.key)
}

fn digest_observed(
    file: &mut File,
    size: u64,
    version: Option<&native::FileVersion>,
) -> Result<String> {
    let (hash, unchanged) = if let Some(version) = version {
        let hash = digest(file, size)?;
        (hash, native::file_version(file)?.key == version.key)
    } else {
        let before = file
            .metadata()
            .map_err(io_error)?
            .modified()
            .map_err(io_error)?;
        let hash = digest(file, size)?;
        let after = file.metadata().map_err(io_error)?;
        (
            hash,
            after.len() == size && after.modified().map_err(io_error)? == before,
        )
    };
    if !unchanged {
        return Err(FileError::Corrupt);
    }
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn snapshot_validation_detects_a_same_size_rewrite_with_restored_mtime() {
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("world.mca");
        fs::write(&path, b"reviewed").unwrap();
        let export = CopiedExport::open(source.path(), &[]).unwrap();
        let reviewed = export.snapshot().unwrap();
        export.validate_snapshot(&reviewed, |_| {}).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, b"modified").unwrap();
        File::open(&path).unwrap().set_modified(modified).unwrap();
        assert!(matches!(
            export.validate_snapshot(&reviewed, |_| {}),
            Err(FileError::Corrupt)
        ));
    }

    #[test]
    fn interrupted_batch_reuses_verified_files_and_repairs_torn_writes() {
        let source = tempfile::tempdir().unwrap();
        let staged = tempfile::tempdir().unwrap();
        for index in 0..SYNC_BATCH_FILES + 8 {
            fs::write(source.path().join(format!("{index:03}.yml")), b"reviewed").unwrap();
        }
        let export = CopiedExport::open(source.path(), &[]).unwrap();
        let reviewed = export.snapshot().unwrap();
        let crash = std::panic::catch_unwind(|| {
            export.copy_to_directory(&reviewed, staged.path(), |_, progress| {
                assert_ne!(
                    progress.files,
                    SYNC_BATCH_FILES as u64 + 2,
                    "simulated crash"
                );
                Ok(())
            })
        });
        assert!(crash.is_err());
        let kept = staged.path().join("000.yml");
        let identity = native::file_identity(&File::open(&kept).unwrap()).unwrap();
        fs::write(
            staged.path().join(format!("{SYNC_BATCH_FILES:03}.yml")),
            b"torn",
        )
        .unwrap();
        fs::write(
            staged
                .path()
                .join(format!("{:03}.yml", SYNC_BATCH_FILES + 1)),
            b"modified",
        )
        .unwrap();
        export
            .copy_to_directory(&reviewed, staged.path(), |_, _| Ok(()))
            .unwrap();
        assert_eq!(
            native::file_identity(&File::open(&kept).unwrap()).unwrap(),
            identity
        );
        assert!(
            CopiedExport::open(staged.path(), &[])
                .unwrap()
                .snapshot()
                .unwrap()
                .same_content(&reviewed)
        );
    }
}
