//! Host implementation of the automation action port.
//!
//! Each execution principal acts through a dedicated keyless "Vesta automation"
//! device: it cannot sign in, it shows up in the principal's device list, and revoking
//! it (or the principal's roles) removes the schedules' authority at the next step.
//! Every action is authorized against current policy before submission and runs
//! through the same durable, idempotent paths as interactive requests: game actions
//! are ordinary operations, backups are backup jobs. Nothing here bypasses policy.
use crate::{
    automation::{Action, ActionError, ActionHandle, ActionPort, ActionStatus},
    backups::{Backups, JobKind, JobState},
    files::FileBrowser,
    identity::{Capability, DeviceId, PrincipalId, Subject},
    operations::{GameCommand, OperationId, OperationState, RuntimeHandle},
};
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use vesta_state::StateDatabase;

/// In-flight and finished trash cleanups by idempotency key.
type TrashResults = Arc<Mutex<HashMap<String, Option<Result<usize, ()>>>>>;

pub const AUTOMATION_DEVICE_LABEL: &str = "Vesta automation";

pub struct HostActions {
    db: StateDatabase,
    /// Filled by composition right after the runtime opens; actions refuse until then.
    runtime: Arc<std::sync::OnceLock<RuntimeHandle>>,
    backups: Option<Arc<Backups>>,
    files: FileBrowser,
    /// Trash cleanups run off the automation thread: it never waits on file admission.
    /// Purging expired trash is idempotent, so a lost entry is simply run again.
    trash: TrashResults,
}

/// How long after expiry an unreadable banned-players.json is retried before the
/// temporary ban is blocked for manual review.
pub(crate) const UNREADABLE_BAN_FILE_GRACE_MS: i64 = 60 * 60 * 1000;

impl HostActions {
    pub(crate) fn tick_startup_whitelist(
        &self,
        runtime: &RuntimeHandle,
    ) -> crate::files::Result<Vec<String>> {
        self.files.tick_startup_whitelist(runtime)
    }
    pub fn new(
        db: StateDatabase,
        runtime: Arc<std::sync::OnceLock<RuntimeHandle>>,
        backups: Option<Arc<Backups>>,
        files: FileBrowser,
    ) -> Self {
        Self {
            db,
            runtime,
            backups,
            files,
            trash: Arc::default(),
        }
    }

    fn purge(&self, key: &str) {
        let mut trash = self.trash.lock().unwrap_or_else(|p| p.into_inner());
        if trash.contains_key(key) || trash.len() >= 64 {
            return;
        }
        trash.insert(key.to_owned(), None);
        let (files, results, key) = (self.files.clone(), self.trash.clone(), key.to_owned());
        std::thread::spawn(move || {
            let outcome = files.purge_expired_trash().map_err(|_| ());
            results
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(key, Some(outcome));
        });
    }

    /// The principal's automation device, created on first use. A revoked one is
    /// never recreated: revocation is the owner's way to stop that user's schedules.
    fn subject(&self, principal: &PrincipalId) -> Result<Subject, ActionError> {
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(format!("vesta-automation-device:{}", principal.as_str()))
        );
        let now = self.db.clock().millis();
        let server = self.db.server_id().to_owned();
        let device = self
            .db
            .write(|c| {
                if let Some((id, revoked)) = c
                    .query_row(
                        "SELECT id, revoked_at FROM devices WHERE server_id=? AND public_key_sha256=?",
                        params![server, fingerprint],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)),
                    )
                    .optional()?
                {
                    return Ok(revoked.is_none().then_some(id));
                }
                let active: Option<i32> = c
                    .query_row(
                        "SELECT 1 FROM principals WHERE id=? AND server_id=? AND status='active'",
                        params![principal.as_str(), server],
                        |r| r.get(0),
                    )
                    .optional()?;
                if active.is_none() {
                    return Ok(None);
                }
                let id = uuid::Uuid::new_v4().to_string();
                c.execute(
                    "INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,?,?,?)",
                    params![id, server, principal.as_str(), AUTOMATION_DEVICE_LABEL, fingerprint, now],
                )?;
                Ok(Some(id))
            })
            .map_err(|_| ActionError::Unavailable)?
            .ok_or(ActionError::Forbidden)?;
        Ok(Subject {
            principal: principal.clone(),
            device: DeviceId::parse(&device).map_err(|_| ActionError::Unavailable)?,
        })
    }

    /// Bounded durable temporary-ban sweep. Original actor, current automation-device policy.
    /// An unreadable ban file defers expiry; after UNREADABLE_BAN_FILE_GRACE_MS the row is
    /// blocked with a notification rather than retried silently forever.
    pub(crate) fn expire_bans(&self, directory: &std::path::Path) -> Result<(), ()> {
        let now = self.db.clock().millis();
        let rows = self.db.read(|c| {
            let mut q = c.prepare("SELECT id,principal_id,player,reason,expires_at FROM moderation_actions WHERE server_id=? AND expiry_state='pending' AND outcome='succeeded' AND expires_at<=? ORDER BY expires_at,id LIMIT 32")?;
            let rows = q.query_map(params![self.db.server_id(), now], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,i64>(4)?)))?.collect::<Result<Vec<_>,_>>()?;
            Ok(rows)
        }).map_err(|_| ())?;
        let mut entries = None;
        for (id, principal, player, reason, expires_at) in rows {
            let principal = PrincipalId::parse(&principal).map_err(|_| ())?;
            let subject = self.subject(&principal);
            let allowed = match &subject {
                Ok(subject) => self
                    .db
                    .read_with(|s| -> crate::Result<bool> {
                        Ok(matches!(
                            crate::identity::policy_in(
                                s.connection(),
                                self.db.server_id(),
                                now,
                                Some(subject)
                            )?
                            .check(Capability::PlayersModerate, None),
                            crate::identity::Decision::Allow
                        ))
                    })
                    .map_err(|_| ())?,
                Err(ActionError::Forbidden) => false,
                Err(_) => return Err(()),
            };
            if !allowed {
                self.db.write_with(|t| -> crate::Result<()> {
                    if t.connection().execute("UPDATE moderation_actions SET expiry_state='blocked' WHERE id=? AND expiry_state='pending'", [id])? != 0 {
                        crate::automation::insert_notification(t.connection(), now, crate::automation::Severity::Warning, "Temporary ban expiry blocked", &format!("Temporary ban #{id} for {player}: the original actor no longer has moderation authority. Review the ban manually."), "moderation")?;
                    }
                    Ok(())
                }).map_err(|_| ())?;
                continue;
            }
            let snapshot = self
                .runtime()
                .map_err(|_| ())?
                .game_snapshot()
                .map_err(|_| ())?;
            if snapshot.phase != crate::operations::ProcessPhase::Running {
                continue;
            }
            let Some(generation) = snapshot.generation else {
                continue;
            };
            // The game rewrites its ban file in place, so a failed read may be a
            // half-written save: retry on later ticks instead of guessing. Only
            // persistent unreadability is surfaced, and nothing is pardoned.
            let Some(list) = entries
                .get_or_insert_with(|| crate::moderation::bans(directory).ok())
                .as_ref()
            else {
                if now.saturating_sub(expires_at) >= UNREADABLE_BAN_FILE_GRACE_MS {
                    self.db.write_with(|t| -> crate::Result<()> {
                        if t.connection().execute("UPDATE moderation_actions SET expiry_state='blocked' WHERE id=? AND expiry_state='pending'", [id])? != 0 {
                            crate::automation::insert_notification(t.connection(), now, crate::automation::Severity::Warning, "Temporary ban expiry needs review", &format!("Temporary ban #{id} for {player}: banned-players.json could not be read for an hour after expiry. No pardon was sent. Review the ban manually."), "moderation")?;
                        }
                        Ok(())
                    }).map_err(|_| ())?;
                }
                continue;
            };
            let matching = {
                list.iter().any(|v| {
                    v.name.eq_ignore_ascii_case(&player)
                        && Some(v.reason.as_str()) == reason.as_deref()
                })
            };
            if !matching {
                self.db.write_with(|t| -> crate::Result<()> {
                    if t.connection().execute("UPDATE moderation_actions SET expiry_state='superseded' WHERE id=? AND expiry_state='pending'", [id])? != 0 {
                        crate::automation::insert_notification(t.connection(), now, crate::automation::Severity::Warning, "Temporary ban expiry not confirmed", &format!("Temporary ban #{id} for {player}: the current vanilla entry is missing or changed. No pardon was sent. Review manually; ban plugins are not supported yet."), "moderation")?;
                    }
                    Ok(())
                }).map_err(|_| ())?;
                continue;
            }
            // Submission atomically tests pending intent and stores its operation id.
            // Exact binding checks again before native write, including later supersession.
            match self.runtime().map_err(|_| ())?.submit_moderation_expiry(
                &subject.map_err(|_| ())?,
                generation,
                player,
                id,
            ) {
                Ok(_) => {}
                Err(e) if matches!(e.code(), "binding_mismatch" | "forbidden") => {}
                Err(_) => return Err(()),
            }
        }
        Ok(())
    }

    fn required(action: &Action) -> Result<Option<Capability>, ActionError> {
        Ok(Some(match action {
            Action::Delay { .. } | Action::Notify { .. } => return Ok(None),
            Action::Backup { .. } => Capability::BackupsCreate,
            Action::VerifyBackup { .. } => Capability::BackupsVerify,
            Action::Start => Capability::LifecycleStart,
            Action::Stop => Capability::LifecycleStop,
            Action::Restart => Capability::LifecycleRestart,
            Action::ConsoleCommand { .. } => Capability::ConsoleWrite,
            Action::CleanTrash => Capability::FilesDelete,
            Action::ScriptPreset { .. } => return Err(ActionError::Unsupported),
        }))
    }

    fn runtime(&self) -> Result<&RuntimeHandle, ActionError> {
        self.runtime.get().ok_or(ActionError::Unavailable)
    }

    fn generation(&self) -> Result<vesta_game_runtime::process::GenerationId, ActionError> {
        self.runtime()?
            .game_snapshot()
            .map_err(|_| ActionError::Unavailable)?
            .generation
            .ok_or(ActionError::Invalid)
    }
}

impl ActionPort for HostActions {
    fn authorize(&self, principal: &PrincipalId, action: &Action) -> Result<(), ActionError> {
        let Some(capability) = Self::required(action)? else {
            return Ok(());
        };
        let subject = self.subject(principal)?;
        let allowed = self
            .db
            .read(|c| {
                Ok(crate::identity::policy_in(
                    c,
                    self.db.server_id(),
                    self.db.clock().millis(),
                    Some(&subject),
                )
                .map(|policy| {
                    matches!(
                        policy.check(capability, None),
                        crate::identity::Decision::Allow
                    )
                })
                .unwrap_or(false))
            })
            .map_err(|_| ActionError::Unavailable)?;
        if allowed {
            Ok(())
        } else {
            Err(ActionError::Forbidden)
        }
    }

    fn begin(
        &self,
        principal: &PrincipalId,
        action: &Action,
        idempotency_key: &str,
    ) -> Result<ActionHandle, ActionError> {
        self.authorize(principal, action)?;
        let subject = self.subject(principal)?;
        // Operations keys are bounded at 128 bytes; hash longer engine keys.
        let key = format!("automation:{:x}", Sha256::digest(idempotency_key));
        // Reconcile first: an operation already submitted under this key is returned
        // before any generation-bound command is rebuilt from current state.
        if matches!(
            action,
            Action::Start | Action::Stop | Action::Restart | Action::ConsoleCommand { .. }
        ) && let Some(existing) = self
            .runtime()?
            .operations_service()
            .find_key_trusted(principal, &key)
            .map_err(|_| ActionError::Unavailable)?
        {
            return Ok(ActionHandle(format!("op:{}", existing.id.as_str())));
        }
        let game = |command: GameCommand| -> Result<ActionHandle, ActionError> {
            let admission = self
                .runtime()?
                .submit_game(&subject, command, &key)
                .map_err(|error| match error.code() {
                    "forbidden" => ActionError::Forbidden,
                    "binding_mismatch" | "invalid_request" => ActionError::Invalid,
                    _ => ActionError::Unavailable,
                })?;
            let (crate::operations::Admission::Created(summary)
            | crate::operations::Admission::Existing(summary)) = admission;
            Ok(ActionHandle(format!("op:{}", summary.id.as_str())))
        };
        match action {
            Action::Backup { profile } | Action::VerifyBackup { profile } => {
                let backups = self.backups.as_ref().ok_or(ActionError::Unsupported)?;
                let kind = if matches!(action, Action::Backup { .. }) {
                    JobKind::Backup
                } else {
                    JobKind::Verify
                };
                let id = backups
                    .submit(&subject, profile, kind, Some(idempotency_key))
                    .map_err(|error| match error {
                        crate::backups::BackupError::UnknownProfile
                        | crate::backups::BackupError::Invalid
                        | crate::backups::BackupError::IdempotencyConflict => ActionError::Invalid,
                        crate::backups::BackupError::Forbidden => ActionError::Forbidden,
                        crate::backups::BackupError::Busy => ActionError::Busy,
                        _ => ActionError::Unavailable,
                    })?;
                Ok(ActionHandle(format!("backup:{id}")))
            }
            Action::Start => game(GameCommand::Start),
            Action::Stop => game(GameCommand::Stop {
                expected_generation: self.generation()?,
            }),
            Action::Restart => game(GameCommand::Restart {
                expected_generation: self.generation()?,
            }),
            Action::ConsoleCommand { line } => game(GameCommand::Console {
                expected_generation: self.generation()?,
                line: line.clone(),
            }),
            Action::CleanTrash => {
                self.purge(idempotency_key);
                Ok(ActionHandle(format!("trash:{idempotency_key}")))
            }
            Action::ScriptPreset { .. } => Err(ActionError::Unsupported),
            Action::Delay { .. } | Action::Notify { .. } => Err(ActionError::Invalid),
        }
    }

    fn poll(&self, handle: &ActionHandle) -> Result<ActionStatus, ActionError> {
        if let Some(key) = handle.0.strip_prefix("trash:") {
            let state = self
                .trash
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(key)
                .cloned();
            return Ok(match state {
                // Lost across a restart: purge again (idempotent) and keep waiting.
                None => {
                    self.purge(key);
                    ActionStatus::Running
                }
                Some(None) => ActionStatus::Running,
                Some(Some(result)) => {
                    self.trash
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(key);
                    match result {
                        Ok(purged) => ActionStatus::Succeeded {
                            output: serde_json::json!({"purged": purged}),
                        },
                        Err(()) => ActionStatus::Failed {
                            code: "trash_cleanup_failed".into(),
                        },
                    }
                }
            });
        }
        if let Some(job) = handle.0.strip_prefix("backup:") {
            let backups = self.backups.as_ref().ok_or(ActionError::Unsupported)?;
            let job = backups
                .job(job)
                .map_err(|_| ActionError::Unavailable)?
                .ok_or(ActionError::Invalid)?;
            return Ok(match job.state {
                JobState::AwaitingConfirmation | JobState::Queued | JobState::Running => {
                    ActionStatus::Running
                }
                JobState::Succeeded => ActionStatus::Succeeded {
                    output: serde_json::json!({
                        "job": job.id,
                        "snapshot_id": job.snapshot_id,
                    }),
                },
                JobState::Failed => ActionStatus::Failed {
                    code: job.error_code.unwrap_or_else(|| "backup_failed".into()),
                },
                JobState::Uncertain => ActionStatus::Uncertain {
                    code: job.error_code.unwrap_or_else(|| "backup_uncertain".into()),
                },
                // Automation never submits restores; reported for completeness.
                JobState::RecoveryRequired => ActionStatus::Failed {
                    code: job
                        .error_code
                        .unwrap_or_else(|| "backup_recovery_required".into()),
                },
            });
        }
        if let Some(id) = handle.0.strip_prefix("op:") {
            let id = OperationId::parse(id).map_err(|_| ActionError::Invalid)?;
            let summary = self
                .runtime()?
                .operations_service()
                .summary_trusted(&id)
                .map_err(|_| ActionError::Unavailable)?
                .ok_or(ActionError::Invalid)?;
            return Ok(match summary.state {
                OperationState::Succeeded => ActionStatus::Succeeded {
                    output: serde_json::json!({"operation": summary.id.as_str()}),
                },
                OperationState::Failed | OperationState::Cancelled => ActionStatus::Failed {
                    code: format!("operation_{}", summary.state.as_str()),
                },
                OperationState::RecoveryRequired => ActionStatus::Uncertain {
                    code: "operation_recovery_required".into(),
                },
                _ => ActionStatus::Running,
            });
        }
        Err(ActionError::Invalid)
    }
}
