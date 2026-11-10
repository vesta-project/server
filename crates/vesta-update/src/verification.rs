//! Fresh, offline observations of staged bytes. Never an activation capability:
//! a future activator must retain/recheck its own opened handles at execution.
use crate::{UpdateStore, metadata, release};
use rusqlite::OptionalExtension;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
};
use vesta_state::{StateDirectory, artifacts::ArtifactName};

#[derive(Debug, Serialize)]
pub struct CandidateObservation {
    pub manifest_sha256: String,
    pub sequence: u64,
    pub version: String,
    pub target: String,
    pub verified_at_ms: i64,
    pub files: usize,
    pub bytes: u64,
    pub activation_supported: bool,
}

/// Constructed only from freshly verified signed proof and held executable
/// handles. Never deserialize this from a caller's verification observation.
pub struct VerifiedPayload {
    pub(crate) observation: CandidateObservation,
    pub(crate) metadata: Option<crate::Metadata>,
    pub(crate) manifest: release::Manifest,
    directory: PathBuf,
    files: Vec<File>,
}
impl VerifiedPayload {
    pub fn manifest_sha256(&self) -> &str {
        &self.observation.manifest_sha256
    }
    pub fn version(&self) -> &str {
        &self.observation.version
    }
    pub fn compatibility(&self) -> Option<&crate::ActivationCompatibility> {
        self.metadata.as_ref().and_then(|m| m.activation.as_ref())
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn command(&mut self, name: &str) -> Result<Command, String> {
        self.recheck()?;
        let index = self
            .manifest
            .files
            .iter()
            .position(|f| f.name == name)
            .ok_or("update_executable_not_in_manifest")?;
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            Ok(Command::new(format!(
                "/proc/{}/fd/{}",
                std::process::id(),
                self.files[index].as_raw_fd()
            )))
        }
        #[cfg(windows)]
        {
            Ok(Command::new(
                self.directory
                    .join("bin")
                    .join(&self.manifest.files[index].name),
            ))
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = index;
            Err("unsupported platform".into())
        }
    }
    pub fn recheck(&mut self) -> Result<(), String> {
        for (file, artifact) in self.files.iter_mut().zip(&self.manifest.files) {
            hash_held(file, artifact)?;
        }
        Ok(())
    }
}
fn hash_held(file: &mut File, artifact: &release::Artifact) -> Result<(), String> {
    if file
        .metadata()
        .map_err(|_| "update_candidate_unavailable")?
        .len()
        != artifact.size
    {
        return Err("update_candidate_size_mismatch".into());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "update_candidate_unavailable")?;
    let mut reader = file.take(
        artifact
            .size
            .checked_add(1)
            .ok_or("update_candidate_invalid")?,
    );
    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|_| "update_candidate_unavailable")?;
        if n == 0 {
            break;
        }
        length += n as u64;
        hash.update(&buffer[..n]);
    }
    if length != artifact.size || hex::encode(hash.finalize()) != artifact.sha256 {
        return Err("update_candidate_hash_mismatch".into());
    }
    Ok(())
}
pub(crate) fn hold_payload(
    directory: PathBuf,
    manifest: release::Manifest,
    hash: String,
    metadata: Option<crate::Metadata>,
    now: i64,
) -> Result<VerifiedPayload, String> {
    let binaries = StateDirectory::new(&directory, None)
        .and_then(|d| d.release_binaries())
        .map_err(|_| "update_candidate_unavailable")?;
    let mut files = Vec::new();
    let mut bytes = 0u64;
    for artifact in &manifest.files {
        let mut f = binaries
            .open_execution(
                &ArtifactName::new(&artifact.name).map_err(|_| "update_candidate_invalid")?,
            )
            .map_err(|_| "update_candidate_unavailable")?
            .ok_or("update_candidate_file_missing")?;
        hash_held(&mut f, artifact)?;
        bytes = bytes
            .checked_add(artifact.size)
            .ok_or("update_candidate_invalid")?;
        files.push(f);
    }
    Ok(VerifiedPayload {
        observation: CandidateObservation {
            manifest_sha256: hash,
            sequence: metadata.as_ref().map_or(0, |m| m.sequence),
            version: manifest.version.clone(),
            target: manifest.target.clone(),
            verified_at_ms: now,
            files: manifest.files.len(),
            bytes,
            activation_supported: false,
        },
        directory,
        manifest,
        metadata,
        files,
    })
}

impl UpdateStore {
    /// Verify saved signed evidence and all bytes without network access. Does
    /// not advance trust, select a release, execute code, or confer authority.
    pub fn verify_staged(&self, hash: &str, now: i64) -> Result<CandidateObservation, String> {
        let _lock = self.lock()?;
        Ok(self.verified_staged_locked(hash, now)?.observation)
    }
    pub(crate) fn verified_staged_locked(
        &self,
        hash: &str,
        now: i64,
    ) -> Result<VerifiedPayload, String> {
        if !metadata::valid_hash(hash) {
            return Err("update_candidate_invalid".into());
        }
        let status = self
            .status()?
            .into_iter()
            .find(|s| s.manifest_sha256 == hash)
            .ok_or("update_candidate_not_found")?;
        if status.phase != "staged" {
            return Err("update_candidate_not_staged".into());
        }
        let trust = self.trust()?;
        let row = self.db.read(|c| {
            c.query_row("SELECT s.metadata,s.signature,p.document,p.signature FROM update_stages s JOIN update_manifest_proofs p ON p.hash=s.hash WHERE s.hash=?", [hash], |r| {
                Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,Vec<u8>>(2)?,r.get::<_,Vec<u8>>(3)?))
            }).optional().map_err(Into::into)
        }).map_err(|_|"update_state_unavailable")?.ok_or("update_candidate_proof_unavailable")?;
        let (m, _) = metadata::verify(&trust, &row.0, &row.1, now)?;
        if m.manifest_sha256 != hash || m.sequence != status.sequence || m.version != status.version
        {
            return Err("update_candidate_identity_mismatch".into());
        }
        let pin = hex::decode(hash).map_err(|_| "update_candidate_invalid")?;
        let key = hex::decode(&trust.release_key).map_err(|_| "update_trust_invalid")?;
        let manifest = release::verify_manifest_bytes(&row.2, &row.3, &pin, &key)?;
        if manifest.target != m.target || manifest.version != m.version || manifest.files.len() != 4
        {
            return Err("update_manifest_mismatch".into());
        }
        hold_payload(
            self.candidate(hash),
            manifest,
            hash.to_owned(),
            Some(m),
            now,
        )
    }
}
