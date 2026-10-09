//! Content-addressed publication of already-verified pack bytes into protected state.
//!
//! The name is the artifact's SHA-256 plus a format extension, so a published name can
//! never be rebound to different bytes: an existing name is verified, never replaced,
//! and earlier artifacts are untouched by later publications or failures. Bytes are
//! staged under a unique create-new name, synced, then hard-linked without replacement.
//! Every load re-verifies size and digest; filesystem checks are not race-free confinement.
//!
//! This is local trusted preparation, not a route, session, catalogue selection or grant.
use super::{BuiltVestaPack, PackError, PreparedArchive};
use sha2::{Digest, Sha256 as DigestSha256};
use std::{
    fmt,
    fs::File,
    io::{Read, Write},
};
use vesta_protocol::pack::catalogue::{ArchiveFormat, Sha256};
use vesta_state::{
    StateDatabase, StateError,
    artifacts::{ArtifactDirectory, ArtifactName},
};

/// Bounded directory work. Pruning unreferenced artifacts needs catalogue reference
/// tracking and is deliberately not offered; publication refuses beyond this count.
pub const MAX_ARTIFACT_ENTRIES: usize = 1_024;
/// Matches the largest Vesta archive and the largest pinned opaque archive.
pub const MAX_ARTIFACT_BYTES: usize = super::MAX_VESTA_ARCHIVE_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactFormat {
    Vesta,
    Zip,
    Mrpack,
}
impl ArtifactFormat {
    const fn extension(self) -> &'static str {
        match self {
            Self::Vesta => "vestapack",
            Self::Zip => "zip",
            Self::Mrpack => "mrpack",
        }
    }
}

/// Identity of immutable published bytes. Not a download grant or catalogue revision.
#[derive(Clone, Eq, PartialEq)]
pub struct PublishedArtifact {
    format: ArtifactFormat,
    sha256: Sha256,
    size: u64,
}
impl PublishedArtifact {
    pub fn format(&self) -> ArtifactFormat {
        self.format
    }
    pub fn sha256(&self) -> &Sha256 {
        &self.sha256
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    fn name(&self) -> Result<ArtifactName, PackError> {
        ArtifactName::new(&format!(
            "{}.{}",
            self.sha256.to_hex(),
            self.format.extension()
        ))
        .map_err(state_error)
    }
}
impl fmt::Debug for PublishedArtifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublishedArtifact")
            .field("format", &self.format)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// Owned bytes re-read from disk and verified against the published identity.
pub struct LoadedArtifact {
    artifact: PublishedArtifact,
    bytes: Vec<u8>,
}
impl LoadedArtifact {
    pub fn artifact(&self) -> &PublishedArtifact {
        &self.artifact
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
impl fmt::Debug for LoadedArtifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedArtifact")
            .field("artifact", &self.artifact)
            .finish_non_exhaustive()
    }
}

/// Trusted local composition over the canonical state directory.
pub struct PackArtifacts {
    directory: ArtifactDirectory,
}
impl fmt::Debug for PackArtifacts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PackArtifacts[<redacted>]")
    }
}
impl PackArtifacts {
    pub fn open(db: &StateDatabase) -> Result<Self, PackError> {
        Ok(Self {
            directory: db.directory().artifacts().map_err(state_error)?,
        })
    }

    pub fn publish_vesta(&self, pack: &BuiltVestaPack) -> Result<PublishedArtifact, PackError> {
        self.publish(ArtifactFormat::Vesta, pack.bytes(), pack.sha256())
    }

    /// The opaque archive is stored as pinned; it is never parsed or extracted.
    pub fn publish_archive(
        &self,
        archive: &PreparedArchive,
    ) -> Result<PublishedArtifact, PackError> {
        let format = match archive.description().format() {
            ArchiveFormat::Zip => ArtifactFormat::Zip,
            ArchiveFormat::Mrpack => ArtifactFormat::Mrpack,
        };
        self.publish(format, archive.bytes(), archive.description().sha256())
    }

    fn publish(
        &self,
        format: ArtifactFormat,
        bytes: &[u8],
        sha256: &Sha256,
    ) -> Result<PublishedArtifact, PackError> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(PackError::TooLarge);
        }
        // Defence in depth: never publish bytes under a digest they do not have.
        if &DigestSha256::digest(bytes)[..] != sha256.as_bytes() {
            return Err(PackError::Conflict);
        }
        let artifact = PublishedArtifact {
            format,
            sha256: *sha256,
            size: bytes.len() as u64,
        };
        let name = artifact.name()?;
        if self.directory.open(&name).map_err(state_error)?.is_some() {
            // Identical content is idempotent; anything else stays untouched and refuses.
            self.load(&artifact, MAX_ARTIFACT_BYTES)?;
            return Ok(artifact);
        }
        // Stale stages count, so failed attempts cannot grow the directory without bound.
        match self
            .directory
            .count(MAX_ARTIFACT_ENTRIES - 1)
            .map_err(state_error)?
        {
            Some(_) => {}
            None => return Err(PackError::TooLarge),
        }
        let mut stage = Stage::create(&self.directory)?;
        stage.write(bytes)?;
        fault(Fault::BeforeLink)?;
        match self.directory.link_new(&stage.name, &name) {
            Ok(()) => {}
            Err(error) if error.code() == "state_artifact_exists" => {
                // A concurrent identical publication won; verify rather than trust it.
                drop(stage);
                self.load(&artifact, MAX_ARTIFACT_BYTES)?;
                return Ok(artifact);
            }
            Err(error) => return Err(state_error(error)),
        }
        fault(Fault::AfterLink)?;
        stage.retire()?;
        // The published name must now be a single-link file holding exactly these bytes.
        self.load(&artifact, MAX_ARTIFACT_BYTES)?;
        Ok(artifact)
    }

    /// Re-read and verify the published bytes. Absent names are `NotPublished`; a size or
    /// digest disagreement is `Conflict`, never silently served under that identity.
    pub fn load(
        &self,
        artifact: &PublishedArtifact,
        limit: usize,
    ) -> Result<LoadedArtifact, PackError> {
        if limit == 0 || limit > MAX_ARTIFACT_BYTES {
            return Err(PackError::InvalidLimit);
        }
        if artifact.size > limit as u64 {
            return Err(PackError::TooLarge);
        }
        let file = self
            .directory
            .open(&artifact.name()?)
            .map_err(state_error)?
            .ok_or(PackError::NotPublished)?;
        let bytes = read_exact_bounded(file, artifact.size as usize)?;
        if &DigestSha256::digest(&bytes)[..] != artifact.sha256.as_bytes() {
            return Err(PackError::Conflict);
        }
        Ok(LoadedArtifact {
            artifact: artifact.clone(),
            bytes,
        })
    }
}

fn read_exact_bounded(file: File, expected: usize) -> Result<Vec<u8>, PackError> {
    let length = file.metadata().map_err(|_| PackError::Unavailable)?.len();
    if length != expected as u64 {
        return Err(PackError::Conflict);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected)
        .map_err(|_| PackError::Unavailable)?;
    // One extra byte detects growth after the metadata check.
    file.take(expected as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PackError::Unavailable)?;
    if bytes.len() != expected {
        return Err(PackError::Conflict);
    }
    Ok(bytes)
}

/// A unique staged name, removed on every exit path including after publication.
struct Stage<'a> {
    directory: &'a ArtifactDirectory,
    name: ArtifactName,
    file: Option<File>,
}
impl<'a> Stage<'a> {
    fn create(directory: &'a ArtifactDirectory) -> Result<Self, PackError> {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|_| PackError::Unavailable)?;
        let name = ArtifactName::new(&format!("stage-{}.tmp", hex::encode(random)))
            .map_err(state_error)?;
        let file = directory.create_new(&name).map_err(state_error)?;
        Ok(Self {
            directory,
            name,
            file: Some(file),
        })
    }
    /// Write, sync and close. The handle must be released before the name is linked
    /// or retired (Windows refuses both while it is open without delete sharing).
    fn write(&mut self, bytes: &[u8]) -> Result<(), PackError> {
        let mut file = self.file.take().ok_or(PackError::Unavailable)?;
        file.write_all(bytes).map_err(|_| PackError::Unavailable)?;
        file.sync_all().map_err(|_| PackError::Unavailable)
    }
    fn retire(mut self) -> Result<(), PackError> {
        self.file = None;
        self.directory
            .remove(&self.name)
            .map(|_| ())
            .map_err(state_error)
    }
}
impl Drop for Stage<'_> {
    fn drop(&mut self) {
        self.file = None;
        let _ = self.directory.remove(&self.name);
    }
}

fn state_error(error: StateError) -> PackError {
    match error.code() {
        "state_artifact_linked" | "state_file_linked" | "state_dir_linked" => PackError::LinkedPath,
        "state_artifact_exists" => PackError::Conflict,
        _ => PackError::Unavailable,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Fault {
    BeforeLink,
    AfterLink,
}
#[cfg(not(test))]
fn fault(_: Fault) -> Result<(), PackError> {
    Ok(())
}
#[cfg(test)]
fn fault(point: Fault) -> Result<(), PackError> {
    tests::fault(point)
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;
