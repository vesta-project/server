//! SFTP adapter with one journaled recovery copy per writable handle.
//! Bounded chunks become visible immediately; interrupted uploads remain partial.
use super::*;
use std::{
    io::{Read, Seek, SeekFrom, Write},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

pub(crate) struct SftpWrite {
    pub(crate) path: RelativePath,
    pub(crate) sha256: String,
    capability: Capability,
    prepared: bool,
    identity: String,
    length: u64,
    modified: Option<std::time::SystemTime>,
    version: Arc<AtomicU64>,
    observed_version: u64,
}

impl FileBrowser {
    pub(crate) fn sftp_stat(
        &self,
        subject: &Subject,
        path: Option<&RelativePath>,
    ) -> Result<FileEntry> {
        let files = self.for_remote();
        files
            .db
            .read_with(|tx| {
                let policy = identity::policy_in(
                    tx.connection(),
                    files.db.server_id(),
                    files.db.clock().millis(),
                    Some(subject),
                )?;
                let requested = path.map(RelativePath::resource).unwrap_or(Resource::ROOT);
                authorize(&policy, Capability::FilesList, &requested)?;
                let checked = files.root.resolve(path)?;
                authorize(&policy, Capability::FilesList, &checked.resource)?;
                if checked.metadata.is_file() {
                    let file = native::open_regular(&checked.path)?;
                    let actual = native::actual_path(&file, &checked.path)?;
                    authorize(
                        &policy,
                        Capability::FilesList,
                        &files.root.resource(&actual)?,
                    )?;
                }
                Ok(FileEntry {
                    name: path
                        .map(|p| p.as_str().rsplit('/').next().unwrap_or(""))
                        .unwrap_or("/")
                        .into(),
                    kind: if checked.metadata.is_dir() {
                        EntryKind::Directory
                    } else {
                        EntryKind::File
                    },
                    size: checked.metadata.len(),
                    modified_at: checked.metadata.modified().ok(),
                })
            })
            .map_err(FileError::from)
    }

    // Internal content observation requires the action being performed, never an
    // implicit files.read grant. The bytes never leave the write implementation.
    fn sftp_snapshot(
        &self,
        subject: &Subject,
        path: &RelativePath,
        cap: Capability,
        max: u64,
    ) -> Result<(Vec<u8>, String)> {
        self.db
            .read_with(|tx| {
                self.authorize(tx.connection(), subject, path, cap)?;
                let checked = self.root.resolve(Some(path))?;
                let file = native::open_regular(&checked.path)?;
                let actual = native::actual_path(&file, &checked.path)?;
                let policy = identity::policy_in(
                    tx.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                    Some(subject),
                )?;
                authorize(&policy, cap, &self.root.resource(&actual)?)?;
                if file.metadata().map_err(io_error)?.len() > max {
                    return Err(FileError::TooLarge);
                }
                let mut bytes = Vec::new();
                file.take(max + 1)
                    .read_to_end(&mut bytes)
                    .map_err(io_error)?;
                if bytes.len() as u64 > max {
                    return Err(FileError::TooLarge);
                }
                let hash = hex::encode(Sha256::digest(&bytes));
                Ok((bytes, hash))
            })
            .map_err(FileError::from)
    }

    pub(crate) fn sftp_open_write(
        &self,
        subject: &Subject,
        path: RelativePath,
        create: bool,
        truncate: bool,
        exclusive: bool,
        max: u64,
    ) -> Result<SftpWrite> {
        let files = self.for_remote();
        let admission = mutation_admission(files.db.directory().path())?;
        let _guard = files.mutation_guard(&admission)?;
        let (capability, hash, prepared) =
            match files.sftp_snapshot(subject, &path, Capability::FilesOverwrite, max) {
                Ok(_) if exclusive => return Err(FileError::Conflict(None)),
                Ok((_, hash)) if truncate => {
                    let change = files.write_locked(
                        subject,
                        &path,
                        &WritePrecondition::Matches(hash),
                        &[],
                        None,
                    )?;
                    (Capability::FilesOverwrite, change.sha256, true)
                }
                Ok((_, hash)) => (Capability::FilesOverwrite, hash, false),
                Err(FileError::NotFound | FileError::Control(_)) if create => {
                    let change = files.write_locked(
                        subject,
                        &path,
                        &WritePrecondition::Absent,
                        &[],
                        None,
                    )?;
                    (Capability::FilesCreate, change.sha256, true)
                }
                Err(e) => return Err(e),
            };
        let checked = files.root.resolve(Some(&path))?;
        let file = native::open_regular(&checked.path)?;
        let identity = native::file_identity(&file)?;
        let metadata = file.metadata().map_err(io_error)?;
        let version = write_version(files.db.directory().path(), &identity)?;
        let observed_version = version.load(Ordering::Acquire);
        Ok(SftpWrite {
            path,
            sha256: hash,
            capability,
            prepared,
            identity,
            length: metadata.len(),
            modified: metadata.modified().ok(),
            version,
            observed_version,
        })
    }
    pub(crate) fn sftp_write_chunk(
        &self,
        subject: &Subject,
        handle: &mut SftpWrite,
        offset: u64,
        data: &[u8],
        append: bool,
        max: u64,
    ) -> Result<()> {
        if data.len() > FILE_CHUNK_BYTES {
            return Err(FileError::TooLarge);
        }
        let files = self.for_remote();
        let admission = mutation_admission(files.db.directory().path())?;
        let _guard = files.mutation_guard(&admission)?;
        if handle.version.load(Ordering::Acquire) != handle.observed_version {
            return Err(FileError::Conflict(None));
        }
        if !handle.prepared {
            // Keep the original exactly once. Subsequent chunks do not reread,
            // hash, stage, or copy the growing target into trash.
            let (bytes, hash) =
                files.sftp_snapshot(subject, &handle.path, handle.capability, max)?;
            if hash != handle.sha256 {
                return Err(FileError::Conflict(Some(hash)));
            }
            files.write_locked(
                subject,
                &handle.path,
                &WritePrecondition::Matches(hash),
                &bytes,
                None,
            )?;
            let checked = files.root.resolve(Some(&handle.path))?;
            let file = native::open_regular(&checked.path)?;
            handle.identity = native::file_identity(&file)?;
            handle.length = file.metadata().map_err(io_error)?.len();
            handle.modified = file.metadata().map_err(io_error)?.modified().ok();
            handle.version = write_version(files.db.directory().path(), &handle.identity)?;
            handle.observed_version = handle.version.load(Ordering::Acquire);
            handle.prepared = true;
        }
        let offset = if append { handle.length } else { offset };
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(FileError::TooLarge)?;
        if end > max || end > MAX_WRITE_BYTES as u64 {
            return Err(FileError::TooLarge);
        }
        let checked = files.root.resolve(Some(&handle.path))?;
        files.require_mutation_space(
            checked.path.parent().ok_or(FileError::InvalidPath)?,
            end.saturating_sub(handle.length),
            0,
        )?;
        let mut file = native::open_writable_regular(&checked.path)?;
        let actual = native::actual_path(&file, &checked.path)?;
        let metadata = file.metadata().map_err(io_error)?;
        if native::file_identity(&file)? != handle.identity
            || metadata.len() != handle.length
            || metadata.modified().ok() != handle.modified
        {
            return Err(FileError::Conflict(None));
        }
        let mut applied = false;
        let result = files
            .db
            .write_with(|tx| -> Result<()> {
                let policy = identity::policy_in(
                    tx.connection(),
                    files.db.server_id(),
                    files.db.clock().millis(),
                    Some(subject),
                )?;
                authorize(&policy, handle.capability, &handle.path.resource())?;
                authorize(&policy, handle.capability, &files.root.resource(&actual)?)?;
                let record = vesta_state::audit::AuditRecord::new(
                    vesta_state::audit::AuditActor::new(
                        subject.principal.as_str().into(),
                        subject.device.as_str().into(),
                        "sftp".into(),
                    )?,
                    "files.sftp.write".into(),
                    format!(
                        "file:{}",
                        hex::encode(Sha256::digest(handle.path.as_str().as_bytes()))
                    ),
                    "success".into(),
                    None,
                    [
                        ("offset".into(), Some(offset.to_string())),
                        ("bytes".into(), Some(data.len().to_string())),
                    ],
                )?;
                file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
                applied = true;
                file.write_all(data).map_err(io_error)?;
                files.audit.append(tx.connection(), &record)?;
                Ok(())
            })
            .map_err(FileError::from);
        if applied {
            handle.observed_version = handle.version.fetch_add(1, Ordering::AcqRel) + 1;
        }
        match result {
            Ok(()) => {
                let metadata = file.metadata().map_err(io_error)?;
                handle.length = metadata.len();
                handle.modified = metadata.modified().ok();
                Ok(())
            }
            Err(e) if applied => Err(FileError::RecoveryRequired(Box::new(e))),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn sftp_remove(
        &self,
        subject: &Subject,
        path: &RelativePath,
        max: u64,
    ) -> Result<()> {
        let files = self.for_remote();
        let (_, hash) = files.sftp_snapshot(subject, path, Capability::FilesDelete, max)?;
        files.delete(subject, path, &hash).map(|_| ())
    }

    pub(crate) fn sftp_rename(
        &self,
        subject: &Subject,
        from: &RelativePath,
        to: &RelativePath,
        max: u64,
    ) -> Result<()> {
        let files = self.for_remote();
        let (_, hash) = match files.sftp_snapshot(subject, from, Capability::FilesRename, max) {
            Err(FileError::NotRegular) => return files.rename_directory(subject, from, to),
            other => other?,
        };
        files.rename(subject, from, to, &hash).map(|_| ())
    }
}

// Only active native file identities remain; there is no idle cleanup task.
fn write_version(state: &std::path::Path, identity: &str) -> Result<Arc<AtomicU64>> {
    type Key = (std::path::PathBuf, String);
    static VERSIONS: OnceLock<Mutex<std::collections::HashMap<Key, Weak<AtomicU64>>>> =
        OnceLock::new();
    let mut versions = VERSIONS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .map_err(|_| FileError::Unavailable)?;
    versions.retain(|_, value| value.strong_count() != 0);
    let key = (state.to_owned(), identity.to_owned());
    if let Some(version) = versions.get(&key).and_then(Weak::upgrade) {
        return Ok(version);
    }
    if versions.len() >= 4096 {
        return Err(FileError::IntentsFull);
    }
    let version = Arc::new(AtomicU64::new(0));
    versions.insert(key, Arc::downgrade(&version));
    Ok(version)
}
