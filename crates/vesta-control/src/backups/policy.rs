//! Backup profiles, retention and limits, editable at runtime (F25).
//!
//! `host.toml` supplies the initial policy. Once an owner/admin saves one through the API it
//! is stored (`backup_policy`, one row) and replaces the host.toml profiles and limits
//! until it is reset. The repository location, S3 credentials and restore root are
//! never part of it: they use trusted host settings or the fixed instance default. Every save names the revision it
//! replaces, so concurrent editors cannot silently overwrite each other. Jobs read the
//! current policy when they start; a removed profile's queued jobs fail with
//! `backup_profile_unknown`, and its existing restore points stay in the repository.
use super::{BackupError, Backups, Inner, Result, allowed};
use crate::{
    config::{
        BackupLimits, BackupMode, BackupProfile, BackupSettings, DEFAULT_MIN_FREE_BYTES,
        DEFAULT_WARN_SECONDS, MutationPolicy, validate_backup_policy,
    },
    identity::{Capability, Subject},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use vesta_state::{
    StateDatabase,
    audit::{AuditActor, AuditLog, AuditRecord},
};

/// The profiles and limits in force.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupPolicy {
    pub profiles: Vec<BackupProfile>,
    pub limits: BackupLimits,
    /// The stored revision, or `None` while host.toml applies.
    pub revision: Option<i64>,
}
impl BackupPolicy {
    pub fn profile(&self, name: &str) -> Option<&BackupProfile> {
        self.profiles.iter().find(|p| p.name == name)
    }
    fn from_settings(settings: &BackupSettings) -> Self {
        Self {
            profiles: settings.profiles.clone(),
            limits: settings.limits.clone(),
            revision: None,
        }
    }
}

/// The stored and API form of a policy. Closed: unknown fields are refused.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyDocument {
    pub profiles: Vec<ProfileDocument>,
    #[serde(default)]
    pub limits: LimitsDocument,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProfileDocument {
    pub name: String,
    /// `online` or `stopped`.
    pub mode: String,
    #[serde(default)]
    pub keep_last: u32,
    #[serde(default)]
    pub keep_daily: u32,
    #[serde(default)]
    pub keep_weekly: u32,
    #[serde(default)]
    pub keep_monthly: u32,
    /// `warn`, `fail-world` (default) or `fail-any`.
    #[serde(default)]
    pub on_mutation: Option<String>,
    #[serde(default)]
    pub warn_seconds: Option<u32>,
    #[serde(default)]
    pub max_snapshots: Option<u32>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LimitsDocument {
    #[serde(default)]
    pub max_snapshots: Option<u32>,
    #[serde(default)]
    pub max_repository_bytes: Option<u64>,
    /// Defaults to 1 GiB when absent.
    #[serde(default)]
    pub min_free_bytes: Option<u64>,
}

impl PolicyDocument {
    /// Validate with the same rules as host.toml.
    pub fn to_policy(&self) -> Result<(Vec<BackupProfile>, BackupLimits)> {
        let mut profiles = Vec::with_capacity(self.profiles.len());
        for p in &self.profiles {
            let mode = match p.mode.as_str() {
                "online" => BackupMode::Online,
                "stopped" => BackupMode::Stopped,
                _ => return Err(BackupError::Invalid),
            };
            let on_mutation = match p.on_mutation.as_deref() {
                None | Some("fail-world") => MutationPolicy::FailWorld,
                Some("warn") => MutationPolicy::Warn,
                Some("fail-any") => MutationPolicy::FailAny,
                Some(_) => return Err(BackupError::Invalid),
            };
            profiles.push(BackupProfile {
                name: p.name.clone(),
                mode,
                keep_last: p.keep_last,
                keep_daily: p.keep_daily,
                keep_weekly: p.keep_weekly,
                keep_monthly: p.keep_monthly,
                on_mutation,
                warn_seconds: p.warn_seconds.unwrap_or(DEFAULT_WARN_SECONDS),
                max_snapshots: p.max_snapshots,
                include: p.include.clone(),
                exclude: p.exclude.clone(),
            });
        }
        let limits = BackupLimits {
            max_snapshots: self.limits.max_snapshots,
            max_repository_bytes: self.limits.max_repository_bytes,
            min_free_bytes: self.limits.min_free_bytes.unwrap_or(DEFAULT_MIN_FREE_BYTES),
        };
        validate_backup_policy(&profiles, &limits).map_err(|_| BackupError::Invalid)?;
        Ok((profiles, limits))
    }

    pub fn from_policy(profiles: &[BackupProfile], limits: &BackupLimits) -> Self {
        Self {
            profiles: profiles
                .iter()
                .map(|p| ProfileDocument {
                    name: p.name.clone(),
                    mode: match p.mode {
                        BackupMode::Online => "online",
                        BackupMode::Stopped => "stopped",
                    }
                    .into(),
                    keep_last: p.keep_last,
                    keep_daily: p.keep_daily,
                    keep_weekly: p.keep_weekly,
                    keep_monthly: p.keep_monthly,
                    on_mutation: Some(
                        match p.on_mutation {
                            MutationPolicy::Warn => "warn",
                            MutationPolicy::FailWorld => "fail-world",
                            MutationPolicy::FailAny => "fail-any",
                        }
                        .into(),
                    ),
                    warn_seconds: Some(p.warn_seconds),
                    max_snapshots: p.max_snapshots,
                    include: p.include.clone(),
                    exclude: p.exclude.clone(),
                })
                .collect(),
            limits: LimitsDocument {
                max_snapshots: limits.max_snapshots,
                max_repository_bytes: limits.max_repository_bytes,
                min_free_bytes: Some(limits.min_free_bytes),
            },
        }
    }
}

/// The policy in force at open: the stored one when present (it must still be valid),
/// otherwise host.toml's.
pub(super) fn load(db: &StateDatabase, settings: &BackupSettings) -> Result<BackupPolicy> {
    let stored: Option<(String, i64)> = db.read(|c| {
        Ok(c.query_row(
            "SELECT policy_json,revision FROM backup_policy WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
    })?;
    let Some((json, revision)) = stored else {
        return Ok(BackupPolicy::from_settings(settings));
    };
    let document: PolicyDocument = serde_json::from_str(&json).map_err(|_| BackupError::State)?;
    let (profiles, limits) = document.to_policy().map_err(|_| BackupError::State)?;
    Ok(BackupPolicy {
        profiles,
        limits,
        revision: Some(revision),
    })
}

fn record(
    subject: &Subject,
    action: &str,
    revision: Option<i64>,
) -> vesta_state::Result<AuditRecord> {
    AuditRecord::new(
        AuditActor::new(
            subject.principal.as_str().into(),
            subject.device.as_str().into(),
            "backups".into(),
        )?,
        action.into(),
        "backup-policy".into(),
        "accepted".into(),
        None,
        [("revision".into(), revision.map(|r| r.to_string()))],
    )
}

impl Inner {
    pub(super) fn policy(&self) -> Arc<BackupPolicy> {
        self.policy
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl Backups {
    /// Observe repository readiness without creating any files.
    pub fn repository_initialized(&self) -> bool {
        self.inner.engine.require_initialized().is_ok()
    }

    /// Save the complete profile set (including an empty set to disable backups).
    /// The repository is fixed by the host; no remote path or credentials are accepted.
    pub fn configure(
        &self,
        subject: &Subject,
        expected_revision: Option<i64>,
        document: &PolicyDocument,
        initialize_repository: bool,
        key: &str,
    ) -> Result<i64> {
        use sha2::{Digest, Sha256};
        let (profiles, limits) = document.to_policy()?;
        if key.is_empty() || key.len() > 200 {
            return Err(BackupError::Invalid);
        }
        let canonical = PolicyDocument::from_policy(&profiles, &limits);
        let json = serde_json::to_string(&canonical).map_err(|_| BackupError::Invalid)?;
        if json.len() > 65_536 {
            return Err(BackupError::Invalid);
        }
        let inner = &self.inner;
        let _single = inner.policy_write.lock().unwrap_or_else(|p| p.into_inner());
        let db = &inner.db;
        let key_hash = Sha256::digest(
            serde_json::to_vec(&(
                "backup-config-v1",
                db.server_id(),
                subject.principal.as_str(),
                subject.device.as_str(),
                key,
            ))
            .map_err(|_| BackupError::Invalid)?,
        );
        let intent_hash = Sha256::digest(
            serde_json::to_vec(&(expected_revision, &canonical, initialize_repository))
                .map_err(|_| BackupError::Invalid)?,
        );
        let audit = AuditLog::new(db.clone())?;
        let now = db.clock().millis();
        let (revision, replay) = db.write(|c| {
            if !allowed(c, db, subject, Capability::BackupsConfigure)? {
                return Ok(Err(BackupError::Forbidden));
            }
            let previous: Option<(Vec<u8>, String)> = c.query_row(
                "SELECT intent_hash,result_json FROM management_permission_receipts WHERE key_hash=?",
                [key_hash.as_slice()], |r| Ok((r.get(0)?, r.get(1)?)),
            ).optional()?;
            if let Some((hash, result)) = previous {
                if hash != intent_hash.as_slice() { return Ok(Err(BackupError::IdempotencyConflict)); }
                let revision = serde_json::from_str::<i64>(&result).map_err(|_| vesta_state::StateError::new("backup_receipt_invalid"))?;
                return Ok(Ok((revision, true)));
            }
            let current: Option<i64> = c.query_row("SELECT revision FROM backup_policy WHERE singleton=1", [], |r| r.get(0)).optional()?;
            if current != expected_revision || current.is_some_and(|r| r >= 9_007_199_254_740_991) {
                return Ok(Err(BackupError::PolicyConflict));
            }
            c.execute("DELETE FROM management_permission_receipts WHERE key_hash IN (SELECT key_hash FROM management_permission_receipts WHERE completed_at<? ORDER BY completed_at LIMIT 256)", [now.saturating_sub(90 * 24 * 60 * 60 * 1000)])?;
            let count: i64 = c.query_row("SELECT count(*) FROM management_permission_receipts", [], |r| r.get(0))?;
            if count >= 4096 { return Ok(Err(BackupError::Busy)); }
            if initialize_repository {
                if super::check_locations(&inner.settings, &inner.game, db.directory().path()).is_err()
                    || !super::safe_repository_location(&inner.settings)
                { return Ok(Err(BackupError::Invalid)); }
                if inner.engine.init().is_err() { return Ok(Err(BackupError::Unavailable)); }
            }
            let revision = current.map_or(1, |r| r + 1);
            c.execute("INSERT INTO backup_policy(singleton,policy_json,revision,updated_at,updated_by) VALUES(1,?,?,?,?) ON CONFLICT(singleton) DO UPDATE SET policy_json=excluded.policy_json,revision=excluded.revision,updated_at=excluded.updated_at,updated_by=excluded.updated_by", params![json,revision,now,subject.principal.as_str()])?;
            audit.append(c, &record(subject, if initialize_repository {"backups.repository.configure"} else {"backups.config.update"}, Some(revision))?)?;
            c.execute("INSERT INTO management_permission_receipts(key_hash,intent_hash,result_json,completed_at) VALUES(?,?,?,?)", params![key_hash.as_slice(),intent_hash.as_slice(),revision.to_string(),now])?;
            Ok(Ok((revision, false)))
        })??;
        if !replay {
            *inner.policy.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(BackupPolicy {
                profiles,
                limits,
                revision: Some(revision),
            });
        }
        Ok(revision)
    }

    /// The profiles and limits in force now.
    pub fn policy(&self) -> Arc<BackupPolicy> {
        self.inner.policy()
    }

    /// Replace the policy. `expected_revision` is the revision being replaced (`None`
    /// while host.toml applies); a mismatch is `PolicyConflict`. Returns the new
    /// revision. Needs the owner/administrator-reserved `backups.configure`.
    pub fn set_policy(
        &self,
        subject: &Subject,
        expected_revision: Option<i64>,
        document: &PolicyDocument,
    ) -> Result<i64> {
        let (profiles, limits) = document.to_policy()?;
        // Stored in its canonical form, so defaults are explicit.
        let canonical = PolicyDocument::from_policy(&profiles, &limits);
        let json = serde_json::to_string(&canonical).map_err(|_| BackupError::Invalid)?;
        let inner = &self.inner;
        let _single = inner.policy_write.lock().unwrap_or_else(|p| p.into_inner());
        let db = &inner.db;
        let audit = AuditLog::new(db.clone())?;
        let now = db.clock().millis();
        let revision = db.write(|c| {
            if !allowed(c, db, subject, Capability::BackupsConfigure)? {
                return Ok(Err(BackupError::Forbidden));
            }
            let current: Option<i64> = c
                .query_row("SELECT revision FROM backup_policy WHERE singleton=1", [], |r| r.get(0))
                .optional()?;
            if current != expected_revision {
                return Ok(Err(BackupError::PolicyConflict));
            }
            let revision = current.map_or(1, |r| r + 1);
            c.execute(
                "INSERT INTO backup_policy (singleton,policy_json,revision,updated_at,updated_by) VALUES (1,?,?,?,?) \
                 ON CONFLICT(singleton) DO UPDATE SET policy_json=excluded.policy_json,revision=excluded.revision,updated_at=excluded.updated_at,updated_by=excluded.updated_by",
                params![json, revision, now, subject.principal.as_str()],
            )?;
            audit.append(c, &record(subject, "backups.policy.update", Some(revision))?)?;
            Ok(Ok(revision))
        })??;
        *inner.policy.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(BackupPolicy {
            profiles,
            limits,
            revision: Some(revision),
        });
        Ok(revision)
    }

    /// Drop the stored policy so host.toml applies again. `expected_revision` must be
    /// the stored revision.
    pub fn reset_policy(&self, subject: &Subject, expected_revision: i64) -> Result<()> {
        let inner = &self.inner;
        let _single = inner.policy_write.lock().unwrap_or_else(|p| p.into_inner());
        let db = &inner.db;
        let audit = AuditLog::new(db.clone())?;
        db.write(|c| {
            if !allowed(c, db, subject, Capability::BackupsConfigure)? {
                return Ok(Err(BackupError::Forbidden));
            }
            let removed = c.execute(
                "DELETE FROM backup_policy WHERE singleton=1 AND revision=?",
                params![expected_revision],
            )?;
            if removed == 0 {
                return Ok(Err(BackupError::PolicyConflict));
            }
            audit.append(c, &record(subject, "backups.policy.reset", None)?)?;
            Ok(Ok(()))
        })??;
        *inner.policy.write().unwrap_or_else(|p| p.into_inner()) =
            Arc::new(BackupPolicy::from_settings(&inner.settings));
        Ok(())
    }
}
