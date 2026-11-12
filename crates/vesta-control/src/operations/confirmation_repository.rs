//! Confirmation SQL over one caller-owned configured transaction. No transaction
//! management, authority injection, raw token storage or unbounded row projection.
use super::{
    OperationsResult, SourceRevision,
    confirmation::MAX_CONFIRMATION_TTL_MS,
    domain::{OperationError, OperationErrorCode, corrupt},
    repository::StoredOperation,
};
#[cfg(test)]
use rusqlite::OptionalExtension;
use rusqlite::{Connection, params};

/// A private ticket for THIS transaction's successful burn. It is not an accepted
/// receipt and cannot authorize execution. It is intentionally not Clone/Debug.
pub(super) struct BurnTicket {
    row_id: i64,
    burned_at_ms: i64,
}

pub(super) struct ConfirmationRepository<'a> {
    connection: &'a Connection,
    server: &'a str,
    now: i64,
}

impl<'a> ConfirmationRepository<'a> {
    pub(super) fn new(connection: &'a Connection, server: &'a str, now: i64) -> Self {
        Self {
            connection,
            server,
            now,
        }
    }

    /// This mutation precedes ALL operation/actor/policy/revision refusal paths.
    /// Acceptance or clean refusal audit commits in this same outer transaction.
    /// Expired/foreign-operation tokens are also burned; accepted/replayed tokens
    /// are never reset. A refusal cannot create an accepted_at value.
    pub(super) fn burn(&self, token_hash: &str) -> OperationsResult<Option<BurnTicket>> {
        let changed = self.connection.execute(
            "UPDATE confirmations SET consumed_at=?1 WHERE server_id=?2 AND token_hash=?3 \
             AND consumed_at IS NULL AND accepted_at IS NULL",
            params![self.now, self.server, token_hash],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        if changed != 1 {
            return Err(corrupt());
        }
        let row_id: i64 = self.connection.query_row(
            "SELECT rowid FROM confirmations WHERE server_id=?1 AND token_hash=?2 \
             AND consumed_at=?3 AND accepted_at IS NULL",
            params![self.server, token_hash, self.now],
            |row| row.get(0),
        )?;
        Ok(Some(BurnTicket {
            row_id,
            burned_at_ms: self.now,
        }))
    }

    pub(super) fn issue(
        &self,
        operation: &StoredOperation,
        revision: &SourceRevision,
        token_hash: &str,
        expires_at_ms: i64,
    ) -> OperationsResult<String> {
        // An accepted receipt paired with an awaiting operation is an inconsistent
        // state, not an invitation to mint another destructive confirmation.
        let accepted: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM confirmations WHERE server_id=?1 \
             AND operation_id=?2 AND accepted_at IS NOT NULL)",
            params![self.server, operation.summary.id.as_str()],
            |row| row.get(0),
        )?;
        if accepted {
            return Err(rejected());
        }
        // Reissue supersedes previous live tokens, including expired tokens.
        // accept() and receipt verification additionally require the latest issued
        // row; no historical accepted binding can survive a newer issuance.
        self.connection.execute(
            "UPDATE confirmations SET consumed_at=?1 WHERE server_id=?2 AND operation_id=?3 \
             AND consumed_at IS NULL AND accepted_at IS NULL",
            params![self.now, self.server, operation.summary.id.as_str()],
        )?;
        let mut random_id = zeroize::Zeroizing::new([0_u8; 16]);
        getrandom::fill(&mut *random_id)
            .map_err(|_| vesta_state::StateError::new("state_random_unavailable"))?;
        let id = uuid::Uuid::from_bytes(*random_id).to_string();
        self.connection.execute(
            "INSERT INTO confirmations (id,server_id,operation_id,actor_principal_id,device_id,action,\
             target,payload_hash,source_revision,token_hash,created_at,expires_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![id, self.server, operation.summary.id.as_str(),
                operation.subject.principal.as_str(), operation.subject.device.as_str(),
                operation.operation_type, operation.target, operation.payload_hash, revision.as_str(),
                token_hash, self.now, expires_at_ms],
        )?;
        Ok(id)
    }

    /// All ordinary mismatch/expiry refusals happen before any acceptance mutation.
    pub(super) fn validate_ticket(
        &self,
        ticket: &BurnTicket,
        operation: &StoredOperation,
        revision: &SourceRevision,
    ) -> OperationsResult<()> {
        let exact: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM confirmations WHERE rowid=?1 AND server_id=?2 \
             AND operation_id=?3 AND actor_principal_id=?4 AND device_id=?5 AND action=?6 \
             AND target=?7 AND payload_hash=?8 AND source_revision=?9 AND consumed_at=?10 \
             AND consumed_at=?11 AND accepted_at IS NULL AND created_at<=consumed_at \
             AND expires_at>?11 AND expires_at>created_at AND expires_at-created_at<=?12 \
             AND NOT EXISTS (SELECT 1 FROM confirmations newer WHERE newer.server_id=?2 \
             AND newer.operation_id=?3 AND newer.rowid>?1))",
            params![
                ticket.row_id,
                self.server,
                operation.summary.id.as_str(),
                operation.subject.principal.as_str(),
                operation.subject.device.as_str(),
                operation.operation_type,
                operation.target,
                operation.payload_hash,
                revision.as_str(),
                ticket.burned_at_ms,
                self.now,
                MAX_CONFIRMATION_TTL_MS
            ],
            |row| row.get(0),
        )?;
        if !exact {
            return Err(rejected());
        }
        Ok(())
    }

    /// Stamp a receipt only after the caller verifies the ticket, canonical binding
    /// and current policy against the current native published revision. The same
    /// transaction must queue and append success audit/event or roll everything
    /// back. Any unexpected mutation mismatch is infrastructure, never a committed
    /// ordinary refusal after a partial acceptance mutation.
    pub(super) fn accept(
        &self,
        ticket: &BurnTicket,
        operation: &StoredOperation,
        revision: &SourceRevision,
    ) -> OperationsResult<()> {
        let changed = self.connection.execute(
            "UPDATE confirmations SET accepted_at=?1 WHERE rowid=?2 AND server_id=?3 \
             AND operation_id=?4 AND actor_principal_id=?5 AND device_id=?6 AND action=?7 \
             AND target=?8 AND payload_hash=?9 AND source_revision=?10 \
             AND consumed_at=?11 AND consumed_at=?1 AND accepted_at IS NULL \
             AND created_at<=consumed_at AND expires_at>?1 AND expires_at>created_at \
             AND expires_at-created_at<=?12 AND NOT EXISTS (SELECT 1 FROM confirmations newer \
             WHERE newer.server_id=?3 AND newer.operation_id=?4 AND newer.rowid>?2)",
            params![
                self.now,
                ticket.row_id,
                self.server,
                operation.summary.id.as_str(),
                operation.subject.principal.as_str(),
                operation.subject.device.as_str(),
                operation.operation_type,
                operation.target,
                operation.payload_hash,
                revision.as_str(),
                ticket.burned_at_ms,
                MAX_CONFIRMATION_TTL_MS
            ],
        )?;
        if changed != 1 {
            return Err(corrupt());
        }
        Ok(())
    }

    pub(super) fn queue(&self, operation: &StoredOperation) -> OperationsResult<()> {
        let changed = self.connection.execute(
            "UPDATE operations SET state='queued',updated_at=?1 WHERE id=?2 AND server_id=?3 \
             AND state='awaiting_confirmation' AND requires_confirmation=1 \
             AND owner_token IS NULL AND external_effect_started=0 AND cancel_requested=0",
            params![self.now, operation.summary.id.as_str(), self.server],
        )?;
        if changed != 1 {
            return Err(corrupt());
        }
        Ok(())
    }

    /// Token TTL bounds the opportunity to ACCEPT, not how long a legitimately
    /// accepted queued operation may wait. Run/effect still require current policy
    /// and exact native revision. Every stored timestamp is checked, including
    /// rejection of clock rollback before acceptance, and ambiguous receipts fail.
    pub(super) fn verify_receipt(
        &self,
        operation: &StoredOperation,
        revision: &SourceRevision,
    ) -> OperationsResult<()> {
        let (accepted_count, exact_count): (i64, i64) = self.connection.query_row(
            "SELECT count(*),coalesce(sum(CASE WHEN actor_principal_id=?3 AND device_id=?4 \
             AND action=?5 AND target=?6 AND payload_hash=?7 AND source_revision=?8 \
             AND consumed_at IS NOT NULL AND created_at<=consumed_at \
             AND consumed_at=accepted_at AND accepted_at<expires_at AND accepted_at<=?9 \
             AND expires_at>created_at AND expires_at-created_at<=?10 \
             AND NOT EXISTS (SELECT 1 FROM confirmations newer WHERE newer.server_id=?1 \
             AND newer.operation_id=?2 AND newer.rowid>confirmations.rowid) \
             THEN 1 ELSE 0 END),0) FROM confirmations WHERE server_id=?1 \
             AND operation_id=?2 AND accepted_at IS NOT NULL",
            params![
                self.server,
                operation.summary.id.as_str(),
                operation.subject.principal.as_str(),
                operation.subject.device.as_str(),
                operation.operation_type,
                operation.target,
                operation.payload_hash,
                revision.as_str(),
                self.now,
                MAX_CONFIRMATION_TTL_MS
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if accepted_count != 1 || exact_count != 1 {
            return Err(rejected());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn stored_times(
        &self,
        token_hash: &str,
    ) -> OperationsResult<Option<(Option<i64>, Option<i64>)>> {
        Ok(self.connection.query_row(
            "SELECT consumed_at,accepted_at FROM confirmations WHERE server_id=?1 AND token_hash=?2",
            params![self.server, token_hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?)
    }
}

pub(super) fn rejected() -> OperationError {
    OperationError::rejected(OperationErrorCode::ConfirmationRejected)
}
