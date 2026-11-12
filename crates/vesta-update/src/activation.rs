//! OS-local, stopped-installation activation journal. Native composition owns the
//! live fences, health transport and restart supervision; none is an HTTP DTO.
//! Active selection changes only after execution-bound health. Recovery selects
//! the exact prior healthy payload and never restores a database or lowers trust.
use super::store::{Result, unavailable};
use crate::{
    UpdateStore, metadata, release,
    verification::{VerifiedPayload, hold_payload},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const ACTIVATION_PROTOCOL: u16 = vesta_state::UPDATE_ACTIVATION_PROTOCOL;
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub installation_id: String,
    pub server_id: String,
    pub state_schema: u32,
    pub supervisor_ipc: u8,
    pub tls_sha256: String,
    pub configuration_sha256: String,
    pub launch_sha256: String,
}
impl Scope {
    fn validate(&self) -> Result<()> {
        if self.installation_id.len() != 32
            || !self.installation_id.bytes().all(|b| b.is_ascii_hexdigit())
            || self.server_id.len() != 36
            || self.state_schema == 0
            || self.supervisor_ipc == 0
            || [
                &self.tls_sha256,
                &self.configuration_sha256,
                &self.launch_sha256,
            ]
            .iter()
            .any(|h| !metadata::valid_hash(h))
        {
            return Err("update_scope_unproven".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Health {
    pub manifest_sha256: String,
    pub scope: Scope,
}

/// Implement ONLY at an authenticated native installation boundary. `observe`
/// must retain actual host/consumer/runtime fences and prove stopped/no pending
/// writes. A serialized Scope or successful earlier status is not such a fence.
pub trait ActivationHost {
    fn observe(&mut self) -> Result<Scope>;
    /// Run the verified candidate's restricted health probe, not Minecraft. The
    /// returned proof must bind this payload, TLS/server identity and schema.
    fn trial(&mut self, payload: &mut VerifiedPayload) -> Result<Health>;
}
#[derive(Clone, Serialize, Deserialize)]
struct Selection {
    hash: String,
    directory: PathBuf,
    metadata: Option<crate::Metadata>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub id: String,
    pub state: String,
    pub candidate: String,
    pub previous: String,
    pub error: Option<String>,
}
fn encode<T: Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(unavailable)
}
fn state_error<T>(_: T) -> vesta_state::StateError {
    vesta_state::StateError::new("state_update_activation")
}
fn decode<T: serde::de::DeserializeOwned>(v: &str) -> Result<T> {
    serde_json::from_str(v).map_err(unavailable)
}

impl UpdateStore {
    pub(crate) fn activation_schema(&self) -> Result<()> {
        self.db.write(|c|{c.execute_batch("CREATE TABLE IF NOT EXISTS update_active(singleton INTEGER PRIMARY KEY CHECK(singleton=1),scope TEXT NOT NULL,selection TEXT NOT NULL) STRICT;
            CREATE TABLE IF NOT EXISTS update_activation(id TEXT PRIMARY KEY,request_key TEXT UNIQUE NOT NULL,scope TEXT NOT NULL,candidate TEXT NOT NULL,previous TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('trial','restart_pending','healthy','recovered','recovery_required')),error TEXT) STRICT;")?;Ok(())}).map_err(unavailable)
    }
    /// Import proof of the exact already-installed release selected by the local
    /// receipt. This is not a downgrade/download operation or a remote API seam.
    pub fn register_installed(
        &self,
        installed: &release::Release,
        directory: &Path,
        host: &mut dyn ActivationHost,
        now: i64,
    ) -> Result<()> {
        let _lock = self.lock()?;
        installed.ensure_intact()?;
        if installed.release_key() != self.trust()?.release_key {
            return Err("update_installed_key_mismatch".into());
        }
        let (bytes, signature) = installed.signed_manifest()?;
        self.register_manifest(
            installed.manifest.clone(),
            installed.manifest_hash(),
            bytes,
            signature,
            directory,
            host,
            now,
        )
    }
    pub fn register_saved_installed(
        &self,
        hash: &str,
        directory: &Path,
        host: &mut dyn ActivationHost,
        now: i64,
    ) -> Result<()> {
        let _lock = self.lock()?;
        if !metadata::valid_hash(hash) {
            return Err("update_installed_proof_required".into());
        }
        let row = self
            .db
            .read(|c| {
                c.query_row(
                    "SELECT document,signature FROM update_manifest_proofs WHERE hash=?",
                    [hash],
                    |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?)),
                )
                .optional()
                .map_err(Into::into)
            })
            .map_err(unavailable)?
            .ok_or("update_installed_proof_required")?;
        let key = hex::decode(&self.trust()?.release_key).map_err(unavailable)?;
        let pin = hex::decode(hash).map_err(unavailable)?;
        let manifest = release::verify_manifest_bytes(&row.0, &row.1, &pin, &key)?;
        self.register_manifest(manifest, hash, &row.0, &row.1, directory, host, now)
    }
    #[allow(clippy::too_many_arguments)]
    fn register_manifest(
        &self,
        manifest: release::Manifest,
        hash: &str,
        bytes: &[u8],
        signature: &[u8],
        directory: &Path,
        host: &mut dyn ActivationHost,
        now: i64,
    ) -> Result<()> {
        let directory = vesta_state::StateDirectory::new(directory, None)
            .map_err(unavailable)?
            .path()
            .to_owned();
        let scope = host.observe()?;
        scope.validate()?;
        let existing = self
            .db
            .read(|c| {
                c.query_row(
                    "SELECT scope,selection FROM update_active WHERE singleton=1",
                    [],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(Into::into)
            })
            .map_err(unavailable)?;
        if let Some((old, s)) = existing {
            let selected: Selection = decode(&s)?;
            if decode::<Scope>(&old)? == scope
                && selected.hash == hash
                && selected.directory == directory
            {
                return Ok(());
            }
            return Err("update_installation_already_registered".into());
        }
        let mut payload =
            hold_payload(directory.clone(), manifest.clone(), hash.into(), None, now)?;
        let proof = host.trial(&mut payload)?;
        health(&proof, &scope, hash)?;
        payload.recheck()?;
        let selection = Selection {
            hash: hash.into(),
            directory,
            metadata: None,
        };
        self.db.write(|c|{c.execute("INSERT INTO update_manifest_proofs VALUES(?,?,?) ON CONFLICT(hash) DO UPDATE SET document=excluded.document,signature=excluded.signature",params![selection.hash,bytes,signature])?;c.execute("INSERT INTO update_active VALUES(1,?,?)",params![encode(&scope).map_err(state_error)?,encode(&selection).map_err(state_error)?])?;Ok(())}).map_err(unavailable)
    }
    pub fn registered_for(&self, installation_id: &str) -> Result<bool> {
        let scope = self
            .db
            .read(|c| {
                c.query_row(
                    "SELECT scope FROM update_active WHERE singleton=1",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(Into::into)
            })
            .map_err(unavailable)?;
        match scope {
            None => Ok(false),
            Some(scope) => {
                let scope: Scope = decode(&scope)?;
                scope.validate()?;
                if scope.installation_id != installation_id {
                    Err("update_scope_changed".into())
                } else {
                    Ok(true)
                }
            }
        }
    }
    /// Read-only native command dispatch from the exact committed healthy payload.
    pub fn active_payload(
        &self,
        installation_id: &str,
        now: i64,
    ) -> Result<Option<VerifiedPayload>> {
        let _lock = self.lock()?;
        if !self.registered_for(installation_id)? {
            return Ok(None);
        }
        let (_, selection) = self.active()?;
        self.selected_payload(&selection, now).map(Some)
    }
    pub fn activation_status(&self, id: &str) -> Result<Status> {
        self.db
            .read(|c| status(c, id).map_err(state_error))
            .map_err(unavailable)
    }
    pub fn activations(&self) -> Result<Vec<Status>> {
        self.db
            .read(|c| {
                let mut q =
                    c.prepare("SELECT id FROM update_activation ORDER BY rowid DESC LIMIT 64")?;
                let ids = q
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                ids.iter()
                    .map(|id| status(c, id).map_err(state_error))
                    .collect()
            })
            .map_err(unavailable)
    }
    /// Performs a signed, freshly verified health trial and records a durable
    /// restart request. This does not claim the normal service has restarted.
    pub fn prepare_activation(
        &self,
        hash: &str,
        key: &str,
        host: &mut dyn ActivationHost,
        now: i64,
    ) -> Result<Status> {
        if !metadata::valid_hash(hash)
            || key.is_empty()
            || key.len() > 128
            || !key.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err("update_activation_invalid".into());
        }
        let _lock = self.lock()?;
        let scope = host.observe()?;
        scope.validate()?;
        let old = self
            .db
            .read(|c| {
                c.query_row(
                    "SELECT id,candidate,scope FROM update_activation WHERE request_key=?",
                    [key],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(Into::into)
            })
            .map_err(unavailable)?;
        if let Some((id, candidate, binding)) = old {
            let binding: Scope = decode(&binding)?;
            if binding.installation_id != scope.installation_id
                || binding.server_id != scope.server_id
            {
                return Err("update_scope_changed".into());
            }
            if decode::<Selection>(&candidate)?.hash != hash {
                return Err("update_idempotency_conflict".into());
            }
            return self.activation_status(&id);
        }
        let (original_scope, previous) = self.active()?;
        if original_scope.installation_id != scope.installation_id
            || original_scope.server_id != scope.server_id
            || original_scope.state_schema != scope.state_schema
            || original_scope.supervisor_ipc != scope.supervisor_ipc
        {
            return Err("update_scope_changed".into());
        }
        let pending=self.db.read(|c|c.query_row("SELECT EXISTS(SELECT 1 FROM update_activation WHERE state IN ('trial','restart_pending','recovery_required'))",[],|r|r.get::<_,bool>(0)).map_err(Into::into)).map_err(unavailable)?;
        if pending {
            return Err("update_activation_pending".into());
        }
        if previous.hash == hash {
            let mut payload = self.selected_payload(&previous, now)?;
            payload.recheck()?;
            let id = hex::encode(Sha256::digest(encode(&(key, hash, &scope))?));
            let selection = encode(&previous)?;
            let binding = encode(&scope)?;
            self.db.write(|c|{c.execute("INSERT INTO update_activation(id,request_key,scope,candidate,previous,state) VALUES(?,?,?,?,?,'healthy')",params![id,key,binding,selection,selection])?;Ok(())}).map_err(unavailable)?;
            return self.activation_status(&id);
        }
        let mut candidate = self.verified_staged_locked(hash, now)?;
        compatible(&candidate, &scope)?;
        // Refuse before creating an intent if the only fallback is corrupt.
        let mut fallback = self.selected_payload(&previous, now)?;
        fallback.recheck()?;
        let fallback_health = host.trial(&mut fallback)?;
        health(&fallback_health, &scope, &previous.hash)?;
        fallback.recheck()?;
        let id = hex::encode(Sha256::digest(encode(&(key, hash, &scope))?));
        let target = Selection {
            hash: hash.into(),
            directory: self.candidate(hash),
            metadata: candidate.metadata.clone(),
        };
        self.db.write(|c|{c.execute("INSERT INTO update_activation(id,request_key,scope,candidate,previous,state) VALUES(?,?,?,?,?,'trial')",params![id,key,encode(&scope).map_err(state_error)?,encode(&target).map_err(state_error)?,encode(&previous).map_err(state_error)?])?;Ok(())}).map_err(unavailable)?;
        let trial = host
            .trial(&mut candidate)
            .and_then(|p| health(&p, &scope, hash))
            .and_then(|_| candidate.recheck())
            .and_then(|_| {
                if host.observe()? == scope {
                    Ok(())
                } else {
                    Err("update_scope_changed".into())
                }
            });
        if trial.is_err() {
            self.mark_recovered(&id, "update_trial_failed")?;
            return self.activation_status(&id);
        }
        self.db.write(|c|{c.execute("UPDATE update_activation SET state='restart_pending' WHERE id=? AND state='trial'",[&id])?;Ok(())}).map_err(unavailable)?;
        self.activation_status(&id)
    }
    /// Native bootstrap calls this under its exclusive host/consumer ownership,
    /// BEFORE opening any normal runtime. Unknown/expired trials are not executed.
    pub fn payload_for_start(
        &self,
        host: &mut dyn ActivationHost,
        now: i64,
    ) -> Result<(Option<String>, VerifiedPayload)> {
        let _lock = self.lock()?;
        let scope = host.observe()?;
        scope.validate()?;
        let pending=self.db.read(|c|c.query_row("SELECT id,scope,candidate,state FROM update_activation WHERE state IN ('trial','restart_pending','recovery_required')",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).optional().map_err(Into::into)).map_err(unavailable)?;
        if let Some((id, binding, candidate, state)) = pending {
            if state != "restart_pending" {
                return Err("update_recovery_required".into());
            }
            if decode::<Scope>(&binding)? != scope {
                return Err("update_scope_changed".into());
            }
            let selected: Selection = decode(&candidate)?;
            let payload = self.verified_staged_locked(&selected.hash, now)?;
            compatible(&payload, &scope)?;
            return Ok((Some(id), payload));
        }
        let (bound, selection) = self.active()?;
        if bound.installation_id != scope.installation_id
            || bound.server_id != scope.server_id
            || bound.state_schema != scope.state_schema
            || bound.supervisor_ipc != scope.supervisor_ipc
        {
            return Err("update_scope_changed".into());
        }
        Ok((None, self.selected_payload(&selection, now)?))
    }
    /// Execution-bound local completion only. The native bootstrap retains the
    /// payload handles and validates its own child's protected readiness proof.
    /// This must never be exposed as a caller-submitted HTTP health assertion.
    pub fn confirm_started(
        &self,
        id: &str,
        payload: &mut VerifiedPayload,
        proof: &Health,
    ) -> Result<Status> {
        let _lock = self.lock()?;
        let (binding, target, state) = self
            .db
            .read(|c| {
                c.query_row(
                    "SELECT scope,candidate,state FROM update_activation WHERE id=?",
                    [id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(Into::into)
            })
            .map_err(unavailable)?;
        if state == "healthy" {
            return self.activation_status(id);
        }
        if state != "restart_pending" {
            return Err("update_activation_state_invalid".into());
        }
        let scope: Scope = decode(&binding)?;
        let target: Selection = decode(&target)?;
        health(proof, &scope, &target.hash)?;
        if payload.manifest_sha256() != target.hash {
            return Err("update_health_mismatch".into());
        }
        compatible(payload, &scope)?;
        payload.recheck()?;
        self.db.write(|c|{c.execute("UPDATE update_active SET scope=?,selection=? WHERE singleton=1",params![binding,encode(&target).map_err(state_error)?])?;c.execute("UPDATE update_activation SET state='healthy',error=NULL WHERE id=? AND state='restart_pending'",[id])?;Ok(())}).map_err(unavailable)?;
        self.activation_status(id)
    }
    /// A native caller must first prove the trial service/game is stopped. No
    /// process kill, unknown-generation bypass or old DB restore happens here.
    pub fn recover_activation(
        &self,
        id: &str,
        host: &mut dyn ActivationHost,
        now: i64,
    ) -> Result<Status> {
        let _lock = self.lock()?;
        let observed = host.observe()?;
        observed.validate()?;
        let (scope, previous, state) = self
            .db
            .read(|c| {
                c.query_row(
                    "SELECT scope,previous,state FROM update_activation WHERE id=?",
                    [id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(Into::into)
            })
            .map_err(unavailable)?;
        if state == "recovered" {
            return self.activation_status(id);
        }
        if state == "healthy" {
            return Err("update_completed_recovery_refused".into());
        }
        let expected: Scope = decode(&scope)?;
        if observed != expected {
            return Err("update_scope_changed".into());
        }
        let previous: Selection = decode(&previous)?;
        let mut payload = self.selected_payload(&previous, now)?;
        let proof = host.trial(&mut payload)?;
        health(&proof, &observed, &previous.hash)?;
        payload.recheck()?;
        // Active remained the previous healthy selection throughout the trial.
        let (_, active) = self.active()?;
        if active.hash != previous.hash || active.directory != previous.directory {
            return Err("update_recovery_selection_changed".into());
        }
        self.mark_recovered(id, "update_restart_failed")?;
        self.activation_status(id)
    }
    fn mark_recovered(&self, id: &str, code: &str) -> Result<()> {
        self.db.write(|c|{c.execute("UPDATE update_activation SET state='recovered',error=? WHERE id=? AND state!='healthy'",params![code,id])?;Ok(())}).map_err(unavailable)
    }
    /// Non-authoritative discovery observation: a committed selection supersedes
    /// the immutable bootstrap receipt. Staged/pending candidates do not.
    pub(crate) fn committed_manifest(&self) -> Result<Option<String>> {
        let encoded = self
            .db
            .read(|c| {
                c.query_row(
                    "SELECT selection FROM update_active WHERE singleton=1",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(Into::into)
            })
            .map_err(unavailable)?;
        encoded
            .map(|value| {
                let selected: Selection = decode(&value)?;
                if !metadata::valid_hash(&selected.hash) {
                    return Err("update_selection_invalid".into());
                }
                Ok(selected.hash)
            })
            .transpose()
    }
    fn active(&self) -> Result<(Scope, Selection)> {
        let (scope, selection) = self
            .db
            .read(|c| {
                c.query_row(
                    "SELECT scope,selection FROM update_active WHERE singleton=1",
                    [],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .map_err(Into::into)
            })
            .map_err(|_| "update_installation_not_registered")?;
        Ok((decode(&scope)?, decode(&selection)?))
    }
    fn selected_payload(&self, selection: &Selection, now: i64) -> Result<VerifiedPayload> {
        if !metadata::valid_hash(&selection.hash) {
            return Err("update_selection_invalid".into());
        }
        let (bytes, signature) = self
            .db
            .read(|c| {
                c.query_row(
                    "SELECT document,signature FROM update_manifest_proofs WHERE hash=?",
                    [&selection.hash],
                    |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?)),
                )
                .map_err(Into::into)
            })
            .map_err(unavailable)?;
        let trust = self.trust()?;
        let key = hex::decode(&trust.release_key).map_err(unavailable)?;
        let hash = hex::decode(&selection.hash).map_err(unavailable)?;
        let manifest = release::verify_manifest_bytes(&bytes, &signature, &hash, &key)?;
        // Exact prior healthy selection is recovery authority, not permission
        // to download an arbitrary older release or lower accepted sequence.
        hold_payload(
            selection.directory.clone(),
            manifest,
            selection.hash.clone(),
            selection.metadata.clone(),
            now,
        )
    }
}
fn health(proof: &Health, scope: &Scope, hash: &str) -> Result<()> {
    if proof.scope != *scope || proof.manifest_sha256 != hash {
        Err("update_health_mismatch".into())
    } else {
        Ok(())
    }
}
fn compatible(payload: &VerifiedPayload, scope: &Scope) -> Result<()> {
    let attestation = payload
        .compatibility()
        .ok_or("update_compatibility_unproven")?;
    if attestation.protocol != ACTIVATION_PROTOCOL
        || attestation.state_schema_min != scope.state_schema
        || attestation.state_schema_max != scope.state_schema
        || attestation.supervisor_ipc != scope.supervisor_ipc
    {
        return Err("update_compatibility_requires_migration".into());
    }
    Ok(())
}
fn status(c: &rusqlite::Connection, id: &str) -> Result<Status> {
    let (candidate, previous, state, error) = c
        .query_row(
            "SELECT candidate,previous,state,error FROM update_activation WHERE id=?",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .map_err(unavailable)?;
    Ok(Status {
        id: id.into(),
        state,
        candidate: decode::<Selection>(&candidate)?.hash,
        previous: decode::<Selection>(&previous)?.hash,
        error,
    })
}
