//! Explicit trusted pack preparation. No listener, session or filesystem mutation.
//! Prepared bytes are immutable artifacts; later on-disk replacement cannot change
//! them under a prior hash. A refresh is bounded, not an atomic tree snapshot.
mod build;
mod configured;
pub use configured::PreparedPack;
mod delivery;
pub(crate) mod payload;
mod publish;
mod releases;
pub mod workspace;
pub use build::{
    BuiltVestaPack, MAX_VESTA_ARCHIVE_BYTES, MAX_VESTA_ENTRIES, MAX_VESTA_INDEX_BYTES,
    VestaBuildError, VestaBuildLimits, build_vesta,
};
pub use publish::{
    ArtifactFormat, LoadedArtifact, MAX_ARTIFACT_BYTES, MAX_ARTIFACT_ENTRIES, PackArtifacts,
    PruneReport, PublishedArtifact,
};
pub use releases::{
    PackMutation, PackOutcome, PackRelease, PackReleases, ProviderFailure, ProviderFailureDetails,
    ReleaseError, ReleasePlan, UpdatePolicy, UpdateStatus,
};

use crate::config::UnresolvedDisposition;
use crate::files::{FileError, PackNode, PackRoot, RelativePath};
use std::{collections::BTreeMap, fmt, path::Path};
use vesta_protocol::pack::{
    catalogue::{
        ArchiveFormat, ArchivePack, CatalogueError, FileSource, Manifest, ManifestMetadata,
        PackContent, PackFile, RelativePath as CataloguePath, Sha256,
    },
    sources::SourceRegistry,
};
use vesta_state::StateDatabase;

pub const MAX_PINNED_BYTES: usize = 64 * 1_024 * 1_024;
pub const MAX_PINNED_FILE_BYTES: usize = 16 * 1_024 * 1_024;
pub const MAX_SNAPSHOT_FILES: usize = 4_096;
pub const MAX_SNAPSHOT_SCAN: usize = 16_384;
pub const MAX_SOURCE_DOCUMENT_BYTES: usize = 65_536;

pub const MAX_SELECTED_DIRECTORIES: usize = 256;

/// Validated selections within one root namespace. Order does not change naming.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedDirectories(Vec<RelativePath>);
impl SelectedDirectories {
    pub fn new(paths: &[impl AsRef<str>]) -> Result<Self, PackError> {
        if paths.is_empty() || paths.len() > MAX_SELECTED_DIRECTORIES {
            return Err(PackError::InvalidPath);
        }
        let mut selections: Vec<RelativePath> = Vec::new();
        let mut bytes = 0usize;
        for text in paths {
            let path = RelativePath::parse(text.as_ref())?;
            CataloguePath::parse(path.as_str())?;
            bytes += path.as_str().len();
            if bytes > crate::config::MAX_TOTAL_PATH_BYTES {
                return Err(PackError::TooLarge);
            }
            for existing in &selections {
                let a = existing.as_str().to_ascii_lowercase();
                let b = path.as_str().to_ascii_lowercase();
                if a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/")) {
                    return Err(PackError::InvalidPath);
                }
                // Reject directory-case aliases even for disjoint sibling leaves.
                for (left, right) in existing.as_str().split('/').zip(path.as_str().split('/')) {
                    if !left.eq_ignore_ascii_case(right) {
                        break;
                    }
                    if left != right {
                        return Err(PackError::InvalidPath);
                    }
                }
            }
            selections.push(path);
        }
        selections.sort();
        Ok(Self(selections))
    }
    pub fn paths(&self) -> &[RelativePath] {
        &self.0
    }
}

/// Explicit backend-selected registry document. Missing/malformed/linked sources
/// refuse; absence never silently changes a known provider file into embedded bytes.
pub fn load_source_registry(source: &Path) -> Result<SourceRegistry, PackError> {
    let bytes = crate::files::read_local_document(source, MAX_SOURCE_DOCUMENT_BYTES)?;
    Ok(SourceRegistry::decode(&bytes)?)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotLimits {
    file_bytes: usize,
    total_bytes: usize,
    files: usize,
    scanned: usize,
}
impl SnapshotLimits {
    pub fn new(
        file_bytes: usize,
        total_bytes: usize,
        files: usize,
        scanned: usize,
    ) -> Result<Self, PackError> {
        if file_bytes == 0
            || file_bytes > MAX_PINNED_FILE_BYTES
            || total_bytes == 0
            || total_bytes > MAX_PINNED_BYTES
            || file_bytes > total_bytes
            || files == 0
            || files > MAX_SNAPSHOT_FILES
            || scanned == 0
            || scanned > MAX_SNAPSHOT_SCAN
        {
            return Err(PackError::InvalidLimit);
        }
        Ok(Self {
            file_bytes,
            total_bytes,
            files,
            scanned,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackError {
    ProviderNotConfigured,
    ProviderUnavailable,
    InvalidLimit,
    InvalidPath,
    ProtectedPath,
    LinkedPath,
    Unavailable,
    TooLarge,
    InsufficientSpace,
    InvalidArchive,
    UnresolvedFile,
    /// A published name or bytes disagree with the expected content identity.
    Conflict,
    NotPublished,
    Catalogue(CatalogueError),
}
impl PackError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ProviderNotConfigured => "content_provider_not_configured",
            Self::ProviderUnavailable => "content_provider_unavailable",
            Self::InvalidLimit => "pack_limit_invalid",
            Self::InvalidPath => "pack_path_invalid",
            Self::ProtectedPath => "pack_path_protected",
            Self::LinkedPath => "pack_path_linked",
            Self::Unavailable => "pack_unavailable",
            Self::TooLarge => "pack_too_large",
            Self::InsufficientSpace => "pack_storage_low",
            Self::InvalidArchive => "pack_archive_invalid",
            Self::UnresolvedFile => "pack_unresolved_file",
            Self::Conflict => "pack_artifact_conflict",
            Self::NotPublished => "pack_artifact_not_published",
            Self::Catalogue(_) => "pack_catalogue_invalid",
        }
    }
}
impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for PackError {}
impl From<CatalogueError> for PackError {
    fn from(error: CatalogueError) -> Self {
        Self::Catalogue(error)
    }
}
impl From<FileError> for PackError {
    fn from(error: FileError) -> Self {
        match error {
            FileError::InvalidPath | FileError::InvalidRoot => Self::InvalidPath,
            FileError::ProtectedPath => Self::ProtectedPath,
            FileError::LinkedPath => Self::LinkedPath,
            FileError::TooLarge => Self::TooLarge,
            FileError::InsufficientSpace => Self::InsufficientSpace,
            _ => Self::Unavailable,
        }
    }
}

/// Trusted local composition only, like FileBrowser::open; not a remote constructor
/// or an administrative grant. Host secrets are supplied by configuration, and the
/// actual canonical database directory is always excluded even if omitted by host.
pub struct PackPreparation {
    root: PackRoot,
    limits: SnapshotLimits,
    unresolved: UnresolvedDisposition,
    directories: Option<SelectedDirectories>,
}
impl PackPreparation {
    pub fn open(
        db: &StateDatabase,
        root: &Path,
        protected: &[&Path],
        limits: SnapshotLimits,
    ) -> Result<Self, PackError> {
        let mut exclusions = protected.to_vec();
        exclusions.push(db.directory().path());
        Ok(Self {
            root: PackRoot::open(root, &exclusions)?,
            limits,
            unresolved: UnresolvedDisposition::Refuse,
            directories: None,
        })
    }

    /// Trusted local composition only, not a frontend-selectable policy.
    pub fn with_unresolved_disposition(mut self, disposition: UnresolvedDisposition) -> Self {
        self.unresolved = disposition;
        self
    }

    pub fn with_selected_directories(mut self, directories: SelectedDirectories) -> Self {
        self.directories = Some(directories);
        self
    }

    /// Called explicitly by the trusted backend. Never a live-server auto scanner.
    /// Hidden files/directories and protected exclusions never enter the catalogue;
    /// unsafe visible links/names and work overflow refuse the whole preparation.
    pub fn snapshot(
        &self,
        metadata: ManifestMetadata,
        sources: &SourceRegistry,
    ) -> Result<PreparedSnapshot, PackError> {
        self.snapshot_with_disposition(self.unresolved, metadata, sources)
    }

    /// Explicit backend-selected alternative, bounded exactly like configured snapshots.
    pub fn snapshot_with_disposition(
        &self,
        disposition: UnresolvedDisposition,
        metadata: ManifestMetadata,
        sources: &SourceRegistry,
    ) -> Result<PreparedSnapshot, PackError> {
        let mut remaining = self.limits.scanned;
        let mut missing_directories = Vec::new();
        let mut pending = Vec::new();
        if let Some(selections) = &self.directories {
            for path in selections.paths() {
                if remaining == 0 {
                    return Err(PackError::TooLarge);
                }
                remaining -= 1;
                match self.root.selected_directory(path) {
                    Ok(true) => pending.push(Some(path.clone())),
                    Ok(false) => missing_directories.push(path.clone()),
                    Err(FileError::ProtectedPath) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        } else {
            pending.push(None);
        }
        let mut files = Vec::new();
        let mut checked_files = Vec::new();
        let mut private_files = Vec::new();
        let mut bodies = BTreeMap::new();
        let mut observed_bytes = 0;
        let mut total = 0;
        while let Some(directory) = pending.pop() {
            for node in self.root.children(directory.as_ref(), &mut remaining)? {
                match node {
                    PackNode::Directory(path) => {
                        // Validate depth/portable catalogue spelling before descending.
                        CataloguePath::parse(path.as_str())?;
                        pending.push(Some(path));
                    }
                    PackNode::File(path) => {
                        if checked_files.len() == self.limits.files {
                            return Err(PackError::TooLarge);
                        }
                        let name = CataloguePath::parse(path.as_str())?;
                        let limit = self
                            .limits
                            .file_bytes
                            .min(self.limits.total_bytes - observed_bytes);
                        let (bytes, digest) = self.root.pin(&path, limit)?;
                        observed_bytes += bytes.len();
                        let hash = Sha256::parse(&digest)?;
                        // A stale known hash refuses even under ExcludePrivate; it must
                        // never be reclassified by a permissive unresolved fallback.
                        let known = sources.source_for(&name, &hash)?;
                        if known.is_none() && disposition == UnresolvedDisposition::Refuse {
                            return Err(PackError::UnresolvedFile);
                        }
                        let file = PackFile::new(
                            path.as_str(),
                            &digest,
                            bytes.len() as u64,
                            known.cloned().unwrap_or_else(FileSource::embedded),
                        )?;
                        checked_files.push(file.clone());
                        if known.is_none() && disposition == UnresolvedDisposition::ExcludePrivate {
                            private_files.push(PrivateFileEntry {
                                path: name,
                                sha256: hash,
                                size: bytes.len() as u64,
                            });
                            // No retained body and no public catalogue row. The metadata
                            // remains local report data, never private download authority.
                            continue;
                        }
                        total += bytes.len();
                        files.push(file);
                        if bodies.insert(name, bytes.into_boxed_slice()).is_some() {
                            return Err(CatalogueError::PathCollision.into());
                        }
                    }
                }
            }
        }
        // Validate the entire observed namespace, including excluded private rows,
        // so collisions do not depend on which disposition was selected.
        Manifest::new(metadata.clone(), PackContent::Custom(checked_files))?;
        private_files.sort_by(|left, right| left.path.cmp(&right.path));
        let manifest = Manifest::new(metadata, PackContent::Custom(files))?;
        Ok(PreparedSnapshot {
            manifest,
            bodies,
            total,
            private_files,
            missing_directories,
        })
    }

    /// Pin an opaque confined archive. Do not parse/extract its contents, fetch a
    /// provider or reopen the filename to serve bytes under this fingerprint.
    pub fn archive(&self, path: &RelativePath) -> Result<PreparedArchive, PackError> {
        let format = match path.as_str().rsplit_once('.') {
            Some((_, extension)) if extension.eq_ignore_ascii_case("zip") => ArchiveFormat::Zip,
            Some((_, extension)) if extension.eq_ignore_ascii_case("mrpack") => {
                ArchiveFormat::Mrpack
            }
            _ => return Err(PackError::InvalidArchive),
        };
        let (bytes, hash) = self
            .root
            .pin(path, self.limits.file_bytes.min(self.limits.total_bytes))?;
        let description = ArchivePack::new(path.as_str(), &hash, bytes.len() as u64, format, None)?;
        Ok(PreparedArchive {
            description,
            bytes: bytes.into_boxed_slice(),
        })
    }
}
impl fmt::Debug for PackPreparation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PackPreparation[<redacted>]")
    }
}

/// Backend-local excluded-file report. No bytes, URLs, serialization or read grant.
#[derive(Clone, Eq, PartialEq)]
pub struct PrivateFileEntry {
    path: CataloguePath,
    sha256: Sha256,
    size: u64,
}
impl PrivateFileEntry {
    pub fn path(&self) -> &CataloguePath {
        &self.path
    }
    pub fn sha256(&self) -> &Sha256 {
        &self.sha256
    }
    pub fn size(&self) -> u64 {
        self.size
    }
}
impl fmt::Debug for PrivateFileEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrivateFileEntry[<redacted>]")
    }
}

/// Preparation is not permission to publish/read. A pack-only route must enforce
/// current pack policy before exposing this owned immutable public snapshot.
pub struct PreparedSnapshot {
    manifest: Manifest,
    bodies: BTreeMap<CataloguePath, Box<[u8]>>,
    total: usize,
    private_files: Vec<PrivateFileEntry>,
    missing_directories: Vec<RelativePath>,
}
impl PreparedSnapshot {
    /// Backend-local missing-selection report, never download authority.
    pub fn missing_directories(&self) -> &[RelativePath] {
        &self.missing_directories
    }

    pub fn private_files(&self) -> &[PrivateFileEntry] {
        &self.private_files
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn total_bytes(&self) -> usize {
        self.total
    }
    pub fn bytes(&self, path: &CataloguePath) -> Option<&[u8]> {
        self.bodies.get(path).map(Box::as_ref)
    }
}
impl fmt::Debug for PreparedSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedSnapshot")
            .field("files", &self.bodies.len())
            .field("bytes", &self.total)
            .finish_non_exhaustive()
    }
}
pub struct PreparedArchive {
    description: ArchivePack,
    bytes: Box<[u8]>,
}
impl PreparedArchive {
    pub fn description(&self) -> &ArchivePack {
        &self.description
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
impl fmt::Debug for PreparedArchive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedArchive")
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
