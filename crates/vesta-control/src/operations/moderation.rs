//! Moderation history shares the operation writer; expiry never acquires console authority.
use super::*;
use crate::moderation::ModerationAction;
use rusqlite::{OptionalExtension, params};

#[derive(Serialize)]
pub struct ModerationHistory {
    pub id: i64,
    pub principal_id: String,
    pub device_id: String,
    pub operation_id: String,
    pub action: String,
    pub player: String,
    pub reason: Option<String>,
    pub duration_seconds: Option<i64>,
    pub expires_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub outcome: String,
    pub expiry_state: String,
}
impl Operations {
    pub(crate) fn whitelist_receipt(
        &self,
        subject: &Subject,
        id: &OperationId,
    ) -> OperationsResult<Option<OperationSummary>> {
        self.db
            .read_with(|snapshot| {
                let repo = Repository::new(
                    snapshot.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                );
                if let identity::Decision::Deny(reason) = self
                    .policy(&repo, subject)?
                    .check(Capability::PlayersModerate, None)
                {
                    return Err(OperationError::forbidden(reason));
                }
                repo.load(id)?
                    .map(|op| {
                        if op.subject != *subject || op.operation_type != "players.moderate" {
                            return Err(invalid());
                        }
                        Ok(op.summary)
                    })
                    .transpose()
            })
            .map_err(OperationError::from)
    }
    pub fn moderation_history(
        &self,
        subject: &Subject,
        after: i64,
        limit: usize,
    ) -> OperationsResult<Vec<ModerationHistory>> {
        if after < 0 || !(1..=101).contains(&limit) {
            return Err(invalid());
        }
        self.db.read_with(|snapshot| {
            let repo = Repository::new(snapshot.connection(), self.db.server_id(), self.db.clock().millis());
            if let identity::Decision::Deny(reason) = self.policy(&repo, subject)?.check(Capability::PlayersView, None) {
                return Err(OperationError::forbidden(reason));
            }
            let mut statement = repo.connection().prepare("SELECT id,principal_id,device_id,operation_id,action,player,reason,duration_seconds,expires_at,created_at,outcome,expiry_state FROM moderation_actions WHERE server_id=? AND id>? ORDER BY id LIMIT ?")?;
            let rows = statement.query_map(params![self.db.server_id(), after, limit as i64], |r| Ok(ModerationHistory {
                id:r.get(0)?, principal_id:r.get(1)?, device_id:r.get(2)?, operation_id:r.get(3)?, action:r.get(4)?, player:r.get(5)?, reason:r.get(6)?, duration_seconds:r.get(7)?, expires_at_ms:r.get(8)?, created_at_ms:r.get(9)?, outcome:r.get(10)?, expiry_state:r.get(11)?,
            }))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        }).map_err(OperationError::from)
    }
}

pub(super) fn insert(
    repo: &Repository<'_>,
    subject: &Subject,
    op: &StoredOperation,
    action: &ModerationAction,
    expiry: Option<i64>,
) -> OperationsResult<()> {
    let c = repo.connection();
    if let Some(id) = expiry {
        let valid: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM moderation_actions WHERE id=? AND server_id=? AND principal_id=? AND player=? COLLATE NOCASE AND expiry_state='pending' AND outcome='succeeded' AND expires_at<=?)", params![id, repo.server, subject.principal.as_str(), action.player(), repo.now], |r| r.get(0))?;
        if !valid || !matches!(action, ModerationAction::Pardon { .. }) {
            return Err(OperationError::rejected(
                OperationErrorCode::BindingMismatch,
            ));
        }
        c.execute("UPDATE moderation_actions SET expiry_state='submitted',expiry_operation_id=? WHERE id=?", params![op.summary.id.as_str(), id])?;
    } else if matches!(
        action,
        ModerationAction::Ban { .. } | ModerationAction::Pardon { .. }
    ) {
        c.execute("UPDATE moderation_actions SET expiry_state='superseded' WHERE server_id=? AND player=? COLLATE NOCASE AND expiry_state IN ('pending','submitted')", params![repo.server, action.player()])?;
    }
    let duration = action.duration_seconds().map(|v| v as i64);
    let expires = duration
        .map(|v| repo.now.checked_add(v * 1000).ok_or_else(invalid))
        .transpose()?;
    c.execute("INSERT INTO moderation_actions(server_id,principal_id,device_id,operation_id,action,player,reason,duration_seconds,expires_at,created_at,outcome,expiry_state) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)", params![repo.server, subject.principal.as_str(), subject.device.as_str(), op.summary.id.as_str(), action.kind(), action.player(), action.reason().or_else(|| matches!(action, ModerationAction::Ban { .. }).then_some(crate::moderation::DEFAULT_BAN_REASON)), duration, expires, repo.now, op.summary.state.as_str(), if duration.is_some() {"pending"} else {"none"}])?;
    Ok(())
}

pub(super) fn check_expiry(repo: &Repository<'_>, op: &StoredOperation) -> OperationsResult<()> {
    let state: Option<String> = repo.connection().query_row("SELECT expiry_state FROM moderation_actions WHERE server_id=? AND expiry_operation_id=?", params![repo.server, op.summary.id.as_str()], |r| r.get(0)).optional()?;
    if state.is_some_and(|v| v != "submitted") {
        return Err(OperationError::rejected(
            OperationErrorCode::BindingMismatch,
        ));
    }
    Ok(())
}
pub(super) fn outcome(repo: &Repository<'_>, op: &StoredOperation) -> OperationsResult<()> {
    if op.operation_type != "players.moderate" {
        return Ok(());
    }
    let state = op.summary.state;
    // Duration starts at acknowledged delivery, not time spent waiting behind a lock.
    repo.connection().execute(
        "UPDATE moderation_actions SET expires_at=CASE WHEN outcome<>'succeeded' AND ?='succeeded' AND duration_seconds IS NOT NULL THEN ?+duration_seconds*1000 ELSE expires_at END,outcome=? WHERE server_id=? AND operation_id=?",
        params![state.as_str(), repo.now, state.as_str(), repo.server, op.summary.id.as_str()],
    )?;
    if state.is_terminal() || state == OperationState::RecoveryRequired {
        // An uncertain native write is never blindly repeated after a restart.
        let expiry = if state == OperationState::Succeeded {
            "completed"
        } else if !op.effect_started && state != OperationState::RecoveryRequired {
            "pending"
        } else {
            "uncertain"
        };
        repo.connection().execute("UPDATE moderation_actions SET expiry_state=?,expiry_operation_id=CASE WHEN ?='pending' THEN NULL ELSE expiry_operation_id END WHERE server_id=? AND expiry_operation_id=? AND expiry_state='submitted'", params![expiry, expiry, repo.server, op.summary.id.as_str()])?;
    }
    Ok(())
}
