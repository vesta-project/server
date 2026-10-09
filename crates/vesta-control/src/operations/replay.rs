//! Authorized, bounded journal observations. Cursors are private in-process values,
//! not credentials or a transport encoding; a caller must supply current trusted
//! subject lookup keys on every call. No journal payload is returned.
use super::{
    OperationError, OperationId, OperationState, Operations, OperationsResult, domain::invalid,
    repository::Repository,
};
use crate::identity::Subject;
use serde::Deserialize;
use std::fmt;
use vesta_state::{StateError, events::ScanRow};

pub const MAX_REPLAY_RETURN: usize = 100;
pub const MAX_REPLAY_SCAN: usize = vesta_state::events::MAX_PAGE;
const KINDS: [&str; 3] = ["operation.created", "operation.state", "operation.progress"];

/// An opaque checkpoint/continuation bound to the exact observing principal,
/// device and server. There is deliberately no public constructor, wire parser,
/// serialization or field access. Cloning does not transfer observation authority.
#[derive(Clone, Eq, PartialEq)]
pub struct ReplayCursor {
    server: String,
    subject: Subject,
    position: i64,
    through: i64,
}
impl fmt::Debug for ReplayCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReplayCursor").finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayStatus {
    Ok,
    /// History before the pruning watermark is unavailable. No continuation is
    /// issued: a client must obtain a fresh authorized state observation.
    ResyncRequired,
}

/// The closed set written by the operation kernel, never a caller-declared type
/// or permission registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayEventKind {
    Created,
    State,
    Progress,
}
impl ReplayEventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "operation.created",
            Self::State => "operation.state",
            Self::Progress => "operation.progress",
        }
    }
    fn stored(value: &str) -> Option<Self> {
        match value {
            "operation.created" => Some(Self::Created),
            "operation.state" => Some(Self::State),
            "operation.progress" => Some(Self::Progress),
            _ => None,
        }
    }
}

/// Fixed historical lifecycle facts, validated from the bounded journal row.
/// No inputs, results, target, free-form progress text or actor identity are read
/// into this projection. tasks.view observes these facts; it is not mutation or
/// execution authority and does not require the initiating actor's current grants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayEvent {
    pub event_cursor: i64,
    pub kind: ReplayEventKind,
    pub at_ms: i64,
    pub operation_id: OperationId,
    pub state: OperationState,
    pub progress_percent: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayPage {
    pub status: ReplayStatus,
    pub events: Vec<ReplayEvent>,
    pub next_cursor: Option<ReplayCursor>,
    pub complete: bool,
    pub pruned_through: i64,
}

impl Operations {
    /// Captures a new fixed journal horizon and reads after a prior checkpoint,
    /// or after zero when absent. A checkpoint's OLD horizon is not extended by
    /// continuation; reopening explicitly captures a new one.
    pub fn replay_open(
        &self,
        subject: &Subject,
        checkpoint: Option<&ReplayCursor>,
        limit: usize,
    ) -> OperationsResult<ReplayPage> {
        self.replay_read(subject, checkpoint, true, limit)
    }

    /// Continues the original fixed horizon, even if new events have appeared.
    /// Current tasks.view is required again, including for an empty/complete page.
    pub fn replay_page(
        &self,
        subject: &Subject,
        cursor: &ReplayCursor,
        limit: usize,
    ) -> OperationsResult<ReplayPage> {
        self.replay_read(subject, Some(cursor), false, limit)
    }

    /// Obtains a head checkpoint without observing operations. This alone is NOT
    /// an atomic operation-state-and-checkpoint synchronization contract.
    pub fn replay_checkpoint(&self, subject: &Subject) -> OperationsResult<ReplayCursor> {
        self.db
            .read_with(|snapshot| {
                let repo = Repository::new(
                    snapshot.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                );
                self.task_access(&repo, subject)?;
                let bounds = self.events.bounds(repo.connection())?;
                valid_bounds(bounds)?;
                Ok(self.replay_cursor(subject, bounds.latest, bounds.latest))
            })
            .map_err(OperationError::from)
    }

    fn replay_read(
        &self,
        subject: &Subject,
        cursor: Option<&ReplayCursor>,
        open: bool,
        limit: usize,
    ) -> OperationsResult<ReplayPage> {
        if !(1..=MAX_REPLAY_RETURN).contains(&limit) {
            return Err(invalid());
        }
        self.db
            .read_with(|snapshot| {
                let repo = Repository::new(
                    snapshot.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                );
                self.replay_in(&repo, subject, cursor, open, limit)
            })
            .map_err(OperationError::from)
    }

    fn replay_in(
        &self,
        repo: &Repository<'_>,
        subject: &Subject,
        cursor: Option<&ReplayCursor>,
        open: bool,
        limit: usize,
    ) -> OperationsResult<ReplayPage> {
        // Resolve authority BEFORE cursor validation/bounds, not merely before
        // projection: unauthorized callers get no cursor-validity oracle.
        self.task_access(repo, subject)?;
        let bounds = self.events.bounds(repo.connection())?;
        valid_bounds(bounds)?;
        let position = match cursor {
            Some(cursor) => {
                if cursor.subject != *subject || cursor.server != self.db.server_id() {
                    return Err(invalid_cursor());
                }
                cursor.position
            }
            None if open => 0,
            None => return Err(invalid_cursor()),
        };
        let through = if open {
            bounds.latest
        } else {
            cursor.ok_or_else(invalid_cursor)?.through
        };
        if position < 0 || through < 0 || position > through || through > bounds.latest {
            return Err(invalid_cursor());
        }
        if position < bounds.pruned_through {
            return Ok(ReplayPage {
                status: ReplayStatus::ResyncRequired,
                events: Vec::new(),
                next_cursor: None,
                complete: false,
                pruned_through: bounds.pruned_through,
            });
        }
        if position == through {
            return Ok(ReplayPage {
                status: ReplayStatus::Ok,
                events: Vec::new(),
                next_cursor: Some(self.replay_cursor(subject, position, through)),
                complete: true,
                pruned_through: bounds.pruned_through,
            });
        }
        let rows = self.events.scan(
            repo.connection(),
            position,
            through,
            MAX_REPLAY_SCAN,
            &KINDS,
            vesta_state::events::MAX_PAYLOAD_CHARS,
        )?;
        let mut events = Vec::with_capacity(limit);
        let mut processed = position;
        let mut stopped = false;
        for row in &rows {
            if row.cursor <= processed || row.cursor > through {
                return Err(StateError::new("state_integrity_failed").into());
            }
            if let Some(kind) = ReplayEventKind::stored(&row.event_type) {
                if let Some(operation_id) = self.replay_operation(repo, row)? {
                    if events.len() == limit {
                        // Keep this eligible event for the next page. Ineligible
                        // rows after the output limit still consume scan budget.
                        stopped = true;
                        break;
                    }
                    let (state, progress_percent) = validate_payload(row)?;
                    events.push(ReplayEvent {
                        event_cursor: row.cursor,
                        kind,
                        at_ms: row.at_ms,
                        operation_id,
                        state,
                        progress_percent,
                    });
                }
            }
            processed = row.cursor;
        }
        if !stopped && rows.len() < MAX_REPLAY_SCAN {
            // Cursor holes (e.g. rolled-back inserts) do not prevent completion.
            processed = through;
        }
        Ok(ReplayPage {
            status: ReplayStatus::Ok,
            events,
            next_cursor: Some(self.replay_cursor(subject, processed, through)),
            complete: processed == through,
            pruned_through: bounds.pruned_through,
        })
    }

    fn replay_cursor(&self, subject: &Subject, position: i64, through: i64) -> ReplayCursor {
        ReplayCursor {
            server: self.db.server_id().to_owned(),
            subject: subject.clone(),
            position,
            through,
        }
    }

    fn replay_operation(
        &self,
        repo: &Repository<'_>,
        row: &ScanRow,
    ) -> OperationsResult<Option<OperationId>> {
        let id = OperationId::parse(row.operation_id.as_deref().ok_or_else(malformed)?)
            .map_err(|_| malformed())?;
        // Read only fixed task facts, never actor/binding/input data. A missing
        // task is unavailable and advances without parsing its payload. Unknown
        // operation bindings and purged inputs do not hide safe lifecycle history.
        Ok(repo.summary(&id)?.map(|_| id))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LifecyclePayload {
    #[serde(rename = "operationId")]
    operation_id: String,
    state: String,
    progress: u8,
}
fn validate_payload(row: &ScanRow) -> OperationsResult<(OperationState, u8)> {
    let payload = row.payload_json.as_deref().ok_or_else(malformed)?;
    let lifecycle: LifecyclePayload = serde_json::from_str(payload).map_err(|_| malformed())?;
    if Some(lifecycle.operation_id.as_str()) != row.operation_id.as_deref()
        || OperationId::parse(&lifecycle.operation_id).is_err()
        || lifecycle.progress > 100
    {
        return Err(malformed());
    }
    let state = OperationState::stored(&lifecycle.state).map_err(|_| malformed())?;
    Ok((state, lifecycle.progress))
}
fn valid_bounds(bounds: vesta_state::events::Bounds) -> OperationsResult<()> {
    if bounds.pruned_through < 0
        || bounds.latest < bounds.pruned_through
        || bounds.oldest_retained < 0
        || (bounds.oldest_retained != 0
            && (bounds.oldest_retained <= bounds.pruned_through
                || bounds.oldest_retained > bounds.latest))
    {
        return Err(StateError::new("state_integrity_failed").into());
    }
    Ok(())
}
fn invalid_cursor() -> OperationError {
    StateError::new("state_operation_cursor_invalid").into()
}
fn malformed() -> OperationError {
    StateError::new("state_operation_event_malformed").into()
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;
