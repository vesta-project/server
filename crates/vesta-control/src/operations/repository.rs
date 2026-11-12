//! Private SQL boundary over ONE caller-owned configured transaction. SQL
//! projections bound every string before rusqlite materializes it; repositories
//! never open, commit or roll back transactions.
use super::domain::{
    MAX_INPUT_BYTES, MAX_LOCK_BYTES, MAX_RESOURCES, OperationError, OperationErrorCode,
    OperationId, OperationState, OperationSummary, OperationsResult, corrupt, identifier, lock_key,
    text,
};
use super::{
    domain::{MAX_PAGE, REQUEST_SOURCE},
    input::PreparedOperation,
};
use crate::identity::{DeviceId, PrincipalId, Subject};
use rusqlite::params_from_iter;
use rusqlite::{Connection, OptionalExtension, Row, params};

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum InputStatus {
    Available,
    UnavailableLegacy,
    Purged,
}

/// No Debug: a row includes the private ownership token and binding headers.
pub(super) struct StoredOperation {
    pub(super) summary: OperationSummary,
    pub(super) subject: Subject,
    pub(super) source: String,
    pub(super) operation_type: String,
    pub(super) target: String,
    pub(super) payload_hash: String,
    pub(super) idempotency_key: Option<String>,
    pub(super) effect_idempotent: bool,
    pub(super) requires_confirmation: bool,
    pub(super) effect_started: bool,
    pub(super) input_status: InputStatus,
    pub(super) owner_token: Option<String>,
}

pub(super) struct Repository<'a> {
    connection: &'a Connection,
    pub(super) server: &'a str,
    pub(super) now: i64,
}
impl<'a> Repository<'a> {
    pub(super) fn new(connection: &'a Connection, server: &'a str, now: i64) -> Self {
        Self {
            connection,
            server,
            now,
        }
    }
    pub(super) fn connection(&self) -> &Connection {
        self.connection
    }

    pub(super) fn summary(&self, id: &OperationId) -> OperationsResult<Option<OperationSummary>> {
        let sql = format!(
            "SELECT {} FROM operations WHERE id=? AND server_id=?",
            summary_columns()
        );
        let raw = self
            .connection
            .query_row(&sql, params![id.as_str(), self.server], raw_summary)
            .optional()?;
        raw.map(RawSummary::decode).transpose()
    }
    pub(super) fn summaries(
        &self,
        after: i64,
        limit: usize,
    ) -> OperationsResult<Vec<OperationSummary>> {
        let sql = format!(
            "SELECT {} FROM operations WHERE server_id=? AND seq>? ORDER BY seq LIMIT ?",
            summary_columns(),
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params![self.server, after, limit as i64], raw_summary)?;
        rows.map(|row| {
            row.map_err(OperationError::from)
                .and_then(RawSummary::decode)
        })
        .collect()
    }
    pub(super) fn load(&self, id: &OperationId) -> OperationsResult<Option<StoredOperation>> {
        let sql = format!(
            "SELECT {},{},{},{},{},{},{},{},external_effect_idempotent,requires_confirmation,\
             external_effect_started,{},{},{},{} FROM operations WHERE id=? AND server_id=?",
            summary_columns(),
            bounded("actor_principal_id", 128),
            bounded("device_id", 128),
            bounded("source", 64),
            bounded("type", 64),
            bounded("target", 512),
            bounded("payload_hash", 64),
            bounded("idempotency_key", 128),
            bounded("input_status", 24),
            bounded("owner_token", 128),
            nullable_valid("owner_token", 128),
            nullable_valid("idempotency_key", 128),
        );
        let raw = self
            .connection
            .query_row(&sql, params![id.as_str(), self.server], |row| {
                Ok(RawOperation {
                    summary: raw_summary(row)?,
                    principal: row.get(8)?,
                    device: row.get(9)?,
                    source: row.get(10)?,
                    operation_type: row.get(11)?,
                    target: row.get(12)?,
                    payload_hash: row.get(13)?,
                    idempotency_key: row.get(14)?,
                    effect_idempotent: row.get(15)?,
                    requires_confirmation: row.get(16)?,
                    effect_started: row.get(17)?,
                    input_status: row.get(18)?,
                    owner_token: row.get(19)?,
                    owner_valid: row.get(20)?,
                    key_valid: row.get(21)?,
                })
            })
            .optional()?;
        raw.map(RawOperation::decode).transpose()
    }
    pub(super) fn require(&self, id: &OperationId) -> OperationsResult<StoredOperation> {
        self.load(id)?
            .ok_or_else(|| OperationError::rejected(OperationErrorCode::NotFound))
    }
    pub(super) fn input(&self, op: &StoredOperation) -> OperationsResult<Option<String>> {
        if op.input_status != InputStatus::Available {
            return Ok(None);
        }
        let sql = format!(
            "SELECT {} FROM operations WHERE id=? AND server_id=?",
            bounded("input_json", MAX_INPUT_BYTES)
        );
        let value: Option<Option<String>> = self
            .connection
            .query_row(&sql, params![op.summary.id.as_str(), self.server], |row| {
                row.get(0)
            })
            .optional()?;
        // An available empty JSON string is not a useful typed input, and the
        // projection treats it just like oversized/NUL/missing input.
        Ok(value.flatten())
    }
    pub(super) fn resources(&self, id: &OperationId) -> OperationsResult<Vec<String>> {
        self.resource_rows(id, false)
    }
    fn held_resources(&self, id: &OperationId) -> OperationsResult<Vec<String>> {
        self.resource_rows(id, true)
    }
    fn resource_rows(&self, id: &OperationId, held: bool) -> OperationsResult<Vec<String>> {
        let table = if held {
            "resource_locks"
        } else {
            "operation_resources"
        };
        let sql = format!(
            "SELECT {} FROM {table} WHERE operation_id=? ORDER BY resource COLLATE BINARY LIMIT ?",
            bounded("resource", MAX_LOCK_BYTES),
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement
            .query_map(params![id.as_str(), (MAX_RESOURCES + 1) as i64], |row| {
                row.get::<_, Option<String>>(0)
            })?;
        let mut out = Vec::new();
        for row in rows {
            let resource = row?.ok_or_else(corrupt)?;
            if !lock_key(&resource) || out.len() == MAX_RESOURCES {
                return Err(corrupt());
            }
            out.push(resource);
        }
        Ok(out)
    }
    /// Claimed/recovery work must hold its EXACT declared resources. Unclaimed
    /// work must hold none. NULL owners are accepted only for legacy startup
    /// reconciliation, never as a worker/session mutation authorization.
    pub(super) fn check_locks(&self, op: &StoredOperation) -> OperationsResult<Vec<String>> {
        let resources = self.resources(&op.summary.id)?;
        let held = self.held_resources(&op.summary.id)?;
        if op.summary.state.is_terminal() {
            // The historical effect marker remains true after a genuine terminal
            // result. Terminal work must have released ownership/locks regardless
            // of that marker; retaining it is not ongoing worker ownership.
            if !held.is_empty() || op.owner_token.is_some() {
                return Err(corrupt());
            }
        } else if op.summary.state.is_claimed()
            || op.summary.state == OperationState::RecoveryRequired
            || op.effect_started
        {
            if held != resources {
                return Err(corrupt());
            }
        } else if !held.is_empty() || op.owner_token.is_some() {
            return Err(corrupt());
        }
        if op.summary.state == OperationState::RecoveryRequired && op.owner_token.is_some() {
            return Err(corrupt());
        }
        Ok(resources)
    }
}

impl<'a> Repository<'a> {
    pub(super) fn find_key(
        &self,
        principal: &PrincipalId,
        key: &str,
    ) -> OperationsResult<Option<StoredOperation>> {
        let sql = format!(
            "SELECT {} FROM operations WHERE server_id=? AND actor_principal_id=? AND idempotency_key=?",
            bounded("id", 64),
        );
        let raw: Option<Option<String>> = self
            .connection
            .query_row(&sql, params![self.server, principal.as_str(), key], |row| {
                row.get(0)
            })
            .optional()?;
        match raw {
            None => Ok(None),
            Some(value) => {
                let id = stored_id(value)?;
                Ok(Some(self.require(&id)?))
            }
        }
    }
    pub(super) fn insert(
        &self,
        subject: &Subject,
        plan: &PreparedOperation,
        key: Option<&str>,
    ) -> OperationsResult<StoredOperation> {
        let id = OperationId::fresh();
        let state = if plan.requires_confirmation {
            OperationState::AwaitingConfirmation
        } else {
            OperationState::Queued
        };
        self.connection.execute(
            "INSERT INTO operations (id,server_id,actor_principal_id,device_id,source,type,target,payload_hash,\
             idempotency_key,state,requires_confirmation,external_effect_idempotent,created_at,updated_at,input_json,input_status) \
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'available')",
            params![id.as_str(), self.server, subject.principal.as_str(), subject.device.as_str(),
                REQUEST_SOURCE, plan.operation_type, plan.target, plan.payload_hash, key, state.as_str(),
                plan.requires_confirmation, plan.effect_idempotent, self.now, self.now, plan.canonical_input],
        )?;
        for resource in &plan.resources {
            self.connection.execute(
                "INSERT INTO operation_resources (operation_id,resource) VALUES (?,?)",
                params![id.as_str(), resource],
            )?;
        }
        self.require(&id)
    }
    /// Keyset pages bound memory, not total queue latency. The caller keeps walking
    /// inside the SAME immediate transaction; restarting at the first page would
    /// starve independent work behind many blocked rows.
    pub(super) fn queued_page(&self, after: i64) -> OperationsResult<Vec<(OperationId, i64)>> {
        self.ids_page(after, "state='queued'")
    }
    pub(super) fn nonterminal_page(&self, after: i64) -> OperationsResult<Vec<(OperationId, i64)>> {
        self.ids_page(after, "state NOT IN ('succeeded','failed','cancelled')")
    }
    fn ids_page(
        &self,
        after: i64,
        predicate: &'static str,
    ) -> OperationsResult<Vec<(OperationId, i64)>> {
        let sql = format!(
            "SELECT {},seq FROM operations WHERE server_id=? AND seq>? AND {predicate} ORDER BY seq LIMIT ?",
            bounded("id", 64),
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params![self.server, after, MAX_PAGE as i64], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, seq) = row?;
            if seq <= after || seq <= 0 {
                return Err(corrupt());
            }
            out.push((stored_id(id)?, seq));
        }
        Ok(out)
    }
    pub(super) fn conflicting(&self, seq: i64, resources: &[String]) -> OperationsResult<bool> {
        if resources.is_empty() {
            return Ok(false);
        }
        if resources.len() > MAX_RESOURCES {
            return Err(corrupt());
        }
        let marks = vec!["?"; resources.len()].join(",");
        let sql =
            format!("SELECT EXISTS(SELECT 1 FROM resource_locks WHERE resource IN ({marks}))");
        let held: bool =
            self.connection
                .query_row(&sql, params_from_iter(resources.iter()), |row| row.get(0))?;
        if held {
            return Ok(true);
        }
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM operations o JOIN operation_resources r ON r.operation_id=o.id \
             WHERE o.server_id=? AND o.state='queued' AND o.seq<? AND r.resource IN ({marks}))",
        );
        let mut parameters: Vec<rusqlite::types::Value> =
            vec![self.server.to_owned().into(), seq.into()];
        parameters.extend(resources.iter().cloned().map(rusqlite::types::Value::Text));
        Ok(self
            .connection
            .query_row(&sql, params_from_iter(parameters), |row| row.get(0))?)
    }
    pub(super) fn claim(
        &self,
        op: &StoredOperation,
        resources: &[String],
        token: &str,
    ) -> OperationsResult<StoredOperation> {
        if op.summary.state != OperationState::Queued
            || op.owner_token.is_some()
            || op.effect_started
            || op.summary.cancel_requested
        {
            return Err(corrupt());
        }
        for resource in resources {
            self.connection.execute(
                "INSERT INTO resource_locks (resource,operation_id,acquired_at) VALUES (?,?,?)",
                params![resource, op.summary.id.as_str(), self.now],
            )?;
        }
        self.changed(self.connection.execute(
            "UPDATE operations SET state='validating',owner_token=?,updated_at=? \
             WHERE id=? AND server_id=? AND state='queued' AND owner_token IS NULL \
             AND external_effect_started=0 AND cancel_requested=0",
            params![token, self.now, op.summary.id.as_str(), self.server],
        )?)?;
        self.require(&op.summary.id)
    }
}

impl<'a> Repository<'a> {
    pub(super) fn request_cancel(&self, op: &StoredOperation) -> OperationsResult<StoredOperation> {
        if !matches!(
            op.summary.state,
            OperationState::Validating | OperationState::Running
        ) || op.owner_token.is_none()
            || op.summary.cancel_requested
        {
            return Err(corrupt());
        }
        self.changed(self.connection.execute(
            "UPDATE operations SET state='cancelling',cancel_requested=1,updated_at=? \
             WHERE id=? AND server_id=? AND state=? AND owner_token IS ?",
            params![
                self.now,
                op.summary.id.as_str(),
                self.server,
                op.summary.state.as_str(),
                op.owner_token
            ],
        )?)?;
        self.require(&op.summary.id)
    }
    pub(super) fn cancel_unclaimed(
        &self,
        op: &StoredOperation,
    ) -> OperationsResult<StoredOperation> {
        if !matches!(
            op.summary.state,
            OperationState::Queued | OperationState::AwaitingConfirmation
        ) || op.owner_token.is_some()
            || op.effect_started
        {
            return Err(corrupt());
        }
        self.connection.execute(
            "UPDATE operations SET cancel_requested=1 WHERE id=? AND server_id=?",
            params![op.summary.id.as_str(), self.server],
        )?;
        self.finish_unstarted(op, OperationState::Cancelled, None)
    }
    /// Only trusted OS-owned restart reconciliation or an exact private claim may
    /// call this. The predicate checks the loaded owner, including NULL solely for
    /// retained legacy rows; there is no caller-facing NULL-owner overload.
    pub(super) fn finish_unstarted(
        &self,
        op: &StoredOperation,
        terminal: OperationState,
        code: Option<&'static str>,
    ) -> OperationsResult<StoredOperation> {
        if !terminal.is_terminal() || op.effect_started {
            return Err(corrupt());
        }
        self.changed(self.connection.execute(
            "UPDATE operations SET state=?,finished_at=?,updated_at=?,owner_token=NULL,error_code=?,error_message=NULL \
             WHERE id=? AND server_id=? AND state=? AND owner_token IS ? AND external_effect_started=0",
            params![terminal.as_str(), self.now, self.now, code, op.summary.id.as_str(), self.server,
                op.summary.state.as_str(), op.owner_token],
        )?)?;
        self.delete_locks(&op.summary.id)?;
        self.require(&op.summary.id)
    }
}

impl<'a> Repository<'a> {
    pub(super) fn requeue_unstarted(
        &self,
        op: &StoredOperation,
    ) -> OperationsResult<StoredOperation> {
        if !op.summary.state.is_claimed() || op.effect_started || op.summary.cancel_requested {
            return Err(corrupt());
        }
        self.changed(self.connection.execute(
            "UPDATE operations SET state='queued',owner_token=NULL,external_effect_started=0,progress_percent=0,\
             progress_message=NULL,error_code=NULL,error_message=NULL,updated_at=? \
             WHERE id=? AND server_id=? AND state=? AND owner_token IS ? AND external_effect_started=0",
            params![self.now, op.summary.id.as_str(), self.server, op.summary.state.as_str(), op.owner_token],
        )?)?;
        self.delete_locks(&op.summary.id)?;
        self.require(&op.summary.id)
    }
    pub(super) fn recovery_required(
        &self,
        op: &StoredOperation,
    ) -> OperationsResult<StoredOperation> {
        if op.summary.state.is_terminal() {
            return Err(corrupt());
        }
        self.changed(self.connection.execute(
            "UPDATE operations SET state='recovery_required',owner_token=NULL,finished_at=NULL,updated_at=?,\
             error_code='effect_outcome_unknown',error_message=NULL \
             WHERE id=? AND server_id=? AND state=? AND owner_token IS ?",
            params![self.now, op.summary.id.as_str(), self.server, op.summary.state.as_str(), op.owner_token],
        )?)?;
        // Intentionally no DELETE of locks or input: recovery is NONTERMINAL.
        self.require(&op.summary.id)
    }
    pub(super) fn check_foreign_or_terminal_locks(&self) -> OperationsResult<()> {
        let broken: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM resource_locks l LEFT JOIN operations o ON o.id=l.operation_id \
             WHERE o.id IS NULL OR o.server_id<>? OR o.state IN ('succeeded','failed','cancelled'))",
            [self.server], |row| row.get(0),
        )?;
        if broken {
            return Err(corrupt());
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn purge_page(
        &self,
        cutoff: i64,
        limit: usize,
    ) -> OperationsResult<Vec<OperationId>> {
        let sql = format!(
            "SELECT {} FROM operations WHERE server_id=? AND input_status='available' \
             AND state IN ('succeeded','failed','cancelled') AND finished_at IS NOT NULL AND finished_at<? \
             ORDER BY seq LIMIT ?",
            bounded("id", 64),
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params![self.server, cutoff, limit as i64], |row| {
            row.get::<_, Option<String>>(0)
        })?;
        rows.map(|row| row.map_err(OperationError::from).and_then(stored_id))
            .collect()
    }
    #[cfg(test)]
    pub(super) fn purge(&self, id: &OperationId, cutoff: i64) -> OperationsResult<()> {
        let op = self.require(id)?;
        self.check_locks(&op)?;
        if !op.summary.state.is_terminal() || op.owner_token.is_some() {
            return Err(corrupt());
        }
        self.changed(self.connection.execute(
            "UPDATE operations SET input_json=NULL,input_status='purged' WHERE id=? AND server_id=? \
             AND input_status='available' AND state IN ('succeeded','failed','cancelled') \
             AND finished_at IS NOT NULL AND finished_at<?",
            params![id.as_str(), self.server, cutoff],
        )?)
    }
}

impl<'a> Repository<'a> {
    /// Mutations below are reachable only after the owner core has checked the
    /// exact current private session/token, canonical binding and lockset, and
    /// resolved policy in this same transaction. Returned-outcome bookkeeping may
    /// record clean denial, never grant a new effect. SQL repeats owner/state/effect.
    pub(super) fn mark_running(&self, op: &StoredOperation) -> OperationsResult<StoredOperation> {
        if op.summary.state != OperationState::Validating
            || op.effect_started
            || op.summary.cancel_requested
            || op.owner_token.is_none()
        {
            return Err(corrupt());
        }
        self.changed(self.connection.execute(
            "UPDATE operations SET state='running',updated_at=? WHERE id=? AND server_id=? \
             AND state='validating' AND owner_token=? AND external_effect_started=0 AND cancel_requested=0",
            params![self.now, op.summary.id.as_str(), self.server, op.owner_token],
        )?)?;
        self.require(&op.summary.id)
    }
    pub(super) fn mark_effect(&self, op: &StoredOperation) -> OperationsResult<StoredOperation> {
        if op.summary.state != OperationState::Running
            || op.effect_started
            || op.summary.cancel_requested
            || op.owner_token.is_none()
        {
            return Err(corrupt());
        }
        self.changed(self.connection.execute(
            "UPDATE operations SET external_effect_started=1,updated_at=? WHERE id=? AND server_id=? \
             AND state='running' AND owner_token=? AND external_effect_started=0 AND cancel_requested=0",
            params![self.now, op.summary.id.as_str(), self.server, op.owner_token],
        )?)?;
        self.require(&op.summary.id)
    }
    #[cfg(test)]
    pub(super) fn progress(
        &self,
        op: &StoredOperation,
        percent: u8,
    ) -> OperationsResult<StoredOperation> {
        if !op.summary.state.is_claimed() || op.owner_token.is_none() || percent > 100 {
            return Err(corrupt());
        }
        self.changed(self.connection.execute(
            "UPDATE operations SET progress_percent=?,progress_message=NULL,updated_at=? \
             WHERE id=? AND server_id=? AND state=? AND owner_token=? AND external_effect_started=? \
             AND cancel_requested=?",
            params![percent, self.now, op.summary.id.as_str(), self.server, op.summary.state.as_str(),
                op.owner_token, op.effect_started, op.summary.cancel_requested],
        )?)?;
        self.require(&op.summary.id)
    }
    pub(super) fn finish_owned(
        &self,
        op: &StoredOperation,
        terminal: OperationState,
        code: Option<&'static str>,
        result_json: Option<&str>,
    ) -> OperationsResult<StoredOperation> {
        if !op.summary.state.is_claimed()
            || op.owner_token.is_none()
            || !terminal.is_terminal()
            || (terminal == OperationState::Cancelled && op.effect_started)
            || (terminal == OperationState::Succeeded && !op.effect_started)
        {
            return Err(corrupt());
        }
        self.changed(self.connection.execute(
            "UPDATE operations SET state=?,finished_at=?,updated_at=?,owner_token=NULL,error_code=?,\
             error_message=NULL,result_json=?,progress_message=NULL WHERE id=? AND server_id=? AND state=? \
             AND owner_token=? AND external_effect_started=? AND cancel_requested=?",
            params![terminal.as_str(), self.now, self.now, code, result_json,
                op.summary.id.as_str(), self.server, op.summary.state.as_str(), op.owner_token,
                op.effect_started, op.summary.cancel_requested],
        )?)?;
        self.delete_locks(&op.summary.id)?;
        self.require(&op.summary.id)
    }
    fn delete_locks(&self, id: &OperationId) -> OperationsResult<()> {
        self.connection.execute(
            "DELETE FROM resource_locks WHERE operation_id=?",
            [id.as_str()],
        )?;
        Ok(())
    }
    fn changed(&self, count: usize) -> OperationsResult<()> {
        if count != 1 {
            return Err(corrupt());
        }
        Ok(())
    }
}

pub(super) fn bounded(column: &'static str, max: usize) -> String {
    format!(
        "CASE WHEN typeof({column})='text' AND length(CAST({column} AS BLOB)) BETWEEN 1 AND {max} \
             AND instr({column},char(0))=0 THEN {column} END"
    )
}
fn nullable_valid(column: &'static str, max: usize) -> String {
    format!(
        "CASE WHEN {column} IS NULL OR (typeof({column})='text' \
             AND length(CAST({column} AS BLOB)) BETWEEN 1 AND {max} AND instr({column},char(0))=0) THEN 1 ELSE 0 END"
    )
}
fn summary_columns() -> String {
    format!(
        "{},seq,{},progress_percent,cancel_requested,created_at,updated_at,finished_at",
        bounded("id", 64),
        bounded("state", 24)
    )
}
struct RawSummary {
    id: Option<String>,
    seq: i64,
    state: Option<String>,
    progress: i64,
    cancel: i64,
    created: i64,
    updated: i64,
    finished: Option<i64>,
}
fn raw_summary(row: &Row<'_>) -> rusqlite::Result<RawSummary> {
    Ok(RawSummary {
        id: row.get(0)?,
        seq: row.get(1)?,
        state: row.get(2)?,
        progress: row.get(3)?,
        cancel: row.get(4)?,
        created: row.get(5)?,
        updated: row.get(6)?,
        finished: row.get(7)?,
    })
}
impl RawSummary {
    fn decode(self) -> OperationsResult<OperationSummary> {
        let state = OperationState::stored(&self.state.ok_or_else(corrupt)?)?;
        if self.seq <= 0
            || !(0..=100).contains(&self.progress)
            || (!state.is_terminal() && self.finished.is_some())
        {
            return Err(corrupt());
        }
        Ok(OperationSummary {
            id: stored_id(self.id)?,
            seq: self.seq,
            state,
            progress_percent: self.progress as u8,
            cancel_requested: boolean(self.cancel)?,
            created_at_ms: self.created,
            updated_at_ms: self.updated,
            finished_at_ms: self.finished,
        })
    }
}
struct RawOperation {
    summary: RawSummary,
    principal: Option<String>,
    device: Option<String>,
    source: Option<String>,
    operation_type: Option<String>,
    target: Option<String>,
    payload_hash: Option<String>,
    idempotency_key: Option<String>,
    effect_idempotent: i64,
    requires_confirmation: i64,
    effect_started: i64,
    input_status: Option<String>,
    owner_token: Option<String>,
    owner_valid: i64,
    key_valid: i64,
}
impl RawOperation {
    fn decode(self) -> OperationsResult<StoredOperation> {
        let source = self.source.ok_or_else(corrupt)?;
        let operation_type = self.operation_type.ok_or_else(corrupt)?;
        let target = self.target.ok_or_else(corrupt)?;
        let payload_hash = self.payload_hash.ok_or_else(corrupt)?;
        if !text(&source, 64)
            || !identifier(&operation_type, 64)
            || !text(&target, 512)
            || self.owner_valid != 1
            || self.key_valid != 1
            || self
                .idempotency_key
                .as_deref()
                .is_some_and(|key| !text(key, 128))
            || self.owner_token.as_deref().is_some_and(|token| {
                token.is_empty()
                    || !token
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            })
        {
            return Err(corrupt());
        }
        let input_status = match self.input_status.as_deref() {
            Some("available") => InputStatus::Available,
            Some("unavailable_legacy") => InputStatus::UnavailableLegacy,
            Some("purged") => InputStatus::Purged,
            _ => return Err(corrupt()),
        };
        let principal =
            PrincipalId::parse(&self.principal.ok_or_else(corrupt)?).map_err(|_| corrupt())?;
        let device = DeviceId::parse(&self.device.ok_or_else(corrupt)?).map_err(|_| corrupt())?;
        Ok(StoredOperation {
            summary: self.summary.decode()?,
            subject: Subject { principal, device },
            source,
            operation_type,
            target,
            payload_hash,
            idempotency_key: self.idempotency_key,
            effect_idempotent: boolean(self.effect_idempotent)?,
            requires_confirmation: boolean(self.requires_confirmation)?,
            effect_started: boolean(self.effect_started)?,
            input_status,
            owner_token: self.owner_token,
        })
    }
}
fn boolean(value: i64) -> OperationsResult<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(corrupt()),
    }
}
fn stored_id(value: Option<String>) -> OperationsResult<OperationId> {
    OperationId::parse(&value.ok_or_else(corrupt)?).map_err(|_| corrupt())
}
