//! Backend-only configured whole-pack preparation. No network or extraction.
use super::{
    BuiltVestaPack, PackError, PreparedArchive, PreparedSnapshot, VestaBuildError,
    VestaBuildLimits, build_vesta,
};
use crate::{config::PackMetadata, files::RelativePath};
use sha2::{Digest, Sha256 as DigestSha256};
use vesta_protocol::pack::catalogue::{
    Manifest, ManifestMetadata, PackContent, Platform, PlatformPack, Sha256,
};

/// Immutable explicit preparation; publication still requires current authority.
pub struct PreparedPack {
    pub(super) manifest: Manifest,
    pub(super) content: PreparedContent,
    missing_directories: Vec<RelativePath>,
}
pub(super) enum PreparedContent {
    Scanned(BuiltVestaPack),
    Archive(PreparedArchive),
    Platform { bytes: Box<[u8]>, hash: Sha256 },
}
impl std::fmt::Debug for PreparedPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedPack")
            .field("missing_directory_count", &self.missing_directories.len())
            .finish_non_exhaustive()
    }
}
impl PreparedPack {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn missing_directories(&self) -> &[RelativePath] {
        &self.missing_directories
    }
    pub(crate) fn scanned(
        snapshot: PreparedSnapshot,
        metadata: &PackMetadata,
    ) -> Result<Self, VestaBuildError> {
        let built = build_vesta(&snapshot, metadata, VestaBuildLimits::default())?;
        Ok(Self {
            manifest: snapshot.manifest,
            content: PreparedContent::Scanned(built),
            missing_directories: snapshot.missing_directories,
        })
    }
    pub(crate) fn archive(
        archive: PreparedArchive,
        metadata: ManifestMetadata,
    ) -> Result<Self, PackError> {
        Ok(Self {
            manifest: Manifest::new(
                metadata,
                PackContent::Archive(archive.description().clone()),
            )?,
            content: PreparedContent::Archive(archive),
            missing_directories: Vec::new(),
        })
    }
    pub(crate) fn platform(
        reference: PlatformPack,
        metadata: ManifestMetadata,
    ) -> Result<Self, PackError> {
        let source = reference.source();
        // A native catalogue reference, not a synthetic custom pack or fetched archive.
        let wire = serde_json::json!({
            "schemaVersion": vesta_protocol::pack::catalogue::SCHEMA_VERSION,
            "name": metadata.name(), "version": metadata.version(),
            "revision": metadata.revision(), "minimumRevision": metadata.minimum_revision(),
            "content": { "mode": "platform", "platform": match source.platform_name() {
                Some(Platform::Modrinth) => "modrinth", Some(Platform::Curseforge) => "curseforge", None => return Err(PackError::InvalidPath),
            }, "projectId": source.project_id(), "versionId": source.version_id() }
        });
        if source.url().is_some() {
            return Err(PackError::InvalidPath);
        }
        let bytes = serde_json::to_vec(&wire).map_err(|_| PackError::Unavailable)?;
        // Re-admit the serialized artifact through the strict native wire boundary.
        let manifest = Manifest::decode(&bytes)?;
        let hash = Sha256::parse(&hex::encode(DigestSha256::digest(&bytes)))?;
        Ok(Self {
            manifest,
            content: PreparedContent::Platform {
                bytes: bytes.into_boxed_slice(),
                hash,
            },
            missing_directories: Vec::new(),
        })
    }
}
