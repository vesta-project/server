//! Synchronous properties approvals reuse the existing one-time receipt repository.
//! These operations are never queued into the game effect runner. Restart invalidates
//! incomplete approval operations through the existing unknown-binding recovery rule.
use super::{
    ConfirmationToken, IssuedConfirmation, OperationError, OperationErrorCode, OperationState,
    Repository, SourceRevision, confirmation_repository::ConfirmationRepository,
    input::PreparedOperation,
};
use crate::{
    files::FileError,
    identity::{self, Capability, Subject},
};
use rusqlite::{Connection, params};
use vesta_state::{
    StateDatabase,
    audit::{AuditActor, AuditLog, AuditRecord},
};

#[derive(Clone)]
pub(crate) struct PropertiesApproval {
    pub(crate) token_hash: String,
    pub(crate) payload_hash: String,
    pub(crate) expected_sha256: String,
}
fn mapped(error: OperationError) -> FileError {
    match error {
        OperationError::Rejected {
            code: OperationErrorCode::IdempotencyConflict,
            ..
        } => FileError::IdempotencyConflict,
        OperationError::Rejected { .. } => FileError::ConfirmationInvalid,
        _ => FileError::Unavailable,
    }
}
fn authorize(c: &Connection, server: &str, now: i64, subject: &Subject) -> Result<(), FileError> {
    let policy = identity::policy_in(c, server, now, Some(subject))?;
    if !policy.check(Capability::ConfigEdit, None).allowed() {
        return Err(crate::ControlError::forbidden(identity::DenyReason::NotGranted).into());
    }
    let resource = identity::Resource::parse("server.properties")?;
    for capability in [Capability::FilesRead, Capability::FilesOverwrite] {
        if !policy.check(capability, Some(&resource)).allowed() {
            return Err(crate::ControlError::forbidden(identity::DenyReason::NotGranted).into());
        }
    }
    Ok(())
}
fn record(subject: &Subject, action: &str, id: &str) -> Result<AuditRecord, FileError> {
    Ok(AuditRecord::new(
        AuditActor::new(
            subject.principal.as_str().into(),
            subject.device.as_str().into(),
            "config".into(),
        )?,
        action.into(),
        "server-properties".into(),
        "accepted".into(),
        Some(id.into()),
        [],
    )?)
}
pub(crate) fn prepare_properties_confirmation(
    db: &StateDatabase,
    subject: &Subject,
    payload_hash: &str,
    expected: &str,
    key: &str,
) -> Result<IssuedConfirmation, FileError> {
    let token = ConfirmationToken::random().map_err(mapped)?;
    let token_hash = token.hash();
    let audit = AuditLog::new(db.clone())?;
    db.write_with(|tx| {
        let c = tx.connection();
        let now = db.clock().millis();
        authorize(c,db.server_id(),now,subject)?;
        let repo = Repository::new(c,db.server_id(),now);
        let operation = match repo.find_key(&subject.principal,key).map_err(mapped)? {
            Some(op) => {
                if op.operation_type != "config.server_properties" || op.payload_hash != payload_hash || op.subject != *subject {
                    return Err(FileError::IdempotencyConflict);
                }
                if op.summary.state != OperationState::AwaitingConfirmation { return Err(FileError::ConfirmationInvalid); }
                op
            }
            None => {
                let count: i64 = c.query_row("SELECT count(*) FROM operations WHERE server_id=? AND type='config.server_properties' AND state='awaiting_confirmation'",[db.server_id()],|r| r.get(0))?;
                if count >= 1000 { return Err(FileError::IntentsFull); }
                repo.insert(subject,&PreparedOperation {
                    operation_type: "config.server_properties".into(),canonical_input: "{}".into(),payload_hash: payload_hash.into(),
                    target: "config:server-properties".into(),requirements: vec![],resources: vec![],effect_idempotent: false,requires_confirmation: true,command: None,
                },Some(key)).map_err(mapped)?
            }
        };
        let expires_at_ms = now.checked_add(120_000).ok_or(FileError::Unavailable)?;
        let confirmation_id = ConfirmationRepository::new(c,db.server_id(),now).issue(&operation,&SourceRevision::for_properties(expected),&token_hash,expires_at_ms).map_err(mapped)?;
        audit.append(c,&record(subject,"confirmation.issue",operation.summary.id.as_str())?)?;
        Ok(IssuedConfirmation { confirmation_id,operation_id: operation.summary.id,expires_at_ms,token })
    }).map_err(FileError::from)
}
/// Called inside the browser's native write transaction, immediately before its
/// effect. Receipt consumption, accepted state, effect, audit and HTTP outcome
/// either commit together or the browser compensates the uncommitted effect.
pub(crate) fn accept_properties_confirmation(
    c: &Connection,
    server: &str,
    now: i64,
    subject: &Subject,
    approval: &PropertiesApproval,
) -> Result<(), FileError> {
    authorize(c, server, now, subject)?;
    let receipts = ConfirmationRepository::new(c, server, now);
    let ticket = receipts
        .burn(&approval.token_hash)
        .map_err(mapped)?
        .ok_or(FileError::ConfirmationInvalid)?;
    let id: String = c.query_row(
        "SELECT operation_id FROM confirmations WHERE server_id=? AND token_hash=?",
        params![server, approval.token_hash],
        |r| r.get(0),
    )?;
    let id = super::OperationId::parse(&id).map_err(mapped)?;
    let op = Repository::new(c, server, now)
        .require(&id)
        .map_err(mapped)?;
    if op.subject != *subject
        || op.operation_type != "config.server_properties"
        || op.target != "config:server-properties"
        || op.payload_hash != approval.payload_hash
        || op.summary.state != OperationState::AwaitingConfirmation
    {
        return Err(FileError::ConfirmationInvalid);
    }
    let revision = SourceRevision::for_properties(&approval.expected_sha256);
    receipts
        .validate_ticket(&ticket, &op, &revision)
        .map_err(mapped)?;
    receipts.accept(&ticket, &op, &revision).map_err(mapped)?;
    c.execute("UPDATE operations SET state='succeeded',progress_percent=100,updated_at=?,finished_at=? WHERE server_id=? AND id=? AND state='awaiting_confirmation'",params![now,now,server,id.as_str()])?;
    Ok(())
}

pub(crate) fn refuse_properties_confirmation(
    db: &StateDatabase,
    subject: &Subject,
    approval: &PropertiesApproval,
) -> Result<(), FileError> {
    let audit = AuditLog::new(db.clone())?;
    db.write_with(|tx| {
        let c = tx.connection();
        ConfirmationRepository::new(c, db.server_id(), db.clock().millis())
            .burn(&approval.token_hash)
            .map_err(mapped)?;
        audit.append(
            c,
            &AuditRecord::new(
                AuditActor::new(
                    subject.principal.as_str().into(),
                    subject.device.as_str().into(),
                    "config".into(),
                )?,
                "confirmation.accept".into(),
                "server-properties".into(),
                "denied".into(),
                None,
                [],
            )?,
        )?;
        Ok::<_, FileError>(())
    })
    .map_err(FileError::from)
}

pub(crate) fn check_properties_intent(
    db: &StateDatabase,
    subject: &Subject,
    payload_hash: &str,
    key: &str,
) -> Result<(), FileError> {
    db.read_with(|snapshot| {
        let c = snapshot.connection();
        authorize(c, db.server_id(), db.clock().millis(), subject)?;
        let repo = Repository::new(c, db.server_id(), db.clock().millis());
        if let Some(op) = repo.find_key(&subject.principal, key).map_err(mapped)?
            && (op.operation_type != "config.server_properties"
                || op.payload_hash != payload_hash
                || op.subject != *subject)
        {
            return Err(FileError::IdempotencyConflict);
        }
        Ok(())
    })
    .map_err(FileError::from)
}
