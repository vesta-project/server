//! In-place restore jobs (F27).
//!
//! - Two steps: `prepare_restore` records the exact request (snapshot, paths, whether a
//!   safety snapshot is taken) and returns a one-time token, valid two minutes, that
//!   only the same principal and device can redeem with `confirm_restore`. A wrong
//!   token burns the request.
//! - Only a promoted restore point of the profile, or one of its safety snapshots, can
//!   be restored; candidates and rejected snapshots cannot.
//! - The game must already be stopped: the worker takes the start gate, the supervisor
//!   hold and Vesta's file mutation lock, then requires affirmative stopped facts and,
//!   with a supervisor, no pending crash restart. Nothing here stops or starts the game.
//! - Unless the request explicitly skips it, a safety snapshot of the current tree is
//!   taken first and kept (newest five per profile) so the restore can be undone.
//! - Ignored and protected paths are never written or removed.
//! - Before the live tree changes, the job is durably marked as applying. If it fails
//!   after changing anything, or the agent dies meanwhile, the job ends
//!   `recovery_required` and every start stays blocked, across restarts, until a
//!   later full restore succeeds or an authorized user resolves it. Nothing is ever
//!   restored or rolled back automatically.
use super::{
    BackupError, Backups, Claimed, Inner, JobState, MAX_ACTIVE_JOBS, Outcome, REJECTED, Result,
    allowed, engine, ignore_file, profile_tag,
};
use crate::{
    config::BackupProfile,
    identity::{Capability, Subject},
    operations::ConfirmationToken,
};
use engine::{EngineError, Retention, SAFETY_PREFIX};
use rusqlite::{OptionalExtension, params};
use vesta_state::{
    StateDatabase,
    audit::{AuditActor, AuditLog, AuditRecord},
};

const CONFIRM_TTL_MS: i64 = 120_000;
/// Safety snapshots kept per profile.
const SAFETY_KEEP: u32 = 5;
const MAX_PATHS: usize = 64;

/// A prepared in-place restore awaiting confirmation. The token is returned once.
#[derive(Debug)]
pub struct PreparedRestore {
    pub job_id: String,
    pub token: ConfirmationToken,
    pub expires_at_ms: i64,
}

pub(super) fn safety_tag(profile: &str) -> String {
    format!("{SAFETY_PREFIX}{profile}")
}

fn record(
    subject: &Subject,
    action: &str,
    job: &str,
    outcome: &str,
) -> vesta_state::Result<AuditRecord> {
    AuditRecord::new(
        AuditActor::new(
            subject.principal.as_str().into(),
            subject.device.as_str().into(),
            "backups".into(),
        )?,
        action.into(),
        format!("backup-job:{job}"),
        outcome.into(),
        None,
        [],
    )
}

impl Backups {
    /// Record an in-place restore of `snapshot` (all of it, or only `paths`) for the
    /// subject to confirm. `skip_safety_snapshot` must be explicit to restore without
    /// first taking a safety snapshot.
    pub fn prepare_restore(
        &self,
        subject: &Subject,
        profile: &str,
        snapshot: &str,
        paths: Vec<String>,
        skip_safety_snapshot: bool,
    ) -> Result<PreparedRestore> {
        if self.inner.policy().profile(profile).is_none() {
            return Err(BackupError::UnknownProfile);
        }
        if !engine::valid_snapshot_id(snapshot)
            || paths.len() > MAX_PATHS
            || paths
                .iter()
                .any(|p| crate::files::RelativePath::parse(p).is_err() || !engine::literal_path(p))
        {
            return Err(BackupError::Invalid);
        }
        let token = ConfirmationToken::random().map_err(|_| BackupError::Unavailable)?;
        let hash = token.hash();
        let target = serde_json::json!({
            "snapshot": snapshot,
            "paths": paths,
            "safety_snapshot": !skip_safety_snapshot,
        })
        .to_string();
        let id = uuid::Uuid::new_v4().to_string();
        let db = &self.inner.db;
        let audit = AuditLog::new(db.clone())?;
        let now = db.clock().millis();
        let expires_at_ms = now + CONFIRM_TTL_MS;
        db.write(|c| {
            if !allowed(c, db, subject, Capability::BackupsRestore)? {
                return Ok(Err(BackupError::Forbidden));
            }
            expire_confirmations(c, now, now)?;
            let active: i64 = c.query_row(
                "SELECT count(*) FROM backup_jobs WHERE state IN ('awaiting_confirmation','queued','running')",
                [],
                |r| r.get(0),
            )?;
            if active >= MAX_ACTIVE_JOBS {
                return Ok(Err(BackupError::Busy));
            }
            c.execute(
                "INSERT INTO backup_jobs (id,profile,kind,state,requested_by,requested_device,target,created_at,confirmation_hash,confirm_by) VALUES (?,?,'restore_in_place','awaiting_confirmation',?,?,?,?,?,?)",
                params![
                    id,
                    profile,
                    subject.principal.as_str(),
                    subject.device.as_str(),
                    target,
                    now,
                    hash.as_str(),
                    expires_at_ms
                ],
            )?;
            audit.append(c, &record(subject, "backups.restore_in_place.prepare", &id, "pending")?)?;
            Ok(Ok(()))
        })??;
        Ok(PreparedRestore {
            job_id: id,
            token,
            expires_at_ms,
        })
    }

    /// Queue a prepared restore. Only the preparing principal and device can confirm,
    /// once, before expiry; a wrong token burns the request.
    pub fn confirm_restore(
        &self,
        subject: &Subject,
        job_id: &str,
        token: &ConfirmationToken,
    ) -> Result<()> {
        let db = &self.inner.db;
        let audit = AuditLog::new(db.clone())?;
        let now = db.clock().millis();
        let presented = token.hash();
        db.write(|c| {
            let row = c
                .query_row(
                    "SELECT kind,state,requested_by,requested_device,confirmation_hash,confirm_by FROM backup_jobs WHERE id=?",
                    params![job_id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, Option<String>>(4)?,
                            r.get::<_, Option<i64>>(5)?,
                        ))
                    },
                )
                .optional()?;
            let Some((kind, state, principal, device, hash, confirm_by)) = row else {
                return Ok(Err(BackupError::UnknownJob));
            };
            if kind != "restore_in_place"
                || state != "awaiting_confirmation"
                || principal != subject.principal.as_str()
                || device != subject.device.as_str()
            {
                return Ok(Err(BackupError::ConfirmationInvalid));
            }
            let refusal = if confirm_by.is_none_or(|by| now > by) {
                Some("backup_confirmation_expired")
            } else if hash.as_deref() != Some(presented.as_str()) {
                Some("backup_confirmation_invalid")
            } else if !allowed(c, db, subject, Capability::BackupsRestore)? {
                Some("backup_authority_revoked")
            } else {
                None
            };
            if let Some(code) = refusal {
                c.execute(
                    "UPDATE backup_jobs SET state='failed',error_code=?,confirmation_hash=NULL,confirm_by=NULL,finished_at=? WHERE id=?",
                    params![code, now, job_id],
                )?;
                audit.append(c, &record(subject, "backups.restore_in_place.confirm", job_id, "refused")?)?;
                return Ok(Err(if code == "backup_authority_revoked" {
                    BackupError::Forbidden
                } else {
                    BackupError::ConfirmationInvalid
                }));
            }
            c.execute(
                "UPDATE backup_jobs SET state='queued',confirmation_hash=NULL,confirm_by=NULL WHERE id=?",
                params![job_id],
            )?;
            audit.append(c, &record(subject, "backups.restore_in_place.confirm", job_id, "accepted")?)?;
            Ok(Ok(()))
        })??;
        self.inner.notify();
        Ok(())
    }

    /// Accept the current state of an interrupted in-place restore without restoring
    /// again. Starts are allowed again once no restore needs recovery.
    pub fn resolve_recovery(&self, subject: &Subject, job_id: &str) -> Result<()> {
        let db = &self.inner.db;
        let audit = AuditLog::new(db.clone())?;
        let now = db.clock().millis();
        db.write(|c| {
            if !allowed(c, db, subject, Capability::BackupsRestore)? {
                return Ok(Err(BackupError::Forbidden));
            }
            let changed = c.execute(
                "UPDATE backup_jobs SET state='failed',result_json=?,finished_at=? WHERE id=? AND state='recovery_required'",
                params![
                    serde_json::json!({"recovery":"resolved_by_user"}).to_string(),
                    now,
                    job_id
                ],
            )?;
            if changed == 0 {
                return Ok(Err(BackupError::UnknownJob));
            }
            audit.append(c, &record(subject, "backups.restore_in_place.resolve", job_id, "accepted")?)?;
            Ok(Ok(()))
        })??;
        self.inner.refresh_recovery();
        Ok(())
    }

    /// Interrupted in-place restores that currently block starts, as
    /// `(job id, error code)`, oldest first.
    pub fn recovery_jobs(&self) -> Result<Vec<(String, Option<String>)>> {
        recovery_jobs(&self.inner.db)
    }
}

/// Interrupted in-place restores, oldest first.
pub fn recovery_jobs(db: &StateDatabase) -> Result<Vec<(String, Option<String>)>> {
    Ok(db.read(|c| {
        let mut s = c.prepare(
            "SELECT id,error_code FROM backup_jobs WHERE state='recovery_required' ORDER BY created_at,id",
        )?;
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    })?)
}

/// Local operator resolution, for an agent that is **not running** (the caller holds
/// the server's exclusive local lease): accept the current files of one interrupted
/// in-place restore so starts are allowed again once none remain. Audited as the
/// protected local owner. A running agent rereads this state when it next opens.
pub fn resolve_recovery_locally(db: &StateDatabase, job_id: &str) -> Result<()> {
    let audit = AuditLog::new(db.clone())?;
    let now = db.clock().millis();
    db.write(|c| {
        let changed = c.execute(
            "UPDATE backup_jobs SET state='failed',result_json=?,finished_at=? WHERE id=? AND state='recovery_required'",
            params![
                serde_json::json!({"recovery":"resolved_locally"}).to_string(),
                now,
                job_id
            ],
        )?;
        if changed == 0 {
            return Ok(Err(BackupError::UnknownJob));
        }
        let record = AuditRecord::new(
            AuditActor::new(
                "local-os-owner".into(),
                "local".into(),
                "protected-local-control".into(),
            )?,
            "backups.restore_in_place.resolve".into(),
            format!("backup-job:{job_id}"),
            "accepted".into(),
            None,
            [],
        )?;
        audit.append(c, &record)?;
        Ok(Ok(()))
    })?
}

impl Backups {
    /// Whether an interrupted in-place restore is blocking every game start.
    pub fn recovery_required(&self) -> bool {
        self.inner
            .recovery
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    /// Safety snapshots taken before in-place restores of `profile`, oldest first.
    pub fn safety_snapshots(&self, profile: &str) -> Result<Vec<engine::Snapshot>> {
        if self.inner.policy().profile(profile).is_none() {
            return Err(BackupError::UnknownProfile);
        }
        let _single = self
            .inner
            .listing
            .try_lock()
            .map_err(|_| BackupError::Busy)?;
        self.inner
            .engine
            .snapshots(&safety_tag(profile))
            .map_err(|error| match error {
                EngineError::Unavailable => BackupError::Busy,
                _ => BackupError::Unavailable,
            })
    }
}

/// Unconfirmed requests whose deadline is before `before` can no longer be confirmed.
pub(super) fn expire_confirmations(
    c: &rusqlite::Connection,
    now: i64,
    before: i64,
) -> rusqlite::Result<usize> {
    c.execute(
        "UPDATE backup_jobs SET state='failed',error_code='backup_confirmation_expired',confirmation_hash=NULL,confirm_by=NULL,finished_at=? WHERE state='awaiting_confirmation' AND confirm_by < ?",
        params![now, before],
    )
}

impl Inner {
    /// Hold a start block exactly while some in-place restore needs recovery.
    pub(super) fn refresh_recovery(&self) {
        let pending = self
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM backup_jobs WHERE state='recovery_required'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            // Unknown: keep blocking rather than risk starting a half-restored world.
            .map_or(true, |n| n > 0);
        let mut recovery = self.recovery.lock().unwrap_or_else(|p| p.into_inner());
        if pending {
            if recovery.is_none() {
                *recovery = Some(self.env.start_gate.block());
            }
        } else {
            *recovery = None;
        }
    }

    pub(super) fn restore_in_place(&self, job: &Claimed, profile: &BackupProfile) -> Outcome {
        let Some(request) = job
            .target
            .as_deref()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        else {
            return Outcome::failed(BackupError::Invalid.code());
        };
        let snapshot = request["snapshot"].as_str().unwrap_or_default().to_owned();
        let take_safety = request["safety_snapshot"].as_bool().unwrap_or(true);
        let paths: Vec<String> = request["paths"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let tags = match self.engine.snapshot_tags(&snapshot) {
            Ok(tags) => tags,
            Err(error) => return Outcome::failed(error.code()),
        };
        let restorable = !tags.iter().any(|t| t == REJECTED)
            && tags
                .iter()
                .any(|t| *t == profile_tag(&profile.name) || *t == safety_tag(&profile.name));
        if !restorable {
            return Outcome::failed("backup_snapshot_not_restorable");
        }
        let ignore = match ignore_file(&self.game) {
            Ok(ignore) => ignore,
            Err(code) => return Outcome::failed(code),
        };

        let admission = match crate::files::mutation_admission(self.db.directory().path()) {
            Ok(admission) => admission,
            Err(_) => return Outcome::failed(BackupError::Unavailable.code()),
        };
        let _files = admission.lock().unwrap_or_else(|p| p.into_inner());
        let consistency = match self.stopped() {
            Ok(consistency) => consistency,
            Err(code) => return Outcome::failed(code),
        };
        if let Some(client) = &self.env.supervisor {
            // The hold keeps the game stopped, so this cannot become pending meanwhile.
            let restarting = vesta_supervisor::save::health(client).map_or(true, |h| {
                h.restart_pending_ms.is_some() || h.launch_unresolved
            });
            if restarting {
                consistency.release();
                return Outcome::failed("backup_restore_restart_pending");
            }
        }

        let safety = if take_safety {
            // Exempt from count and size limits, but never fills the repository disk.
            let budget = match self.disk_budget() {
                Ok(budget) => budget,
                Err(code) => {
                    consistency.release();
                    return Outcome::failed(code);
                }
            };
            let tags = ["vesta".to_owned(), safety_tag(&profile.name)];
            match self.engine.backup_within(
                &self.game,
                &ignore,
                &tags,
                Some(&profile_tag(&profile.name)),
                budget.bytes,
            ) {
                Ok(summary) => Some(summary.snapshot_id),
                Err(EngineError::LimitReached) => {
                    consistency.release();
                    return Outcome::failed(budget.exceeded());
                }
                Err(_) => {
                    consistency.release();
                    return Outcome::failed("backup_safety_snapshot_failed");
                }
            }
        } else {
            None
        };
        // From here the live tree may change: an agent crash means recovery required.
        let marked = self.db.write(|c| {
            c.execute(
                "UPDATE backup_jobs SET applying=1,safety_snapshot_id=? WHERE id=?",
                params![safety, job.id],
            )?;
            Ok(())
        });
        if marked.is_err() {
            consistency.release();
            return Outcome::failed(BackupError::State.code());
        }
        let result = self.engine.apply(
            &snapshot,
            &self.game,
            &paths,
            &ignore,
            &self.protected,
            Some(self.policy().limits.min_free_bytes),
        );
        if matches!(result, Err(e) if e.modified) {
            // Block starts before the hold and gate are released.
            let mut recovery = self.recovery.lock().unwrap_or_else(|p| p.into_inner());
            if recovery.is_none() {
                *recovery = Some(self.env.start_gate.block());
            }
        }
        let kept = consistency.release();
        if safety.is_some() {
            let _ = self.engine.forget(
                &safety_tag(&profile.name),
                Retention {
                    keep_last: SAFETY_KEEP,
                    ..Retention::default()
                },
            );
        }
        match result {
            Ok(summary) => {
                if paths.is_empty() {
                    // A complete restore supersedes earlier interrupted ones.
                    let now = self.db.clock().millis();
                    let _ = self.db.write(|c| {
                        c.execute(
                            "UPDATE backup_jobs SET state='failed',result_json=?,finished_at=? WHERE state='recovery_required'",
                            params![
                                serde_json::json!({"recovery":"superseded_by_restore","job":job.id}).to_string(),
                                now
                            ],
                        )?;
                        Ok(())
                    });
                    self.refresh_recovery();
                }
                Outcome {
                    state: JobState::Succeeded,
                    snapshot: Some(snapshot),
                    result: serde_json::json!({
                        "mode": "in_place",
                        "safety_snapshot_id": safety,
                        "files_written": summary.files_written,
                        "files_unchanged": summary.files_unchanged,
                        "files_removed": summary.files_removed,
                        "dirs_removed": summary.dirs_removed,
                        "hold_kept": kept,
                    }),
                    error: None,
                }
            }
            Err(failure) => Outcome {
                state: if failure.modified {
                    JobState::RecoveryRequired
                } else {
                    JobState::Failed
                },
                snapshot: Some(snapshot),
                result: serde_json::json!({
                    "mode": "in_place",
                    "safety_snapshot_id": safety,
                    "modified": failure.modified,
                }),
                error: Some(failure.error.code()),
            },
        }
    }
}

/// Relative `/`-separated forms of the protected paths that lie inside the game
/// directory. Matching folds case on Windows, where names are case-insensitive.
pub(super) fn protected_inside(
    game: &std::path::Path,
    protected: &[std::path::PathBuf],
) -> Vec<String> {
    let mut out: Vec<String> = protected
        .iter()
        .filter_map(|p| p.strip_prefix(game).ok())
        .filter_map(|rel| {
            let parts: Option<Vec<&str>> =
                rel.components().map(|c| c.as_os_str().to_str()).collect();
            parts.filter(|p| !p.is_empty()).map(|p| p.join("/"))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}
