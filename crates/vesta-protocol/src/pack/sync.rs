//! Pure, deterministic reconciliation proposals for a validated custom catalogue.
//!
//! The caller supplies an inventory, not a trusted filesystem observation. This
//! module never reads, hashes, downloads, installs, removes, or authorizes anything.
//! In particular `Remove` only means absent from the desired inventory; it is not
//! permission to delete a local file. Host adapters must establish inventory scope,
//! ownership, native confinement and concurrency preconditions independently.
//!
//! Version labels are compared exactly. A known installed revision ahead of target
//! is rejected (no implicit rollback). A missing revision is explicitly unverified,
//! and a revision below minimum is not treated as compatible. Both still yield a
//! complete proposal, requiring update even when all observed hashes match. Behind
//! revisions at/above minimum are compatible but require revision advancement.
//! A current revision never suppresses actual hash drift. Plans retain `Keep`
//! entries even when no update is needed; transport response shaping is separate.

use super::catalogue::{
    self, BoundedFiles, CatalogueError, MAX_FILES, MAX_TEXT_BYTES, Manifest, PackContent, PackFile,
    RelativePath, SCHEMA_VERSION, Sha256,
};
use serde::Deserialize;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledFile {
    path: RelativePath,
    sha256: Sha256,
}

impl InstalledFile {
    pub fn new(path: &str, sha256: &str) -> Result<Self, CatalogueError> {
        Ok(Self {
            path: RelativePath::parse(path)?,
            sha256: Sha256::parse(sha256)?,
        })
    }

    pub fn path(&self) -> &RelativePath {
        &self.path
    }

    pub fn sha256(&self) -> &Sha256 {
        &self.sha256
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct InstalledState {
    version: Option<String>,
    revision: Option<u64>,
    files: Vec<InstalledFile>,
}

impl InstalledState {
    /// Missing version/revision remain unknown, never guessed from matching hashes.
    /// Installed revision zero is valid descriptive state, but below the positive
    /// minimum of every valid target manifest; it is not a valid target revision.
    /// An empty file vector is explicit absence; invalid/duplicate entries fail,
    /// rather than being skipped or overwritten as in a permissive string map.
    pub fn new(
        version: Option<&str>,
        revision: Option<u64>,
        mut files: Vec<InstalledFile>,
    ) -> Result<Self, CatalogueError> {
        if files.len() > MAX_FILES {
            return Err(CatalogueError::TooManyFiles);
        }
        if let Some(revision) = revision {
            catalogue::validate_revision(revision)?;
        }
        let version = version
            .map(|value| {
                catalogue::validated_text(value, MAX_TEXT_BYTES, CatalogueError::InvalidText)
            })
            .transpose()?;
        let mut text_bytes = version.as_ref().map_or(0, String::len);
        for file in &files {
            catalogue::add_text_budget(&mut text_bytes, file.path.as_str().len() + 64)?;
        }
        catalogue::validate_namespace(files.iter().map(InstalledFile::path), false)?;
        files.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(Self {
            version,
            revision,
            files,
        })
    }

    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    pub fn revision(&self) -> Option<u64> {
        self.revision
    }

    pub fn files(&self) -> &[InstalledFile] {
        &self.files
    }

    /// Strict separate inventory schema: `schemaVersion: 1`, optional `version`
    /// and `revision`, and required `files: [{path, sha256}]`. Not a wire map.
    pub fn decode(input: &[u8]) -> Result<Self, CatalogueError> {
        let wire: WireInstalledState = catalogue::decode_json(input)?;
        if wire.schema_version != SCHEMA_VERSION {
            return Err(CatalogueError::UnsupportedSchema);
        }
        let files = wire
            .files
            .0
            .into_iter()
            .map(|file| InstalledFile::new(&file.path, &file.sha256))
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(wire.version.as_deref(), wire.revision, files)
    }
}

impl fmt::Debug for InstalledState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstalledState")
            .field("revision", &self.revision)
            .field("file_count", &self.files.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncError {
    UnsupportedContent,
    InstalledRevisionAhead,
    PathCollision,
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnsupportedContent => "Pack sync planning failed: unsupported_content",
            Self::InstalledRevisionAhead => "Pack sync planning failed: installed_revision_ahead",
            Self::PathCollision => "Pack sync planning failed: path_collision",
        })
    }
}

impl std::error::Error for SyncError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncOperation {
    Add,
    Update,
    Keep,
    Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncAction {
    operation: SyncOperation,
    path: RelativePath,
    installed_sha256: Option<Sha256>,
    desired_file: Option<PackFile>,
}

impl SyncAction {
    pub fn operation(&self) -> SyncOperation {
        self.operation
    }

    pub fn path(&self) -> &RelativePath {
        &self.path
    }

    /// Inventory precondition only; this digest is not proof of current disk state.
    pub fn installed_sha256(&self) -> Option<&Sha256> {
        self.installed_sha256.as_ref()
    }

    /// Present for add/update/keep; absent for removal. Metadata remains inert.
    pub fn desired_file(&self) -> Option<&PackFile> {
        self.desired_file.as_ref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevisionStatus {
    Unverified,
    BelowMinimum,
    Behind,
    Current,
}

#[derive(Clone, Eq, PartialEq)]
pub struct SyncPlan {
    target_version: String,
    target_revision: u64,
    minimum_revision: u64,
    revision_status: RevisionStatus,
    version_matches: bool,
    file_changes: bool,
    actions: Vec<SyncAction>,
}

impl SyncPlan {
    pub fn target_version(&self) -> &str {
        &self.target_version
    }

    pub fn target_revision(&self) -> u64 {
        self.target_revision
    }

    pub fn minimum_revision(&self) -> u64 {
        self.minimum_revision
    }

    pub fn revision_status(&self) -> RevisionStatus {
        self.revision_status
    }

    /// This describes revision compatibility only, not execution/administrative authority.
    pub fn minimum_revision_satisfied(&self) -> bool {
        matches!(
            self.revision_status,
            RevisionStatus::Behind | RevisionStatus::Current
        )
    }

    pub fn version_matches(&self) -> bool {
        self.version_matches
    }

    pub fn has_file_changes(&self) -> bool {
        self.file_changes
    }

    pub fn needs_update(&self) -> bool {
        !self.version_matches
            || self.file_changes
            || self.revision_status != RevisionStatus::Current
    }

    pub fn actions(&self) -> &[SyncAction] {
        &self.actions
    }
}

impl fmt::Debug for SyncPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncPlan")
            .field("target_revision", &self.target_revision)
            .field("minimum_revision", &self.minimum_revision)
            .field("revision_status", &self.revision_status)
            .field("version_matches", &self.version_matches)
            .field("file_changes", &self.file_changes)
            .field("action_count", &self.actions.len())
            .finish_non_exhaustive()
    }
}

/// Each unique union path produces one action in UTF-8 byte order. This is a
/// comparison, not an executable operation sequence; file/directory transitions
/// and case-only renames are rejected rather than generating order-dependent edits.
pub fn plan(manifest: &Manifest, installed: &InstalledState) -> Result<SyncPlan, SyncError> {
    let PackContent::Custom(desired) = manifest.content() else {
        return Err(SyncError::UnsupportedContent);
    };
    let metadata = manifest.metadata();
    let revision_status = match installed.revision {
        Some(revision) if revision > metadata.revision() => {
            return Err(SyncError::InstalledRevisionAhead);
        }
        Some(revision) if revision < metadata.minimum_revision() => RevisionStatus::BelowMinimum,
        Some(revision) if revision < metadata.revision() => RevisionStatus::Behind,
        Some(_) => RevisionStatus::Current,
        None => RevisionStatus::Unverified,
    };
    catalogue::validate_namespace(
        desired
            .iter()
            .map(PackFile::path)
            .chain(installed.files.iter().map(InstalledFile::path)),
        true,
    )
    .map_err(|_| SyncError::PathCollision)?;

    let mut actions = Vec::with_capacity(desired.len() + installed.files.len());
    let mut desired = desired.iter().peekable();
    let mut observed = installed.files.iter().peekable();
    while desired.peek().is_some() || observed.peek().is_some() {
        match (desired.peek(), observed.peek()) {
            (Some(wanted), Some(current)) if wanted.path() == current.path() => {
                let operation = if wanted.sha256() == current.sha256() {
                    SyncOperation::Keep
                } else {
                    SyncOperation::Update
                };
                actions.push(SyncAction {
                    operation,
                    path: wanted.path().clone(),
                    installed_sha256: Some(*current.sha256()),
                    desired_file: Some((*wanted).clone()),
                });
                desired.next();
                observed.next();
            }
            (Some(wanted), current)
                if current.is_none_or(|current| wanted.path() < current.path()) =>
            {
                actions.push(SyncAction {
                    operation: SyncOperation::Add,
                    path: wanted.path().clone(),
                    installed_sha256: None,
                    desired_file: Some((*wanted).clone()),
                });
                desired.next();
            }
            (_, Some(current)) => {
                actions.push(SyncAction {
                    operation: SyncOperation::Remove,
                    path: current.path().clone(),
                    installed_sha256: Some(*current.sha256()),
                    desired_file: None,
                });
                observed.next();
            }
            _ => break,
        }
    }
    let file_changes = actions
        .iter()
        .any(|action| action.operation != SyncOperation::Keep);
    Ok(SyncPlan {
        target_version: metadata.version().to_owned(),
        target_revision: metadata.revision(),
        minimum_revision: metadata.minimum_revision(),
        revision_status,
        version_matches: installed.version() == Some(metadata.version()),
        file_changes,
        actions,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireInstalledState {
    schema_version: u16,
    version: Option<String>,
    revision: Option<u64>,
    files: BoundedFiles<WireInstalledFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireInstalledFile {
    path: String,
    sha256: String,
}
