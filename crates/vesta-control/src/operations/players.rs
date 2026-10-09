//! Current-authorized, closed `list` queries over the concrete owned game runtime.
//!
//! A Subject is a trusted application lookup key, never transport authentication.
//! This API accepts no command text, handler or authorization callback. ConsoleRead
//! is checked in an admitted current snapshot before mailbox/native admission,
//! again by the owned native writer, and immediately before exposing a result.
//! Task-view or pack/password access is not console access.
//!
//! Native acknowledgements mean the exact fixed stdin write returned, not that the
//! game processed it. A caller timeout/shutdown cannot detach its writer: the driver
//! retains the receipt, generation, database owner and native workers until actual
//! acknowledgement/exit. Database admission and native IO have their own ownership
//! semantics; the query deadline is not a hard cancellation bound for those calls.
//!
//! Without request IDs in console output, causal correlation is impossible. Even a
//! serialized, post-write-horizon matching line may be unsolicited, a delayed earlier
//! response or an earlier partial line published later. Returned data describes an
//! observed line for the requested generation, never authenticated player identity,
//! a causally proven response, server readiness or continued future liveness.

use super::{
    OperationError, OperationRuntime, Operations,
    game::{GameLifecycle, LiveGameView},
};
use crate::identity::{self, Capability, Subject};
use std::{
    error::Error,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{Duration, Instant},
};
use vesta_game_runtime::{
    console::{ConsoleCapture, ConsoleError, MAX_PAGE_LINES},
    players::{PlayerList, PlayerListError, parse_player_list_line},
    process::{
        GenerationId, GuardedError, GuardedInput, ProcessPhase, ProcessSnapshot, RuntimeError,
    },
};
use vesta_state::TransactionError;

pub const MAX_PLAYER_QUERY_WAIT: Duration = Duration::from_secs(10);
pub const MAX_PLAYER_QUERY_LINES: usize = 4_000;
pub const MAX_PLAYER_QUERY_BYTES: usize = 1_048_576;

pub type PlayerQueryResult<T> = Result<T, PlayerQueryError>;

/// Input-free classifications, retaining typed fixed native/policy/persistence causes.
pub enum PlayerQueryError {
    InvalidRequest,
    Busy,
    TimedOut,
    ShuttingDown,
    NotRunning,
    StaleGeneration,
    ConsoleGap,
    ConsoleIncomplete,
    ScanLimit,
    Unavailable,
    Parser(PlayerListError),
    Console(ConsoleError),
    Runtime(RuntimeError),
    Operation(OperationError),
    Rollback {
        primary: Box<PlayerQueryError>,
        failure: Box<PlayerQueryError>,
    },
}
impl PlayerQueryError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "players_query_invalid",
            Self::Busy => "players_query_busy",
            Self::TimedOut => "players_query_timeout",
            Self::ShuttingDown => "players_query_stopping",
            Self::NotRunning => "players_query_not_running",
            Self::StaleGeneration => "players_query_stale_generation",
            Self::ConsoleGap => "players_query_console_gap",
            Self::ConsoleIncomplete => "players_query_console_incomplete",
            Self::ScanLimit => "players_query_scan_limit",
            Self::Unavailable => "players_query_unavailable",
            Self::Parser(error) => error.code(),
            Self::Console(error) => error.code(),
            Self::Runtime(error) => error.code(),
            Self::Operation(error) => error.code(),
            Self::Rollback { .. } => "players_query_rollback_failed",
        }
    }
}
impl fmt::Display for PlayerQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Player query failed")
    }
}
impl fmt::Debug for PlayerQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlayerQueryError")
            .field("code", &self.code())
            .finish()
    }
}
impl Error for PlayerQueryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parser(error) => Some(error),
            Self::Console(error) => Some(error),
            Self::Runtime(error) => Some(error),
            Self::Operation(error) => Some(error),
            Self::Rollback { primary, .. } => Some(primary.as_ref()),
            _ => None,
        }
    }
}
impl From<OperationError> for PlayerQueryError {
    fn from(error: OperationError) -> Self {
        Self::Operation(error)
    }
}
impl From<ConsoleError> for PlayerQueryError {
    fn from(error: ConsoleError) -> Self {
        Self::Console(error)
    }
}
impl From<RuntimeError> for PlayerQueryError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(error)
    }
}
impl From<TransactionError<PlayerQueryError>> for PlayerQueryError {
    fn from(error: TransactionError<PlayerQueryError>) -> Self {
        match error {
            TransactionError::State(error) => Self::Operation(error.into()),
            TransactionError::Work(error) => error,
            TransactionError::Rollback { primary, failure } => Self::Rollback {
                primary: Box::new((*primary).into()),
                failure: Box::new(Self::Operation(failure.into())),
            },
        }
    }
}

/// Sensitive console-derived descriptions, not a readiness/identity/causality proof.
pub struct PlayerObservation {
    generation: GenerationId,
    after_cursor: u64,
    response_cursor: u64,
    list: PlayerList,
}
impl PlayerObservation {
    pub fn generation(&self) -> GenerationId {
        self.generation
    }
    pub fn after_cursor(&self) -> u64 {
        self.after_cursor
    }
    pub fn response_cursor(&self) -> u64 {
        self.response_cursor
    }
    pub fn list(&self) -> &PlayerList {
        &self.list
    }
}
impl fmt::Debug for PlayerObservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlayerObservation")
            .field("list", &self.list)
            .finish_non_exhaustive()
    }
}

impl OperationRuntime {
    /// Issue only `list` to this concrete runtime's exact running generation. The
    /// caller must supply a trusted authenticated application Subject, not fabricate
    /// it from a header/UUID. Timeout is an observation failure, never zero players.
    pub fn query_players(
        &self,
        subject: &Subject,
        expected_generation: GenerationId,
        timeout: Duration,
    ) -> PlayerQueryResult<PlayerObservation> {
        self.player_queries()
            .query(subject, expected_generation, timeout)
    }
}

fn with_console_access<T>(
    operations: &Operations,
    subject: &Subject,
    work: impl FnOnce() -> PlayerQueryResult<T>,
) -> PlayerQueryResult<T> {
    operations
        .db
        .read_with(|snapshot| {
            let policy = identity::policy_in(
                snapshot.connection(),
                operations.db.server_id(),
                operations.db.clock().millis(),
                Some(subject),
            )
            .map_err(OperationError::from)?;
            match policy.check(Capability::ConsoleRead, None) {
                identity::Decision::Allow => work(),
                identity::Decision::Deny(reason) => Err(OperationError::forbidden(reason).into()),
            }
        })
        .map_err(PlayerQueryError::from)
}

fn ensure_live(snapshot: ProcessSnapshot, generation: GenerationId) -> PlayerQueryResult<()> {
    if snapshot.generation != Some(generation) {
        return Err(PlayerQueryError::StaleGeneration);
    }
    if snapshot.phase != ProcessPhase::Running {
        return Err(PlayerQueryError::NotRunning);
    }
    if snapshot.output_failed {
        return Err(PlayerQueryError::ConsoleIncomplete);
    }
    if snapshot.stdin_failed {
        return Err(RuntimeError::StdinUnavailable.into());
    }
    Ok(())
}
fn ensure_view(view: &LiveGameView, generation: GenerationId) -> PlayerQueryResult<()> {
    // Sticky loss of native observation proof cannot be hidden by a stale snapshot.
    view.revision()?;
    ensure_live(view.snapshot()?, generation)
}

struct Request {
    subject: Subject,
    generation: GenerationId,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    reply: Option<SyncSender<PlayerQueryResult<PlayerObservation>>>,
}
impl Request {
    fn alive(&self, stopping: &AtomicBool) -> PlayerQueryResult<()> {
        if stopping.load(Ordering::Acquire) {
            return Err(PlayerQueryError::ShuttingDown);
        }
        if self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err(PlayerQueryError::TimedOut);
        }
        Ok(())
    }
    fn respond(&mut self, result: PlayerQueryResult<PlayerObservation>) {
        if let Some(reply) = self.reply.take() {
            // One bounded single-result channel, never a detached response worker.
            let _ = reply.try_send(result);
        }
    }
}
#[derive(Default)]
struct Mailbox {
    pending: Option<Request>,
    busy: bool,
}

pub(super) struct QueryEndpoint {
    operations: Operations,
    mailbox: Arc<Mutex<Mailbox>>,
    stopping: Arc<AtomicBool>,
    view: Arc<LiveGameView>,
}
impl QueryEndpoint {
    fn query(
        &self,
        subject: &Subject,
        generation: GenerationId,
        timeout: Duration,
    ) -> PlayerQueryResult<PlayerObservation> {
        if timeout.is_zero() || timeout > MAX_PLAYER_QUERY_WAIT {
            return Err(PlayerQueryError::InvalidRequest);
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(PlayerQueryError::InvalidRequest)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let (reply, receiver) = mpsc::sync_channel(1);
        let request = Request {
            subject: subject.clone(),
            generation,
            deadline,
            cancelled: cancelled.clone(),
            reply: Some(reply),
        };
        // Hold only the bounded mailbox lock, then resolve policy immediately before
        // enqueue. Driver never holds this lock during DB/native work.
        let mut mailbox = self
            .mailbox
            .lock()
            .map_err(|_| PlayerQueryError::Unavailable)?;
        with_console_access(&self.operations, subject, || {
            request.alive(&self.stopping)?;
            ensure_view(&self.view, generation)?;
            if mailbox.busy {
                return Err(PlayerQueryError::Busy);
            }
            mailbox.busy = true;
            mailbox.pending = Some(request);
            Ok(())
        })?;
        drop(mailbox);
        let result = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        match result {
            Ok(Ok(observation)) => with_console_access(&self.operations, subject, || {
                if self.stopping.load(Ordering::Acquire) {
                    return Err(PlayerQueryError::ShuttingDown);
                }
                ensure_view(&self.view, generation)?;
                // Receiving a result after this caller's deadline does not revive it.
                if Instant::now() >= deadline {
                    return Err(PlayerQueryError::TimedOut);
                }
                Ok(observation)
            }),
            Ok(Err(error)) => Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancelled.store(true, Ordering::Release);
                Err(PlayerQueryError::TimedOut)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                cancelled.store(true, Ordering::Release);
                Err(PlayerQueryError::Unavailable)
            }
        }
    }
}

enum Phase {
    Writing {
        receipt: GuardedInput<PlayerQueryError>,
        marker: Arc<Mutex<Option<u64>>>,
    },
    Reading {
        after: u64,
        cursor: u64,
        lines: usize,
        bytes: usize,
    },
}
struct Pending {
    request: Request,
    phase: Phase,
}

pub(super) struct QueryDriver {
    operations: Operations,
    mailbox: Arc<Mutex<Mailbox>>,
    stopping: Arc<AtomicBool>,
    console: ConsoleCapture,
    current: Option<Pending>,
}
impl QueryDriver {
    pub(super) fn new(
        operations: Operations,
        stopping: Arc<AtomicBool>,
        view: Arc<LiveGameView>,
        console: ConsoleCapture,
    ) -> (QueryEndpoint, Self) {
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        (
            QueryEndpoint {
                operations: operations.clone(),
                mailbox: mailbox.clone(),
                stopping: stopping.clone(),
                view,
            },
            Self {
                operations,
                mailbox,
                stopping,
                console,
                current: None,
            },
        )
    }
    /// Native lifecycle dispatch pauses only while an actual list write is pending.
    /// Waiting for console data must not prevent a separately authorized stop/kill.
    pub(super) fn writer_pending(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|pending| matches!(pending.phase, Phase::Writing { .. }))
    }
    fn clear_busy(&self) {
        self.mailbox
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .busy = false;
    }
    fn take_request(&self) -> PlayerQueryResult<Option<Request>> {
        Ok(self
            .mailbox
            .lock()
            .map_err(|_| PlayerQueryError::Unavailable)?
            .pending
            .take())
    }
    /// Called after native poll/publication, with no mailbox or DB lock held.
    /// An internal native ObserveFailed is returned for the runner's sticky failure
    /// path; no private query may silently consume that lost observation proof.
    pub(super) fn advance(
        &mut self,
        game: &mut GameLifecycle,
        may_enqueue: bool,
        uncertain: bool,
    ) -> Option<RuntimeError> {
        if self.current.is_none() && may_enqueue {
            match self.take_request() {
                Ok(Some(mut request)) => {
                    let queued = self.enqueue(game, &request, uncertain);
                    match queued {
                        Ok(phase) => self.current = Some(Pending { request, phase }),
                        Err(error) => {
                            let observation_failed = matches!(
                                error,
                                PlayerQueryError::Runtime(RuntimeError::ObserveFailed)
                            );
                            request.respond(Err(error));
                            self.clear_busy();
                            if observation_failed {
                                return Some(RuntimeError::ObserveFailed);
                            }
                        }
                    }
                }
                Ok(None) => (),
                Err(_) => (), // poisoned mailbox refuses callers; never executes its request
            }
        }
        let mut pending = self.current.take()?;
        let refusal = pending.request.alive(&self.stopping).and_then(|()| {
            if uncertain {
                Err(RuntimeError::ObserveFailed.into())
            } else {
                ensure_live(game.process.snapshot(), pending.request.generation)
            }
        });
        if let Err(error) = refusal {
            pending.request.cancelled.store(true, Ordering::Release);
            pending.request.respond(Err(error));
        }
        match pending.phase {
            Phase::Writing {
                mut receipt,
                marker,
            } => match receipt.try_result() {
                None => {
                    self.current = Some(Pending {
                        request: pending.request,
                        phase: Phase::Writing { receipt, marker },
                    })
                }
                Some(Ok(())) if pending.request.reply.is_some() => {
                    let after = marker.lock().ok().and_then(|marker| *marker);
                    match after {
                        Some(after) => {
                            self.current = Some(Pending {
                                request: pending.request,
                                phase: Phase::Reading {
                                    after,
                                    cursor: after,
                                    lines: 0,
                                    bytes: 0,
                                },
                            })
                        }
                        None => {
                            pending.request.respond(Err(PlayerQueryError::Unavailable));
                            self.clear_busy();
                        }
                    }
                }
                Some(Ok(())) => self.clear_busy(),
                Some(Err(GuardedError::Guard(error))) => {
                    pending.request.respond(Err(error));
                    self.clear_busy();
                }
                Some(Err(GuardedError::Runtime(error))) => {
                    pending.request.respond(Err(error.into()));
                    self.clear_busy();
                }
            },
            Phase::Reading {
                after,
                mut cursor,
                mut lines,
                mut bytes,
            } => {
                if pending.request.reply.is_none() {
                    self.clear_busy();
                    return None;
                }
                let found = with_console_access(&self.operations, &pending.request.subject, || {
                    pending.request.alive(&self.stopping)?;
                    ensure_live(game.process.snapshot(), pending.request.generation)?;
                    scan(
                        &self.console,
                        after,
                        &mut cursor,
                        &mut lines,
                        &mut bytes,
                        pending.request.generation,
                    )
                });
                match found {
                    Ok(Some(observation)) => {
                        pending.request.respond(Ok(observation));
                        self.clear_busy();
                    }
                    Ok(None) => {
                        self.current = Some(Pending {
                            request: pending.request,
                            phase: Phase::Reading {
                                after,
                                cursor,
                                lines,
                                bytes,
                            },
                        })
                    }
                    Err(error) => {
                        pending.request.respond(Err(error));
                        self.clear_busy();
                    }
                }
            }
        }
        None
    }
    fn enqueue(
        &self,
        game: &mut GameLifecycle,
        request: &Request,
        uncertain: bool,
    ) -> PlayerQueryResult<Phase> {
        let marker = Arc::new(Mutex::new(None));
        let mark = marker.clone();
        let operations = self.operations.clone();
        let subject = request.subject.clone();
        let stopping = self.stopping.clone();
        let cancelled = request.cancelled.clone();
        let deadline = request.deadline;
        let generation = request.generation;
        let view = game.view.clone();
        let console = self.console.clone();
        let receipt = with_console_access(&self.operations, &request.subject, || {
            request.alive(&self.stopping)?;
            if uncertain {
                return Err(RuntimeError::ObserveFailed.into());
            }
            ensure_live(game.process.snapshot(), generation)?;
            game.process
                .queue_list_guarded(generation, move || {
                    with_console_access(&operations, &subject, || {
                        if stopping.load(Ordering::Acquire) {
                            return Err(PlayerQueryError::ShuttingDown);
                        }
                        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                            return Err(PlayerQueryError::TimedOut);
                        }
                        ensure_view(&view, generation)?;
                        // Horizon immediately before the fixed write, not request acceptance
                        // or its later ACK. An immediate response cannot be skipped.
                        let latest = console.snapshot(0, 1)?.latest_cursor;
                        *mark.lock().map_err(|_| PlayerQueryError::Unavailable)? = Some(latest);
                        Ok(())
                    })
                })
                .map_err(PlayerQueryError::from)
        })?;
        Ok(Phase::Writing { receipt, marker })
    }
}

fn scan(
    console: &ConsoleCapture,
    after: u64,
    cursor: &mut u64,
    lines: &mut usize,
    bytes: &mut usize,
    generation: GenerationId,
) -> PlayerQueryResult<Option<PlayerObservation>> {
    let page = console.snapshot(*cursor, MAX_PAGE_LINES)?;
    if page.gap {
        return Err(PlayerQueryError::ConsoleGap);
    }
    for line in page.lines {
        if *lines == MAX_PLAYER_QUERY_LINES
            || line.text.len() > MAX_PLAYER_QUERY_BYTES.saturating_sub(*bytes)
        {
            return Err(PlayerQueryError::ScanLimit);
        }
        *lines += 1;
        *bytes += line.text.len();
        *cursor = line.cursor;
        // Capture explicitly uses lossy UTF-8. Refuse U+FFFD conservatively because
        // it cannot prove whether that scalar was literal or repaired malformed bytes.
        if line.truncated || line.text.contains('\u{fffd}') {
            return Err(PlayerQueryError::ConsoleIncomplete);
        }
        match parse_player_list_line(&line.text) {
            Ok(list) => {
                return Ok(Some(PlayerObservation {
                    generation,
                    after_cursor: after,
                    response_cursor: line.cursor,
                    list,
                }));
            }
            Err(PlayerListError::NotRecognized) => (),
            Err(error) => return Err(PlayerQueryError::Parser(error)),
        }
    }
    // next_cursor is the last scanned line, not latest_cursor: never skip a page.
    *cursor = page.next_cursor;
    Ok(None)
}

#[cfg(test)]
#[path = "players_tests.rs"]
mod tests;
