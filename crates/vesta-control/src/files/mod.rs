//! Canonically authorized, bounded file observations under a trusted host root.
//!
//! Subjects are trusted lookup keys, not transport authentication. Paths use the
//! same conservative ASCII policy as canonical scopes; no Unicode grant is inferred.
//! Native checks reject links/reparse points and protected names, but are not a
//! handle-relative ancestor walk or same-account race-free confinement claim.
//! Mutations (`mutation`) are recoverable single-file writes/deletes/restores with
//! optimistic concurrency. Pending trash provenance is durably journaled before destructive
//! effects; failed completion is compensated only while the owned effect hash still matches.
//! Unix replacement/restore preserves permission mode bits (not ownership, ACLs, xattrs or
//! timestamps). Windows preserves the read-only attribute and refuses read-only replacement
//! or deletion; it does not copy custom ACLs or other attributes. Stages start private on Unix.
mod local_document;
mod mutation;
mod native;
mod pack_root;
mod path;
mod remote;
mod root;
mod upload;
pub use remote::RemoteFileMutation;

pub(crate) use local_document::read_local_document;
pub use mutation::{
    FileChange, MAX_FILE_INTENTS, MAX_INTENT_STAGED_BYTES, MAX_TRASH_FILE_BYTES, MAX_WRITE_BYTES,
    SPACE_RESERVE_BYTES, TrashEntry, TrashPolicy, WritePrecondition,
};
pub(crate) use pack_root::{PackNode, PackRoot};
pub use path::{MAX_COMPONENT_BYTES, MAX_PATH_BYTES, RelativePath};
pub(crate) use root::same_directory;
pub use upload::{MAX_UPLOAD_CHUNK_BYTES, UploadId, UploadPolicy};

use crate::{
    ControlError,
    identity::{self, Capability, PolicySnapshot, Resource, Subject},
};
use sha2::{Digest, Sha256};
use std::{error::Error, fmt, fs, io::Read, path::Path, time::SystemTime};
use vesta_state::{StateDatabase, StateError, TransactionError};

pub const MAX_READ_BYTES: usize = 1_048_576;
pub const MAX_DIRECTORY_ENTRIES: usize = 1_000;
/// Work bound includes hidden/unreadable entries, not just returned entries.
pub const MAX_DIRECTORY_SCAN: usize = 4_000;

pub type Result<T> = std::result::Result<T, FileError>;

#[derive(Clone, Eq, PartialEq)]
pub enum FileError {
    InvalidPath,
    ProtectedPath,
    LinkedPath,
    InvalidRoot,
    NotFound,
    NotRegular,
    NotDirectory,
    TooLarge,
    InvalidLimit,
    Unavailable,
    /// The precondition no longer holds; carries the current SHA-256 when one exists.
    Conflict(Option<String>),
    TrashFull,
    IntentsFull,
    UploadFull,
    IdempotencyConflict,
    ConfirmationRequired,
    ConfirmationInvalid,
    InsufficientSpace,
    /// A change was applied but neither recorded nor reversed; inspect before retrying.
    RecoveryRequired(Box<FileError>),
    Control(ControlError),
    State(StateError),
    Rollback {
        primary: Box<FileError>,
        failure: StateError,
    },
}
impl FileError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidPath => "file_path_invalid",
            Self::ProtectedPath => "file_path_protected",
            Self::LinkedPath => "file_path_linked",
            Self::InvalidRoot => "file_root_invalid",
            Self::NotFound => "file_not_found",
            Self::NotRegular => "file_not_regular",
            Self::NotDirectory => "file_not_directory",
            Self::TooLarge => "file_too_large",
            Self::InvalidLimit => "file_limit_invalid",
            Self::Unavailable => "file_unavailable",
            Self::Conflict(_) => "file_conflict",
            Self::TrashFull => "file_trash_full",
            Self::IntentsFull => "file_intents_full",
            Self::ConfirmationRequired => "confirmation_required",
            Self::ConfirmationInvalid => "confirmation_invalid",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::UploadFull => "file_upload_full",
            Self::InsufficientSpace => "file_insufficient_space",
            Self::RecoveryRequired(_) => "file_recovery_required",
            Self::Control(error) => error.code(),
            Self::State(error) => error.code(),
            Self::Rollback { .. } => "rollback_failed",
        }
    }
}
impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("File operation failed")
    }
}
impl fmt::Debug for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileError")
            .field("code", &self.code())
            .finish()
    }
}
impl Error for FileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Control(error) => Some(error),
            Self::State(error) => Some(error),
            Self::Rollback { primary, .. } | Self::RecoveryRequired(primary) => {
                Some(primary.as_ref())
            }
            _ => None,
        }
    }
}
impl From<ControlError> for FileError {
    fn from(error: ControlError) -> Self {
        Self::Control(error)
    }
}
impl From<StateError> for FileError {
    fn from(error: StateError) -> Self {
        Self::State(error)
    }
}
impl From<rusqlite::Error> for FileError {
    fn from(error: rusqlite::Error) -> Self {
        Self::State(error.into())
    }
}
impl From<TransactionError<FileError>> for FileError {
    fn from(error: TransactionError<FileError>) -> Self {
        match error {
            TransactionError::State(error) => Self::State(error),
            TransactionError::Work(error) => error,
            TransactionError::Rollback { primary, failure } => Self::Rollback {
                primary: Box::new(Self::from(*primary)),
                failure,
            },
        }
    }
}
pub(super) fn io_error(error: std::io::Error) -> FileError {
    if error.kind() == std::io::ErrorKind::NotFound {
        FileError::NotFound
    } else {
        FileError::Unavailable
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    File,
    Directory,
}

/// Only relative portable names and metadata, never a native path or file contents.
#[derive(Clone, Eq, PartialEq)]
pub struct FileEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub modified_at: Option<SystemTime>,
}
impl fmt::Debug for FileEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileEntry")
            .field("kind", &self.kind)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryListing {
    pub entries: Vec<FileEntry>,
}

/// Owned bytes and their actual digest. This is not a pin of future on-disk bytes.
pub struct FileRead {
    bytes: Vec<u8>,
    sha256: String,
    modified_at: Option<SystemTime>,
}
impl FileRead {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn modified_at(&self) -> Option<SystemTime> {
        self.modified_at
    }
}
impl fmt::Debug for FileRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileRead")
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct FileBrowser {
    db: StateDatabase,
    root: root::Root,
    audit: vesta_state::audit::AuditLog,
    trash_policy: TrashPolicy,
    upload_policy: UploadPolicy,
    remote_key: Option<String>,
    properties_write: bool,
    content_write: bool,
    remote_deadline: Option<std::time::Instant>,
    properties_approval: Option<crate::operations::PropertiesApproval>,
}
impl fmt::Debug for FileBrowser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FileBrowser[<redacted>]")
    }
}
impl FileBrowser {
    /// Trusted host composition only. This opens no listener and grants no authority.
    /// The canonical state root is always excluded even if the host omits it.
    pub fn open(db: StateDatabase, root: &Path, protected: &[&Path]) -> Result<Self> {
        let mut protected = protected.to_vec();
        protected.push(db.directory().path());
        let root = root::Root::open(root, &protected)?;
        Ok(Self {
            audit: vesta_state::audit::AuditLog::new(db.clone())?,
            db,
            root,
            trash_policy: TrashPolicy::default(),
            upload_policy: UploadPolicy::default(),
            remote_key: None,
            properties_write: false,
            content_write: false,
            remote_deadline: None,
            properties_approval: None,
        })
    }

    /// Refuse provider work unless a current subject can write an eligible mod
    /// leaf. The unknown provider filename does not justify unscoped authority.
    pub(crate) fn check_content_admission(
        &self,
        subject: &Subject,
        precondition: &WritePrecondition,
    ) -> Result<()> {
        let capability = match precondition {
            WritePrecondition::Absent => Capability::FilesCreate,
            WritePrecondition::Matches(_) => Capability::FilesOverwrite,
        };
        let directory = RelativePath::parse("mods")?;
        self.db
            .read_with(|snapshot| {
                let policy = identity::policy_in(
                    snapshot.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                    Some(subject),
                )?;
                let check = |resource: &Resource| match policy
                    .check_any_direct_child(capability, resource, ".jar")
                {
                    identity::Decision::Allow => Ok(()),
                    identity::Decision::Deny(reason) => {
                        Err(FileError::from(ControlError::forbidden(reason)))
                    }
                };
                check(&directory.resource())?;
                let checked = self.root.resolve(Some(&directory))?;
                if !checked.metadata.is_dir() {
                    return Err(FileError::NotDirectory);
                }
                check(&checked.resource)
            })
            .map_err(FileError::from)
    }

    /// Early download admission only; `write` rechecks authority in its completion
    /// transaction. This grants no durable authority across a network fetch.
    pub(crate) fn check_write_authority(
        &self,
        subject: &Subject,
        path: &RelativePath,
        precondition: &WritePrecondition,
    ) -> Result<()> {
        let capability = match precondition {
            WritePrecondition::Absent => Capability::FilesCreate,
            WritePrecondition::Matches(_) => Capability::FilesOverwrite,
        };
        self.authorized_parent(subject, path, capability)
            .map(|_| ())
    }

    pub fn with_trash_policy(mut self, policy: TrashPolicy) -> Self {
        self.trash_policy = policy;
        self
    }

    /// `None` explicitly selects the root. Returned and scanned work are bounded.
    /// Every returned child must itself pass current `files.list` scope policy.
    pub fn list(
        &self,
        subject: &Subject,
        directory: Option<&RelativePath>,
    ) -> Result<DirectoryListing> {
        self.db
            .read_with(|snapshot| {
                let policy = identity::policy_in(
                    snapshot.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                    Some(subject),
                )?;
                let resource = directory
                    .map(RelativePath::resource)
                    .unwrap_or(Resource::ROOT);
                authorize(&policy, Capability::FilesList, &resource)?;
                let checked = self.root.resolve(directory)?;
                authorize(&policy, Capability::FilesList, &checked.resource)?;
                if !checked.metadata.is_dir() {
                    return Err(FileError::NotDirectory);
                }
                let mut entries = Vec::new();
                let stream = fs::read_dir(&checked.path).map_err(io_error)?;
                for (scanned, entry) in stream.enumerate() {
                    if scanned >= MAX_DIRECTORY_SCAN {
                        return Err(FileError::TooLarge);
                    }
                    let entry = entry.map_err(io_error)?;
                    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                        continue;
                    };
                    let text = if checked.resource.is_root() {
                        name.clone()
                    } else {
                        format!("{}/{}", checked.resource.as_str(), name)
                    };
                    let Ok(relative) = RelativePath::parse(&text) else {
                        continue;
                    };
                    if !policy
                        .check(Capability::FilesList, Some(&relative.resource()))
                        .allowed()
                    {
                        continue;
                    }
                    let child = match self.root.resolve(Some(&relative)) {
                        Ok(child) => child,
                        Err(
                            FileError::NotFound
                            | FileError::LinkedPath
                            | FileError::ProtectedPath
                            | FileError::InvalidPath
                            | FileError::NotRegular,
                        ) => continue,
                        Err(error) => return Err(error),
                    };
                    if !policy
                        .check(Capability::FilesList, Some(&child.resource))
                        .allowed()
                    {
                        continue;
                    }
                    let kind = if child.metadata.is_dir() {
                        EntryKind::Directory
                    } else if child.metadata.is_file() {
                        EntryKind::File
                    } else {
                        continue;
                    };
                    if kind == EntryKind::File {
                        // File metadata alone cannot identify hard-link aliases on Windows.
                        // Inspect the actual leaf handle, without returning its contents.
                        let Ok(file) = native::open_regular(&child.path) else {
                            continue;
                        };
                        let Ok(actual) = native::actual_path(&file, &child.path) else {
                            continue;
                        };
                        let Ok(actual_resource) = self.root.resource(&actual) else {
                            continue;
                        };
                        if actual_resource != child.resource {
                            continue;
                        }
                    }
                    if entries.len() == MAX_DIRECTORY_ENTRIES {
                        return Err(FileError::TooLarge);
                    }
                    let actual_name = child
                        .resource
                        .as_str()
                        .rsplit('/')
                        .next()
                        .ok_or(FileError::InvalidPath)?
                        .to_owned();
                    entries.push(FileEntry {
                        name: actual_name,
                        kind,
                        size: if kind == EntryKind::File {
                            child.metadata.len()
                        } else {
                            0
                        },
                        modified_at: child.metadata.modified().ok(),
                    });
                }
                entries.sort_by(|a, b| {
                    a.name
                        .to_ascii_lowercase()
                        .cmp(&b.name.to_ascii_lowercase())
                        .then_with(|| a.name.cmp(&b.name))
                });
                Ok(DirectoryListing { entries })
            })
            .map_err(FileError::from)
    }

    /// A fresh snapshot covers path scope checks and a bounded read of the opened
    /// regular file. Returning bytes does not create a renewable read grant.
    pub fn read(&self, subject: &Subject, path: &RelativePath, limit: usize) -> Result<FileRead> {
        self.read_with_capability(subject, path, limit, None)
    }

    pub(crate) fn read_with_capability(
        &self,
        subject: &Subject,
        path: &RelativePath,
        limit: usize,
        extra: Option<Capability>,
    ) -> Result<FileRead> {
        if limit == 0 || limit > MAX_READ_BYTES {
            return Err(FileError::InvalidLimit);
        }
        self.db
            .read_with(|snapshot| {
                let policy = identity::policy_in(
                    snapshot.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                    Some(subject),
                )?;
                if let Some(capability) = extra
                    && !policy.check(capability, None).allowed()
                {
                    return Err(ControlError::forbidden(identity::DenyReason::NotGranted).into());
                }
                authorize(&policy, Capability::FilesRead, &path.resource())?;
                let checked = self.root.resolve(Some(path))?;
                authorize(&policy, Capability::FilesRead, &checked.resource)?;
                if !checked.metadata.is_file() {
                    return Err(FileError::NotRegular);
                }
                let file = native::open_regular(&checked.path)?;
                let actual = native::actual_path(&file, &checked.path)?;
                let resource = self.root.resource(&actual)?;
                authorize(&policy, Capability::FilesRead, &resource)?;
                let metadata = file.metadata().map_err(io_error)?;
                if metadata.len() > limit as u64 {
                    return Err(FileError::TooLarge);
                }
                let mut bytes = Vec::new();
                file.take(limit as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(io_error)?;
                if bytes.len() > limit {
                    return Err(FileError::TooLarge);
                }
                let sha256 = hex::encode(Sha256::digest(&bytes));
                Ok(FileRead {
                    bytes,
                    sha256,
                    modified_at: metadata.modified().ok(),
                })
            })
            .map_err(FileError::from)
    }
}
fn authorize(policy: &PolicySnapshot, capability: Capability, resource: &Resource) -> Result<()> {
    match policy.check(capability, Some(resource)) {
        identity::Decision::Allow => Ok(()),
        identity::Decision::Deny(reason) => Err(ControlError::forbidden(reason).into()),
    }
}

#[cfg(test)]
mod tests;
