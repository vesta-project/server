//! Fresh, offline observations of staged bytes. Never an activation capability:
//! a future activator must retain/recheck its own opened handles at execution.
use crate::{UpdateStore, metadata, release};
use rusqlite::OptionalExtension;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Read;
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

impl UpdateStore {
    /// Verify saved signed evidence and all bytes without network access. Does
    /// not advance trust, select a release, execute code, or confer authority.
    pub fn verify_staged(&self, hash: &str, now: i64) -> Result<CandidateObservation, String> {
        if !metadata::valid_hash(hash) {
            return Err("update_candidate_invalid".into());
        }
        let _lock = self.lock()?;
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
        let root = StateDirectory::new(self.candidate(hash), None)
            .map_err(|_| "update_candidate_unavailable")?;
        let binaries = root
            .release_binaries()
            .map_err(|_| "update_candidate_unavailable")?;
        let mut total = 0u64;
        for artifact in &manifest.files {
            let name = ArtifactName::new(&artifact.name).map_err(|_| "update_candidate_invalid")?;
            let mut f = binaries
                .open(&name)
                .map_err(|_| "update_candidate_unavailable")?
                .ok_or("update_candidate_file_missing")?;
            if f.metadata()
                .map_err(|_| "update_candidate_unavailable")?
                .len()
                != artifact.size
            {
                return Err("update_candidate_size_mismatch".into());
            }
            let mut reader = (&mut f).take(
                artifact
                    .size
                    .checked_add(1)
                    .ok_or("update_candidate_invalid")?,
            );
            let mut digest = Sha256::new();
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
                digest.update(&buffer[..n]);
            }
            if length != artifact.size || hex::encode(digest.finalize()) != artifact.sha256 {
                return Err("update_candidate_hash_mismatch".into());
            }
            total = total
                .checked_add(length)
                .ok_or("update_candidate_invalid")?;
        }
        Ok(CandidateObservation {
            manifest_sha256: hash.into(),
            sequence: m.sequence,
            version: m.version,
            target: m.target,
            verified_at_ms: now,
            files: manifest.files.len(),
            bytes: total,
            activation_supported: false,
        })
    }
}
