//! Owned one-time confirmations through the concrete native game composition.
//! Caller-selected revisions, bindings and proof issuers are deliberately absent.
use super::{
    GameCommand, OperationError, OperationErrorCode, OperationId, OperationState, OperationSummary,
    Operations, OperationsResult, SourceRevision,
    confirmation_repository::{BurnTicket, ConfirmationRepository, rejected},
    domain::invalid,
    operation_target,
    repository::{Repository, StoredOperation},
    request_actor,
    runner::{OperationRuntime, RuntimeHandle},
};
use crate::identity::Subject;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::{fmt, time::Duration};
use vesta_state::{StateError, audit::AuditRecord};
use zeroize::Zeroizing;

pub const MAX_CONFIRMATION_TTL: Duration = Duration::from_secs(15 * 60);
pub(super) const MAX_CONFIRMATION_TTL_MS: i64 = 15 * 60 * 1000;
const MAX_TOKEN_BYTES: usize = 128;

/// Owns the caller's allocation, installing erasure before validation. No Clone,
/// Serialize, Display or implicit secret formatting. Previously copied buffers,
/// compiler temporaries, swap and crash dumps are outside this guarantee.
///
/// Issued tokens have 256 bits of OS randomness and canonical base64url encoding.
/// Input accepts bounded base64url text so unknown, incorrectly sized tokens go
/// through the same audited confirmation refusal as any other unknown token.
pub struct ConfirmationToken {
    bytes: Zeroizing<Vec<u8>>,
}
impl TryFrom<Vec<u8>> for ConfirmationToken {
    type Error = OperationError;
    fn try_from(bytes: Vec<u8>) -> OperationsResult<Self> {
        let bytes = Zeroizing::new(bytes);
        if bytes.is_empty()
            || bytes.len() > MAX_TOKEN_BYTES
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_'))
        {
            return Err(invalid());
        }
        Ok(Self { bytes })
    }
}
impl TryFrom<String> for ConfirmationToken {
    type Error = OperationError;
    fn try_from(text: String) -> OperationsResult<Self> {
        Self::try_from(text.into_bytes())
    }
}
impl ConfirmationToken {
    /// Explicit borrowed transport exposure. This never creates another allocation.
    pub fn expose_secret(&self) -> &str {
        // Construction admits only the ASCII base64url alphabet.
        std::str::from_utf8(&self.bytes).expect("validated confirmation token is ASCII")
    }
    fn random() -> OperationsResult<Self> {
        let mut raw = Zeroizing::new([0_u8; 32]);
        getrandom::fill(&mut *raw).map_err(|_| StateError::new("state_random_unavailable"))?;
        Self::try_from(URL_SAFE_NO_PAD.encode(raw.as_slice()))
    }
    fn hash(&self) -> Zeroizing<String> {
        Zeroizing::new(hex::encode(Sha256::digest(self.bytes.as_slice())))
    }
}
impl fmt::Debug for ConfirmationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ConfirmationToken(<redacted>)")
    }
}

/// Returned once; does not expose stored input, source revision or binding headers.
/// The owned token is deliberately not Clone/Serialize.
pub struct IssuedConfirmation {
    pub confirmation_id: String,
    pub operation_id: OperationId,
    pub expires_at_ms: i64,
    pub token: ConfirmationToken,
}
impl fmt::Debug for IssuedConfirmation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedConfirmation")
            .field("confirmation_id", &self.confirmation_id)
            .field("operation_id", &self.operation_id)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("token", &self.token)
            .finish()
    }
}

/// Intentional refusal is a successful transaction outcome, NOT an error smuggled
/// through a nested Result. It commits the burn and secret-free denial audit only;
/// infrastructure failures remain typed errors and roll back the entire writer.
enum ConfirmationDecision {
    Accepted(OperationSummary),
    Refused(OperationError),
}

impl RuntimeHandle {
    /// Issue from the current native published game view and current canonical
    /// policy in one admitted writer. Subject is a trusted application lookup key,
    /// not transport authentication. Reissue supersedes every earlier live token.
    pub fn issue_confirmation(
        &self,
        subject: &Subject,
        id: &OperationId,
        ttl: Duration,
    ) -> OperationsResult<IssuedConfirmation> {
        let operations = self.operations();
        let ttl_ms = match valid_ttl_ms(ttl) {
            Ok(value) => value,
            Err(error) => {
                return Err(operations.refusal(subject, "confirmation.issue", Some(id), error));
            }
        };
        let result = operations.db.write_with(|transaction| {
            let now = operations.db.clock().millis();
            let expires_at_ms = now.checked_add(ttl_ms).ok_or_else(invalid)?;
            let repo = Repository::new(transaction.connection(), operations.db.server_id(), now);
            let (operation, plan) = require_awaiting(operations, &repo, subject, id)?;
            let revision = self.source_revision()?;
            require_current_target(&plan, &revision)?;
            let token = ConfirmationToken::random()?;
            let token_hash = token.hash();
            let confirmation_id =
                ConfirmationRepository::new(repo.connection(), operations.db.server_id(), now)
                    .issue(&operation, &revision, &token_hash, expires_at_ms)?;
            operations.audit.append(
                repo.connection(),
                &AuditRecord::new(
                    request_actor(subject)?,
                    "confirmation.issue".to_owned(),
                    operation_target(id),
                    "accepted".to_owned(),
                    Some(id.as_str().to_owned()),
                    [("expires_at_ms".to_owned(), Some(expires_at_ms.to_string()))],
                )?,
            )?;
            Ok(IssuedConfirmation {
                confirmation_id,
                operation_id: id.clone(),
                expires_at_ms,
                token,
            })
        });
        // Only clean typed operation refusals, after rollback, receive an external
        // refusal audit. Infrastructure/rollback failures never become denials.
        operations.finish_request(result, subject, "confirmation.issue", Some(id))
    }

    /// Consume the owned token before ANY operation/actor/policy/revision refusal.
    /// All ordinary checks precede receipt/queue mutations. A clean refusal commits
    /// burn+denial audit atomically; success commits receipt+queue+audit+event.
    /// Failed audit/infrastructure rolls EVERYTHING back, including the burn.
    pub fn confirm(
        &self,
        subject: &Subject,
        id: &OperationId,
        token: ConfirmationToken,
    ) -> OperationsResult<OperationSummary> {
        let token_hash = token.hash();
        drop(token);
        let operations = self.operations();
        let result = operations.db.write_with(|transaction| {
            let now = operations.db.clock().millis();
            let repo = Repository::new(transaction.connection(), operations.db.server_id(), now);
            let confirmations =
                ConfirmationRepository::new(repo.connection(), operations.db.server_id(), now);
            let ticket = confirmations.burn(&token_hash)?;
            let checked =
                check_acceptance(self, &repo, &confirmations, subject, id, ticket.as_ref());
            let (operation, revision) = match checked {
                Ok(value) => value,
                Err(error @ OperationError::Rejected { .. }) => {
                    audit_refusal_in(operations, repo.connection(), subject, id, &error)?;
                    return Ok(ConfirmationDecision::Refused(error));
                }
                Err(error) => return Err(error),
            };
            // No clean refusal may be produced after this point: unexpected SQL
            // mutation mismatches are infrastructure and force whole-writer rollback.
            let ticket = ticket.as_ref().ok_or_else(super::domain::corrupt)?;
            confirmations.accept(ticket, &operation, &revision)?;
            confirmations.queue(&operation)?;
            let after = repo.require(id)?;
            operations.record_state(
                &repo,
                request_actor(subject)?,
                "confirmation.consume",
                &after,
                OperationState::AwaitingConfirmation,
            )?;
            Ok(ConfirmationDecision::Accepted(after.summary))
        });
        match result.map_err(OperationError::from)? {
            ConfirmationDecision::Accepted(summary) => Ok(summary),
            ConfirmationDecision::Refused(error) => Err(error),
        }
    }
}

fn valid_ttl_ms(ttl: Duration) -> OperationsResult<i64> {
    if ttl.is_zero() || ttl > MAX_CONFIRMATION_TTL || ttl.as_millis() == 0 {
        return Err(invalid());
    }
    i64::try_from(ttl.as_millis()).map_err(|_| invalid())
}

fn require_awaiting(
    operations: &Operations,
    repo: &Repository<'_>,
    subject: &Subject,
    id: &OperationId,
) -> OperationsResult<(StoredOperation, super::input::PreparedOperation)> {
    let operation = repo.load(id)?.ok_or_else(rejected)?;
    if operation.summary.state != OperationState::AwaitingConfirmation
        || !operation.requires_confirmation
        || operation.subject != *subject
        || operation.owner_token.is_some()
        || operation.effect_started
        || operation.summary.cancel_requested
    {
        return Err(rejected());
    }
    repo.check_locks(&operation)?;
    let plan = operations.verify_binding(repo, &operation)?;
    Ok((operation, plan))
}

fn require_current_target(
    plan: &super::input::PreparedOperation,
    revision: &SourceRevision,
) -> OperationsResult<()> {
    if let Some(GameCommand::ForceKill {
        expected_generation,
    }) = plan.command
        && *revision != SourceRevision::for_game(Some(expected_generation))
    {
        return Err(OperationError::rejected(
            OperationErrorCode::SourceRevisionUnavailable,
        ));
    }
    Ok(())
}

fn check_acceptance(
    runtime: &RuntimeHandle,
    repo: &Repository<'_>,
    confirmations: &ConfirmationRepository<'_>,
    subject: &Subject,
    id: &OperationId,
    ticket: Option<&BurnTicket>,
) -> OperationsResult<(StoredOperation, SourceRevision)> {
    let ticket = ticket.ok_or_else(rejected)?;
    let (operation, plan) = require_awaiting(runtime.operations(), repo, subject, id)?;
    let revision = runtime.source_revision()?;
    require_current_target(&plan, &revision)?;
    confirmations.validate_ticket(ticket, &operation, &revision)?;
    Ok((operation, revision))
}

fn audit_refusal_in(
    operations: &Operations,
    connection: &Connection,
    subject: &Subject,
    id: &OperationId,
    primary: &OperationError,
) -> OperationsResult<()> {
    let (code, reason) = match primary {
        OperationError::Rejected { code, reason } => (*code, *reason),
        _ => return Err(super::domain::corrupt()),
    };
    let cause = reason.map_or(code.code(), crate::identity::DenyReason::code);
    let audit = request_actor(subject).and_then(|actor| {
        let record = AuditRecord::new(
            actor,
            "confirmation.consume".to_owned(),
            operation_target(id),
            "rejected".to_owned(),
            Some(id.as_str().to_owned()),
            [("reason".to_owned(), Some(cause.to_owned()))],
        )?;
        operations.audit.append(connection, &record)?;
        Ok(())
    });
    audit.map_err(|audit_failure| OperationError::AuditUnavailable {
        primary: Box::new(primary.clone()),
        audit_failure: Box::new(audit_failure),
    })
}

/// Shared claim/run/effect receipt check. The kernel must first verify the exact
/// canonical operation and current policy; this verifies explicit acceptance,
/// complete binding and the concrete native source revision. consumed_at alone,
/// migrated rows, refused tokens and superseded historical rows are never proof.
/// Token expiry bounds acceptance, not the lifetime of an already accepted receipt.
pub(super) fn verify_receipt_in(
    connection: &Connection,
    server_id: &str,
    operation: &StoredOperation,
    revision: &SourceRevision,
    now_ms: i64,
) -> OperationsResult<()> {
    if !operation.requires_confirmation {
        return Err(rejected());
    }
    ConfirmationRepository::new(connection, server_id, now_ms).verify_receipt(operation, revision)
}

#[cfg(test)]
#[path = "confirmation_tests.rs"]
mod tests;

impl OperationRuntime {
    pub fn issue_confirmation(
        &self,
        subject: &Subject,
        id: &OperationId,
        ttl: Duration,
    ) -> OperationsResult<IssuedConfirmation> {
        self.network_handle().issue_confirmation(subject, id, ttl)
    }
    pub fn confirm(
        &self,
        subject: &Subject,
        id: &OperationId,
        token: ConfirmationToken,
    ) -> OperationsResult<OperationSummary> {
        self.network_handle().confirm(subject, id, token)
    }
}
