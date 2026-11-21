//! Read-only copied-export admission and copying through the existing per-file
//! mutation journal. Source roots are selected by local composition, not HTTP.
use super::{FileBrowser, FileError, RelativePath, Result, io_error, native, root::Root};
use crate::identity::Subject;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    fs::File,
    io::{Read, Seek, SeekFrom},
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
        let file = native::open_regular(&checked.path)?;
        if self
            .root
            .resource(&native::actual_path(&file, &checked.path)?)?
            != checked.resource
        {
            return Err(FileError::LinkedPath);
        }
        if file.metadata().map_err(io_error)?.len() != entry.size
            || (!entry.identity.is_empty() && native::file_identity(&file)? != entry.identity)
        {
            return Err(FileError::Corrupt);
        }
        Ok(file)
    }
    pub(crate) fn snapshot(&self) -> Result<CopySnapshot> {
        self.snapshot_observed(|_| {})
    }

    /// Keep original activity timestamps in a private local staging copy.
    #[cfg(target_os = "linux")]
    pub(crate) fn preserve_modified_times(
        &self,
        snapshot: &CopySnapshot,
        target: &Path,
    ) -> Result<()> {
        for entry in snapshot.entries.iter().filter(|entry| !entry.directory) {
            let modified = self.file(entry)?.metadata().map_err(io_error)?.modified().map_err(io_error)?;
            let path = RelativePath::parse(&entry.path)?;
            File::open(target.join(path.as_str())).map_err(io_error)?
                .set_modified(modified).map_err(io_error)?;
        }
        Ok(())
    }
    /// Snapshot with running totals reported after every hashed file.
    pub(crate) fn snapshot_observed(
        &self,
        mut observe: impl FnMut(CopyProgress),
    ) -> Result<CopySnapshot> {
        let mut progress = CopyProgress::default();
        let mut entries = Vec::new();
        let mut queue = vec![String::new()];
        let mut bytes = 0u64;
        while let Some(directory) = queue.pop() {
            let relative = if directory.is_empty() {
                None
            } else {
                Some(RelativePath::parse(&directory)?)
            };
            let checked = self.root.resolve(relative.as_ref())?;
            for item in fs::read_dir(&checked.path).map_err(io_error)? {
                if entries.len() >= self.limits.entries {
                    return Err(FileError::TooLarge);
                }
                let item = item.map_err(io_error)?;
                let name = item
                    .file_name()
                    .into_string()
                    .map_err(|_| FileError::InvalidPath)?;
                if [
                    "vesta-state.db",
                    "setup.json",
                    "host.toml",
                    "key.pem",
                    "certificate.pem",
                ]
                .iter()
                .any(|n| name.eq_ignore_ascii_case(n))
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
                if checked.metadata.is_dir() {
                    queue.push(path.clone());
                    entries.push(CopyEntry {
                        path,
                        directory: true,
                        size: 0,
                        sha256: String::new(),
                        identity: String::new(),
                    });
                } else if checked.metadata.is_file() {
                    // Admission binds the opened regular file. Never reopen a pathname
                    // between native identity validation and its streamed digest.
                    let mut entry = CopyEntry {
                        path,
                        directory: false,
                        size: checked.metadata.len(),
                        sha256: String::new(),
                        identity: String::new(),
                    };
                    if entry.size > self.limits.file_bytes {
                        return Err(FileError::TooLarge);
                    }
                    bytes = bytes.checked_add(entry.size).ok_or(FileError::TooLarge)?;
                    if bytes > self.limits.total_bytes {
                        return Err(FileError::TooLarge);
                    }
                    let mut file = self.file(&entry)?;
                    entry.identity = native::file_identity(&file)?;
                    entry.sha256 = digest(&mut file, entry.size)?;
                    progress.files += 1;
                    progress.bytes += entry.size;
                    observe(progress);
                    entries.push(entry);
                } else {
                    return Err(FileError::NotRegular);
                }
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let encoded = serde_json::to_vec(&entries).map_err(|_| FileError::Corrupt)?;
        Ok(CopySnapshot {
            entries,
            bytes,
            sha256: hex::encode(Sha256::digest(encoded)),
        })
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
        if self.snapshot()?.sha256 != snapshot.sha256 {
            return Err(FileError::Corrupt);
        }
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
        if self.snapshot()?.sha256 != snapshot.sha256 {
            return Err(FileError::Corrupt);
        }
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
impl CopiedExport {
    /// Resumable read-only copy into a fresh, host-managed, unregistered staging
    /// directory (host-scope imports). No per-server journal exists yet, so each
    /// file is written to a sibling part file, verified against the reviewed
    /// digest, synced and renamed. Already-present files are kept only when their
    /// digest matches, so a crash resumes without recopying verified content. The
    /// source is reopened through the admitted root with identity checks and is
    /// never written, locked or deleted. `boundary` runs before every entry.
    pub(crate) fn copy_to_directory(
        &self,
        snapshot: &CopySnapshot,
        target: &Path,
        mut boundary: impl FnMut(&CopyEntry, CopyProgress) -> Result<()>,
    ) -> Result<CopyProgress> {
        let source = self.root.resolve(None)?.path;
        let target = fs::canonicalize(target).map_err(io_error)?;
        if source.starts_with(&target) || target.starts_with(&source) {
            return Err(FileError::InvalidRoot);
        }
        let mut progress = CopyProgress::default();
        for entry in &snapshot.entries {
            boundary(entry, progress)?;
            let relative = RelativePath::parse(&entry.path)?;
            let mut path = target.clone();
            path.extend(relative.as_str().split('/'));
            let existing = fs::symlink_metadata(&path);
            if let Ok(meta) = &existing
                && (meta.file_type().is_symlink() || (meta.is_dir() != entry.directory))
            {
                return Err(FileError::Corrupt);
            }
            if entry.directory {
                if existing.is_err() {
                    fs::create_dir(&path).map_err(io_error)?;
                }
                continue;
            }
            if existing.is_ok() {
                let mut file = File::open(&path).map_err(io_error)?;
                if file.metadata().map_err(io_error)?.len() == entry.size
                    && digest(&mut file, entry.size)? == entry.sha256
                {
                    progress.files += 1;
                    progress.bytes += entry.size;
                    continue;
                }
                drop(file);
                fs::remove_file(&path).map_err(io_error)?;
            }
            let leaf = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or(FileError::InvalidPath)?;
            let part = path.with_file_name(format!(".{leaf}{PART_SUFFIX}"));
            let mut reader = self.file(entry)?;
            reader.seek(SeekFrom::Start(0)).map_err(io_error)?;
            let mut writer = File::create(&part).map_err(io_error)?;
            let mut hash = Sha256::new();
            let mut buffer = vec![0u8; 65536];
            let mut written = 0u64;
            loop {
                let n = reader.read(&mut buffer).map_err(io_error)?;
                if n == 0 {
                    break;
                }
                written += n as u64;
                if written > entry.size {
                    return Err(FileError::Corrupt);
                }
                hash.update(&buffer[..n]);
                std::io::Write::write_all(&mut writer, &buffer[..n]).map_err(write_error)?;
            }
            if written != entry.size || hex::encode(hash.finalize()) != entry.sha256 {
                drop(writer);
                let _ = fs::remove_file(&part);
                return Err(FileError::Corrupt);
            }
            writer.sync_all().map_err(write_error)?;
            drop(writer);
            fs::rename(&part, &path).map_err(io_error)?;
            progress.files += 1;
            progress.bytes += entry.size;
        }
        // Membership and content must still match the reviewed source, and the
        // destination must hold exactly the reviewed tree.
        if self.snapshot()?.sha256 != snapshot.sha256 {
            return Err(FileError::Corrupt);
        }
        let copied = Self::open(&target, &[])?
            .with_limits(self.limits)
            .snapshot()?;
        if !copied.same_content(snapshot) {
            return Err(FileError::Corrupt);
        }
        Ok(progress)
    }
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
