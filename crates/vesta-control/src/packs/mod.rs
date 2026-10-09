//! Explicit trusted pack preparation. No listener, session or filesystem mutation.
//! Prepared bytes are immutable artifacts; later on-disk replacement cannot change
//! them under a prior hash. A refresh is bounded, not an atomic tree snapshot.
mod build;
mod publish;
mod releases;
pub use build::{
    BuiltVestaPack, MAX_VESTA_ARCHIVE_BYTES, MAX_VESTA_ENTRIES, MAX_VESTA_INDEX_BYTES,
    VestaBuildError, VestaBuildLimits, build_vesta,
};
pub use publish::{
    ArtifactFormat, LoadedArtifact, MAX_ARTIFACT_BYTES, MAX_ARTIFACT_ENTRIES, PackArtifacts,
    PublishedArtifact,
};
pub use releases::{
    PackRelease, PackReleases, ReleaseError, ReleasePlan, UpdatePolicy, UpdateStatus,
};

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
    InvalidLimit,
    InvalidPath,
    ProtectedPath,
    LinkedPath,
    Unavailable,
    TooLarge,
    InvalidArchive,
    /// A published name or bytes disagree with the expected content identity.
    Conflict,
    NotPublished,
    Catalogue(CatalogueError),
}
impl PackError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidLimit => "pack_limit_invalid",
            Self::InvalidPath => "pack_path_invalid",
            Self::ProtectedPath => "pack_path_protected",
            Self::LinkedPath => "pack_path_linked",
            Self::Unavailable => "pack_unavailable",
            Self::TooLarge => "pack_too_large",
            Self::InvalidArchive => "pack_archive_invalid",
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
        })
    }

    /// Called explicitly by the trusted backend. Never a live-server auto scanner.
    /// Hidden files/directories and protected exclusions never enter the catalogue;
    /// unsafe visible links/names and work overflow refuse the whole preparation.
    pub fn snapshot(
        &self,
        metadata: ManifestMetadata,
        sources: &SourceRegistry,
    ) -> Result<PreparedSnapshot, PackError> {
        let mut remaining = self.limits.scanned;
        let mut pending = vec![None];
        let mut files = Vec::new();
        let mut bodies = BTreeMap::new();
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
                        if files.len() == self.limits.files {
                            return Err(PackError::TooLarge);
                        }
                        let name = CataloguePath::parse(path.as_str())?;
                        let limit = self.limits.file_bytes.min(self.limits.total_bytes - total);
                        let (bytes, digest) = self.root.pin(&path, limit)?;
                        total += bytes.len();
                        let hash = Sha256::parse(&digest)?;
                        let source = sources
                            .source_for(&name, &hash)?
                            .cloned()
                            .unwrap_or_else(FileSource::embedded);
                        files.push(PackFile::new(
                            path.as_str(),
                            &digest,
                            bytes.len() as u64,
                            source,
                        )?);
                        if bodies.insert(name, bytes.into_boxed_slice()).is_some() {
                            return Err(CatalogueError::PathCollision.into());
                        }
                    }
                }
            }
        }
        let manifest = Manifest::new(metadata, PackContent::Custom(files))?;
        Ok(PreparedSnapshot {
            manifest,
            bodies,
            total,
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
        let (bytes, hash) = self.root.pin(path, self.limits.file_bytes)?;
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

/// Preparation is not permission to publish/read. A future pack-only route must
/// enforce current pack policy before exposing this owned immutable snapshot.
pub struct PreparedSnapshot {
    manifest: Manifest,
    bodies: BTreeMap<CataloguePath, Box<[u8]>>,
    total: usize,
}
impl PreparedSnapshot {
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
