//! Current-authority Activity projection. Never serialize canonical input/results directly.
use super::{domain::invalid, input, repository::InputStatus, *};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ActivityActor {
    Principal {
        principal_id: String,
        display_name: String,
        device_name: String,
    },
    Automation {
        kind: &'static str,
        name: &'static str,
    },
}

/// Resolve labels only through server-local identity rows. Never echo arbitrary audit sources.
pub(super) fn actor(
    c: &Connection,
    server: &str,
    principal: &str,
    device: &str,
    source: &str,
) -> OperationsResult<ActivityActor> {
    let automation: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM devices WHERE server_id=? AND principal_id=? AND id=? AND public_key_sha256=?)",params![server,principal,device,format!("{:x}",Sha256::digest(format!("vesta-automation-device:{principal}")))],|r| r.get(0))?;
    if automation {
        return Ok(ActivityActor::Automation {
            kind: "workflow",
            name: "Vesta automation",
        });
    }
    let label = c.query_row(
        "SELECT p.id, substr(COALESCE(n.name,p.display_name),1,128), substr(d.label,1,128) FROM principals p JOIN devices d ON d.principal_id=p.id AND d.server_id=p.server_id LEFT JOIN user_minecraft_names n ON n.principal_id=p.id WHERE p.server_id=? AND p.id=? AND d.id=?",
        params![server,principal,device], |r| Ok(ActivityActor::Principal { principal_id:r.get(0)?,display_name:r.get(1)?,device_name:r.get(2)? }),
    ).optional()?;
    Ok(label.unwrap_or(ActivityActor::Automation {
        kind: if source == "automation" {
            "workflow"
        } else {
            "system"
        },
        name: if source == "automation" {
            "Workflow engine"
        } else {
            "Vesta agent"
        },
    }))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ActivitySummary {
    Console {
        command: String,
    },
    ConsoleRedacted {
        command_redacted: bool,
    },
    Moderation {
        action: &'static str,
        player: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_seconds: Option<u64>,
    },
    ModerationRedacted {
        details_redacted: bool,
    },
    Lifecycle {
        target_generation: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        result_generation: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        confirmed: Option<bool>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OperationActivity {
    pub kind: &'static str,
    pub actor: ActivityActor,
    pub summary: Option<ActivitySummary>,
    pub summary_purged: bool,
    pub outcome: Option<&'static str>,
}

const KIND_SQL: &str = "CASE type WHEN 'game.start' THEN 'lifecycle.start' WHEN 'game.stop' THEN 'lifecycle.stop' WHEN 'game.sleep' THEN 'lifecycle.sleep' WHEN 'game.restart' THEN 'lifecycle.restart' WHEN 'game.force_kill' THEN 'lifecycle.kill' WHEN 'game.console' THEN 'console.command' WHEN 'players.moderate' THEN 'players.moderate' ELSE 'other' END";
pub fn valid_activity_kind(kind: &str) -> bool {
    matches!(
        kind,
        "lifecycle.start"
            | "lifecycle.stop"
            | "lifecycle.sleep"
            | "lifecycle.restart"
            | "lifecycle.kill"
            | "console.command"
            | "players.moderate"
            | "other"
    )
}
fn kind(raw: &str) -> &'static str {
    match raw {
        "game.start" => "lifecycle.start",
        "game.stop" => "lifecycle.stop",
        "game.sleep" => "lifecycle.sleep",
        "game.restart" => "lifecycle.restart",
        "game.force_kill" => "lifecycle.kill",
        "game.console" => "console.command",
        "players.moderate" => "players.moderate",
        _ => "other",
    }
}

impl Operations {
    /// Bounded exact-ID observation in one current-authority snapshot. Missing
    /// records stay missing; no operation is created, retried or inferred done.
    pub fn activity_get_many(
        &self,
        subject: &Subject,
        ids: &[OperationId],
    ) -> OperationsResult<Vec<(OperationSummary, OperationActivity)>> {
        if ids.is_empty()
            || ids.len() > 32
            || ids
                .iter()
                .map(|id| id.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != ids.len()
        {
            return Err(invalid());
        }
        self.db
            .read_with(|snapshot| {
                let repo = Repository::new(
                    snapshot.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                );
                let policy = self.activity_policy(&repo, subject)?;
                let mut items = Vec::with_capacity(ids.len());
                for id in ids {
                    if let Some(operation) = repo.load(id)? {
                        items.push(Self::activity_in(&repo, subject, &policy, operation)?);
                    }
                }
                Ok(items)
            })
            .map_err(OperationError::from)
    }
    pub fn activity_get(
        &self,
        subject: &Subject,
        id: &OperationId,
    ) -> OperationsResult<Option<(OperationSummary, OperationActivity)>> {
        self.db
            .read_with(|snapshot| {
                let repo = Repository::new(
                    snapshot.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                );
                let policy = self.activity_policy(&repo, subject)?;
                repo.load(id)?
                    .map(|op| Self::activity_in(&repo, subject, &policy, op))
                    .transpose()
            })
            .map_err(OperationError::from)
    }

    /// Filtering happens before LIMIT, in the same snapshot as current capability checks.
    pub fn activity_list(
        &self,
        subject: &Subject,
        after: i64,
        limit: usize,
        filter_kind: Option<&str>,
        actor_me: bool,
    ) -> OperationsResult<Vec<(OperationSummary, OperationActivity)>> {
        if after < 0
            || !(1..=MAX_PAGE).contains(&limit)
            || filter_kind.is_some_and(|k| !valid_activity_kind(k))
        {
            return Err(invalid());
        }
        self.db.read_with(|snapshot| {
            let repo = Repository::new(snapshot.connection(), self.db.server_id(), self.db.clock().millis());
            let policy = self.activity_policy(&repo,subject)?;
            let sql = format!("SELECT {} FROM operations WHERE server_id=?1 AND seq>?2 AND (?3 IS NULL OR {KIND_SQL}=?3) AND (?4=0 OR actor_principal_id=?5) ORDER BY seq LIMIT ?6",repository::bounded("id",64));
            let mut statement = repo.connection().prepare(&sql)?;
            let ids = statement.query_map(params![repo.server,after,filter_kind,actor_me,subject.principal.as_str(),limit as i64], |r| r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            ids.into_iter().map(|id| Self::activity_in(&repo,subject,&policy,repo.require(&OperationId::parse(&id)?)?)).collect()
        }).map_err(OperationError::from)
    }

    fn activity_policy(
        &self,
        repo: &Repository<'_>,
        subject: &Subject,
    ) -> OperationsResult<PolicySnapshot> {
        let policy = self.policy(repo, subject)?;
        if let identity::Decision::Deny(reason) = policy.check(Capability::TasksView, None) {
            return Err(OperationError::forbidden(reason));
        }
        Ok(policy)
    }

    fn activity_in(
        repo: &Repository<'_>,
        subject: &Subject,
        policy: &PolicySnapshot,
        op: StoredOperation,
    ) -> OperationsResult<(OperationSummary, OperationActivity)> {
        let allowed = |cap| matches!(policy.check(cap, None), identity::Decision::Allow);
        let actor = actor(
            repo.connection(),
            repo.server,
            op.subject.principal.as_str(),
            op.subject.device.as_str(),
            &op.source,
        )?;
        let mut summary = None;
        if let Some(raw) = repo.input(&op)?
            && input::digest(&raw) == op.payload_hash
            && let Ok(plan) = input::derive(&op.operation_type, &raw)
            && plan.canonical_input == raw
            && let Some(command) = plan.command
        {
            summary = Some(match command {
                GameCommand::Console { line, .. } => {
                    if allowed(Capability::ConsoleRead) || subject.principal == op.subject.principal
                    {
                        ActivitySummary::Console { command: line }
                    } else {
                        ActivitySummary::ConsoleRedacted {
                            command_redacted: true,
                        }
                    }
                }
                GameCommand::Moderate { action, .. } => {
                    if allowed(Capability::PlayersView) {
                        ActivitySummary::Moderation {
                            action: action.kind(),
                            player: action.player().into(),
                            reason: action.reason().map(str::to_owned),
                            duration_seconds: action.duration_seconds(),
                        }
                    } else {
                        ActivitySummary::ModerationRedacted {
                            details_redacted: true,
                        }
                    }
                }
                command => {
                    let target_generation = match command {
                        GameCommand::Start => None,
                        GameCommand::Stop {
                            expected_generation,
                        }
                        | GameCommand::Restart {
                            expected_generation,
                        }
                        | GameCommand::ForceKill {
                            expected_generation,
                        }
                        | GameCommand::Sleep {
                            expected_generation,
                            ..
                        } => Some(expected_generation.to_hex()),
                        _ => unreachable!(),
                    };
                    let result_generation = if op.summary.state == OperationState::Succeeded {
                        let raw: Option<String> = repo.connection().query_row("SELECT CASE WHEN length(CAST(result_json AS BLOB))<=16384 THEN result_json END FROM operations WHERE server_id=? AND id=?",params![repo.server,op.summary.id.as_str()],|r| r.get(0))?;
                        raw.and_then(|v| serde_json::from_str::<serde_json::Value>(&v).ok())
                            .and_then(|v| {
                                v.get("generation")
                                    .and_then(|g| g.as_str())
                                    .and_then(|g| GenerationId::parse_hex(g).ok())
                                    .map(GenerationId::to_hex)
                            })
                    } else {
                        None
                    };
                    let confirmed = if op.operation_type == "game.force_kill" {
                        Some(repo.connection().query_row("SELECT EXISTS(SELECT 1 FROM confirmations WHERE server_id=? AND operation_id=? AND consumed_at IS NOT NULL)",params![repo.server,op.summary.id.as_str()],|r|r.get(0))?)
                    } else {
                        None
                    };
                    ActivitySummary::Lifecycle {
                        target_generation,
                        result_generation,
                        confirmed,
                    }
                }
            });
        }
        let error: Option<String> = repo.connection().query_row("SELECT CASE WHEN length(error_code)<=64 THEN error_code END FROM operations WHERE server_id=? AND id=?",params![repo.server,op.summary.id.as_str()],|r| r.get(0))?;
        let outcome = terminal_outcome(&op, error.as_deref());
        let activity = OperationActivity {
            kind: kind(&op.operation_type),
            actor,
            summary,
            summary_purged: op.input_status == InputStatus::Purged,
            outcome,
        };
        Ok((op.summary, activity))
    }
}

fn terminal_outcome(op: &StoredOperation, error: Option<&str>) -> Option<&'static str> {
    Some(match op.summary.state {
        OperationState::Succeeded => match op.operation_type.as_str() {
            "game.console" | "players.moderate" => "stdin_written",
            "game.stop" | "game.sleep" | "game.force_kill" => "process_exited",
            "game.start" | "game.restart" => "process_started",
            _ => "succeeded",
        },
        OperationState::RecoveryRequired => "recovery_required",
        OperationState::Cancelled => "cancelled",
        OperationState::Failed => match error {
            Some("game_stop_timeout") => "timeout",
            Some(
                "source_revision_unavailable"
                | "game_generation_unavailable"
                | "game_stale_generation",
            ) => "stale_generation",
            Some("game_stdin_unavailable" | "game_write_failed") => "write_failed",
            Some("forbidden") => "forbidden",
            Some("input_unavailable") => "input_unavailable",
            Some("confirmation_rejected") => "confirmation_rejected",
            Some("maintenance_active") => "maintenance_active",
            _ => "failed",
        },
        _ => return None,
    })
}
