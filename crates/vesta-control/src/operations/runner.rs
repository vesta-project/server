//! Trusted, closed, autonomously driven operation ownership.
//!
//! One driver owns the native process, exact claims and OS/DB owner. Shutdown is
//! observation-only: it never automatically sends stop/kill/restart. A timeout
//! keeps the same JoinHandle and all native IO handles. Drop requests shutdown
//! and joins to real completion; an uncooperative child/writer can block forever.
use super::{
    Claim, FinishOutcome, GameCommand, OperationError, OperationErrorCode, OperationId, Operations,
    OperationsResult, OwnerSession, SourceRevision,
    game::{GameLifecycle, LiveGameView},
    players::{QueryDriver, QueryEndpoint},
};
use std::{
    fmt,
    sync::mpsc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use vesta_game_runtime::process::{
    GenerationId, GuardedError, GuardedInput, LaunchSpec, ProcessPhase, ProcessSnapshot,
    RuntimeError, WaitOutcome,
};
use vesta_state::StateError;

const DRIVER_INTERVAL: Duration = Duration::from_millis(5);
const MAX_SHUTDOWN_WAIT: Duration = Duration::from_secs(300);
const DEFAULT_STOP_WAIT: Duration = Duration::from_secs(10);

// Pure completion policy used by Active::Exit. Draining, uncertainty and missing
// deadlines are never time-based evidence of returned/joined native work.
fn stop_deadline_elapsed(
    phase: ProcessPhase,
    uncertain: Option<&'static str>,
    deadline: Option<Instant>,
    now: Instant,
) -> bool {
    uncertain.is_none()
        && matches!(phase, ProcessPhase::Running | ProcessPhase::Stopping)
        && deadline.is_some_and(|deadline| now >= deadline)
}

/// Bounded ownership facts, not readiness, console data or execution authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunnerStatus {
    pub dispatch_stopped: bool,
    pub driver_finished: bool,
    pub active_operation: Option<OperationId>,
    pub owned_generation: Option<GenerationId>,
    pub last_error_code: Option<&'static str>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShutdownOutcome {
    Complete,
    TimedOut(RunnerStatus),
}

#[derive(Default)]
struct Observation {
    active: Option<OperationId>,
    last_error: Option<&'static str>,
}
struct DriverState {
    // This is the kernel's exact shared stopping flag, not a delayed mirror.
    stopping: Arc<AtomicBool>,
    observation: Mutex<Observation>,
    view: Arc<LiveGameView>,
    players: QueryEndpoint,
    console: vesta_game_runtime::console::ConsoleCapture,
    player_count: Mutex<Option<(GenerationId, u32, i64, u64)>>,
}
impl DriverState {
    fn new(owner: &OwnerSession, game: &GameLifecycle) -> (Arc<Self>, QueryDriver) {
        let stopping = owner.stop_flag();
        let (players, queries) = QueryDriver::new(
            owner.operations().clone(),
            stopping.clone(),
            game.view.clone(),
            game.console.clone(),
        );
        (
            Arc::new(Self {
                stopping,
                observation: Mutex::new(Observation::default()),
                view: game.view.clone(),
                players,
                console: game.console.clone(),
                player_count: Mutex::new(None),
            }),
            queries,
        )
    }
    fn error(&self, code: &'static str) {
        let mut observation = self
            .observation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        observation.last_error = Some(code);
    }
    fn active(&self, id: Option<OperationId>) {
        self.observation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .active = id;
    }
    fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
    }
}

/// Concrete trusted host composition. Only the literal LaunchSpec passed by
/// trusted host code is executable; operation input cannot select executable,
/// argv, cwd, URL, handler or authority. Console text is a bounded desired command,
/// not a launch option or authorization proof. Opening never starts a game
/// except by executing an already-admitted, currently authorized queued Start.
///
/// No manual tick is required. The bounded driver observes/polls and dispatches
/// the closed lifecycle/console enum. There is no arbitrary public registry.
pub struct OperationRuntime {
    operations: Operations,
    shared: Arc<DriverState>,
    // Never detached on timeout. Taking this is allowed only for actual join.
    driver: Option<JoinHandle<OperationsResult<()>>>,
    local: LocalRuntime,
    start_block: super::game::StartGate,
}

/// Trusted OS-local host adapter only. Not a Subject, credential or remote grant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalAction {
    Start,
    Stop,
    Restart,
}

struct LocalRequest {
    action: LocalAction,
    reply: mpsc::SyncSender<Result<(), &'static str>>,
}

/// A bounded mailbox into the existing sole process owner. Host code must expose
/// this only through an authenticated same-account native local channel.
#[derive(Clone)]
pub struct LocalRuntime {
    sender: mpsc::SyncSender<LocalRequest>,
}
impl LocalRuntime {
    pub fn request(&self, action: LocalAction) -> Result<(), &'static str> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.sender
            .try_send(LocalRequest { action, reply })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "local_runtime_busy",
                mpsc::TrySendError::Disconnected(_) => "local_runtime_unavailable",
            })?;
        // Once queued the request is not cancelled by a caller timeout. Only the
        // sole owner produces an outcome; channel clients may lose that outcome.
        receiver.recv().map_err(|_| "local_runtime_unavailable")?
    }
}
impl fmt::Debug for OperationRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperationRuntime")
            .field(
                "dispatch_stopped",
                &self.shared.stopping.load(Ordering::Acquire),
            )
            .field("driver_owned", &self.driver.is_some())
            .finish_non_exhaustive()
    }
}
/// Cloneable observations/admission, without local-owner commands or process ownership.
#[derive(Clone)]
pub struct RuntimeHandle {
    operations: Operations,
    shared: Arc<DriverState>,
}
impl RuntimeHandle {
    /// Check the server-owned live generation in the same admission writer,
    /// after exact retry retrieval and before creating a new operation.
    pub fn submit_game(
        &self,
        subject: &crate::identity::Subject,
        command: GameCommand,
        idempotency_key: &str,
    ) -> OperationsResult<super::Admission> {
        self.operations.admit_with_view(
            subject,
            command.operation_type(),
            &command.canonical_input()?,
            Some(idempotency_key),
            Some(&self.shared.view),
        )
    }

    pub(super) fn operations(&self) -> &Operations {
        &self.operations
    }
    pub(super) fn source_revision(&self) -> OperationsResult<SourceRevision> {
        self.shared.view.revision()
    }
    pub(super) fn player_queries(&self) -> &QueryEndpoint {
        &self.shared.players
    }
    pub fn last_player_count(&self, generation: Option<GenerationId>) -> Option<(u32, i64)> {
        let generation = generation?;
        self.shared
            .player_count
            .lock()
            .ok()?
            .filter(|v| v.0 == generation)
            .map(|v| (v.1, v.2))
    }
    pub(super) fn remember_players(&self, value: &super::players::PlayerObservation) {
        if let Ok(mut count) = self.shared.player_count.lock()
            && self.shared.view.snapshot().ok().and_then(|v| v.generation)
                == Some(value.generation())
            && count.is_none_or(|v| v.0 != value.generation() || v.3 <= value.response_cursor())
        {
            *count = Some((
                value.generation(),
                value.list().online(),
                value.observed_at_ms(),
                value.response_cursor(),
            ));
        }
    }
    pub fn game_snapshot(&self) -> OperationsResult<ProcessSnapshot> {
        self.shared.view.snapshot()
    }
    pub fn game_observation(&self) -> OperationsResult<(ProcessSnapshot, Option<u64>)> {
        self.shared.view.observation()
    }
    /// Separate-supervisor conditions (crash restart, orphan, unresolved launch);
    /// `None` when this agent owns the game process itself.
    pub fn supervision(&self) -> OperationsResult<Option<vesta_supervisor::wire::Health>> {
        self.shared.view.supervision()
    }
    /// Starting/ready/degraded evidence for the current generation.
    pub fn readiness(&self) -> OperationsResult<super::readiness::ReadinessView> {
        self.shared.view.readiness()
    }
    pub fn operations_service(&self) -> &Operations {
        &self.operations
    }
    pub fn console_snapshot(
        &self,
        after: u64,
        limit: usize,
    ) -> Result<vesta_game_runtime::console::ConsolePage, vesta_game_runtime::console::ConsoleError>
    {
        self.shared.console.snapshot(after, limit)
    }
}
impl OperationRuntime {
    pub fn network_handle(&self) -> RuntimeHandle {
        RuntimeHandle {
            operations: self.operations.clone(),
            shared: self.shared.clone(),
        }
    }
    pub fn open(operations: Operations, launch: LaunchSpec) -> OperationsResult<Self> {
        Self::open_with_stop_timeout(operations, launch, DEFAULT_STOP_WAIT)
    }

    /// Trusted host timeout, never operation-selected launch/authority input. The
    /// stop deadline starts only after actual native-write acknowledgement.
    pub fn open_with_stop_timeout(
        operations: Operations,
        launch: LaunchSpec,
        stop_wait: Duration,
    ) -> OperationsResult<Self> {
        Self::open_lifecycle(operations, GameLifecycle::new(launch), stop_wait)
    }

    /// Drive a game owned by the separate supervisor. Shutting this runtime down
    /// never stops that game; it only ends this agent's observation and dispatch.
    pub fn open_supervised(
        operations: Operations,
        remote: vesta_supervisor::RemoteProcess,
        readiness: vesta_game_runtime::readiness::ReadinessConfig,
        gate: super::game::StartGate,
    ) -> OperationsResult<Self> {
        Self::open_lifecycle(
            operations,
            GameLifecycle::supervised(remote)
                .with_readiness(readiness)
                .with_start_gate(gate),
            DEFAULT_STOP_WAIT,
        )
    }

    /// An agent-owned game with trusted readiness settings.
    pub fn open_with_readiness(
        operations: Operations,
        launch: LaunchSpec,
        readiness: vesta_game_runtime::readiness::ReadinessConfig,
        gate: super::game::StartGate,
    ) -> OperationsResult<Self> {
        Self::open_lifecycle(
            operations,
            GameLifecycle::new(launch)
                .with_readiness(readiness)
                .with_start_gate(gate),
            DEFAULT_STOP_WAIT,
        )
    }

    fn open_lifecycle(
        operations: Operations,
        game: GameLifecycle,
        stop_wait: Duration,
    ) -> OperationsResult<Self> {
        if stop_wait > MAX_SHUTDOWN_WAIT {
            return Err(OperationError::rejected(OperationErrorCode::InvalidRequest));
        }
        // Kernel owner is from this exact DB, acquires one OS lock, reconciles,
        // and retains a runtime DB lease before any native or driver work.
        let owner = operations.open_owner()?;
        let operations = owner.operations().clone();
        let start_block = game.start_block.clone();
        let (shared, players) = DriverState::new(&owner, &game);
        let (local_sender, local_receiver) = mpsc::sync_channel(1);
        let mut worker = Driver {
            local_receiver,
            owner,
            game,
            players,
            active: None,
            shared: shared.clone(),
            stop_wait,
            observation_uncertain: None,
            #[cfg(test)]
            after_dispatch: None,
        };
        let driver = thread::Builder::new()
            .name("vesta-operation-runner".into())
            .spawn(move || {
                // Driver's observation-only Drop retains every native child/IO
                // worker even during unwind. Panic never triggers auto-kill.
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker.run()));
                match result {
                    Ok(result) => result,
                    Err(_) => {
                        worker.shared.error("state_operation_worker_panicked");
                        worker.shared.stop();
                        Err(StateError::new("state_operation_worker_panicked").into())
                    }
                }
            })
            .map_err(|_| StateError::new("state_operation_worker_start_failed"))?;
        Ok(Self {
            operations,
            shared,
            driver: Some(driver),
            start_block,
            local: LocalRuntime {
                sender: local_sender,
            },
        })
    }

    /// While set, every start (including restarts and local CLI starts) is refused
    /// with `maintenance_active`. Held by stopped-server backups.
    pub fn start_block(&self) -> super::game::StartGate {
        self.start_block.clone()
    }

    pub fn local_handle(&self) -> LocalRuntime {
        self.local.clone()
    }

    #[cfg(test)]
    pub(super) fn player_queries(&self) -> &QueryEndpoint {
        &self.shared.players
    }
    #[cfg(test)]
    pub(super) fn source_revision(&self) -> OperationsResult<SourceRevision> {
        self.shared.view.revision()
    }
    pub fn game_snapshot(&self) -> OperationsResult<ProcessSnapshot> {
        self.shared.view.snapshot()
    }
    pub fn status(&self) -> OperationsResult<RunnerStatus> {
        let observation = self
            .shared
            .observation
            .lock()
            .map_err(|_| StateError::new("state_operation_runner_unavailable"))?;
        Ok(RunnerStatus {
            dispatch_stopped: self.shared.stopping.load(Ordering::Acquire),
            driver_finished: self.driver.as_ref().is_none_or(JoinHandle::is_finished),
            active_operation: observation.active.clone(),
            owned_generation: self.shared.view.snapshot()?.generation,
            last_error_code: observation.last_error,
        })
    }
    /// Synchronously rejects new claims and delayed native effect gates. Does
    /// not claim that a callback, child or IO thread has actually returned.
    pub fn request_shutdown(&self) {
        self.shared.stop();
    }

    /// Bounded observation only. A timeout leaves the exact driver, owner lease,
    /// claims and native handles attached to this runtime for later observation.
    pub fn shutdown(&mut self, timeout: Duration) -> OperationsResult<ShutdownOutcome> {
        if timeout > MAX_SHUTDOWN_WAIT {
            return Err(OperationError::rejected(OperationErrorCode::InvalidRequest));
        }
        let until = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| OperationError::rejected(OperationErrorCode::InvalidRequest))?;
        self.request_shutdown();
        loop {
            if self.driver.as_ref().is_none_or(JoinHandle::is_finished) {
                if let Some(handle) = self.driver.take() {
                    return handle
                        .join()
                        .map_err(|_| StateError::new("state_operation_worker_panicked"))?
                        .map(|()| ShutdownOutcome::Complete);
                }
                return Ok(ShutdownOutcome::Complete);
            }
            let now = Instant::now();
            if now >= until {
                return Ok(ShutdownOutcome::TimedOut(self.status()?));
            }
            thread::sleep(DRIVER_INTERVAL.min(until.saturating_duration_since(now)));
        }
    }
}
impl Drop for OperationRuntime {
    fn drop(&mut self) {
        self.request_shutdown();
        if let Some(handle) = self.driver.take() {
            // Deliberately unbounded. No timeout, detach, synthetic completion or
            // native stop/kill is an acceptable last-resort ownership substitute.
            let _ = handle.join();
        }
    }
}

enum Active {
    CommandWrite {
        claim: Claim,
        receipt: GuardedInput<OperationError>,
    },
    StopWrite {
        claim: Claim,
        generation: GenerationId,
        receipt: GuardedInput<OperationError>,
        restart: bool,
    },
    Exit {
        claim: Claim,
        generation: GenerationId,
        uncertain: Option<&'static str>,
        stop_deadline: Option<Instant>,
        restart: bool,
    },
}
impl Active {
    fn id(&self) -> &OperationId {
        match self {
            Self::CommandWrite { claim, .. }
            | Self::StopWrite { claim, .. }
            | Self::Exit { claim, .. } => claim.id(),
        }
    }
}
struct Driver {
    local_receiver: mpsc::Receiver<LocalRequest>,
    owner: OwnerSession,
    game: GameLifecycle,
    players: QueryDriver,
    active: Option<Active>,
    shared: Arc<DriverState>,
    stop_wait: Duration,
    // Sticky loss of native observation proof. A later successful poll cannot
    // retroactively turn an uncertain effect into a known timeout/success.
    // Typed native classification, never a database error namespace.
    observation_uncertain: Option<RuntimeError>,
    #[cfg(test)]
    after_dispatch: Option<fn(&mut Driver)>,
}
impl Driver {
    fn note_observation_failure(&mut self, error: RuntimeError) {
        self.observation_uncertain.get_or_insert(error);
        self.game.view.invalidate_revision();
        self.shared.error(error.code());
        self.shared.stop();
    }

    fn retain_observation_failure(&self, first_failure: &mut Option<OperationError>) {
        if let Some(error) = self.observation_uncertain {
            first_failure.get_or_insert_with(|| OperationError::from(error));
        }
    }

    fn run(&mut self) -> OperationsResult<()> {
        let mut first_failure: Option<OperationError> =
            self.observation_uncertain.map(OperationError::from);
        loop {
            if let Err(error) = self.game.observe() {
                self.note_observation_failure(error);
                // No stale Running/Stopping snapshot can expire a stop deadline.
                // Exact claims stay owned until later real exit + every IO join.
                first_failure.get_or_insert_with(|| OperationError::from(error));
            }
            if let Err(error) = self.game.publish() {
                self.shared.error(error.code());
                self.shared.stop();
                first_failure.get_or_insert(error);
            }
            // Observation only: readiness failures never stop dispatch or effects.
            let _ = self.game.track_readiness();
            if let Err(error) = self.advance() {
                self.shared.error(error.code());
                self.shared.stop();
                first_failure.get_or_insert(error);
            }
            if let Some(error) = self.players.advance(
                &mut self.game,
                self.active.is_none(),
                self.observation_uncertain.is_some(),
            ) {
                self.note_observation_failure(error);
                first_failure.get_or_insert_with(|| OperationError::from(error));
            }
            self.shared
                .active(self.active.as_ref().map(|active| active.id().clone()));
            if !self.owner.healthy() {
                self.shared.stop();
            }
            if let Ok(request) = self.local_receiver.try_recv() {
                let result = if self.shared.stopping.load(Ordering::Acquire)
                    || self.observation_uncertain.is_some()
                {
                    Err("local_runtime_unavailable")
                } else if self.active.is_some() || self.players.writer_pending() {
                    Err("local_runtime_busy")
                } else {
                    self.execute_local(request.action)
                };
                let _ = request.reply.send(result);
                // Do not dispatch another effect in the same observation cycle.
                continue;
            }
            if self.shared.stopping.load(Ordering::Acquire) {
                self.owner.stop_claiming();
                if !self.game.is_local()
                    && let Some(Active::Exit { claim, .. }) = self.active.take()
                {
                    // The supervisor, not this agent, owns the game. Waiting for its
                    // exit could block shutdown forever; the outcome is unknown here.
                    if let Err(error) = self.owner.finish(
                        claim,
                        FinishOutcome::Uncertain {
                            code: "game_supervised_outcome_unobserved",
                        },
                    ) {
                        first_failure.get_or_insert(error);
                    }
                }
                if self.active.is_none() && !self.game.retains_native_child() {
                    return first_failure.map_or(Ok(()), Err);
                }
            } else if self.active.is_none() && !self.players.writer_pending() {
                match self.owner.claim_next_with_revision(&self.game.revision()) {
                    Ok(Some(claim)) => {
                        self.shared.active(Some(claim.id().clone()));
                        let result = self.execute(claim);
                        #[cfg(test)]
                        if let Some(probe) = self.after_dispatch.take() {
                            probe(self);
                        }
                        // Internal native polls can lose observation proof while
                        // execute records the task outcome and returns Ok. Keep
                        // that typed failure even if later polls/joins succeed.
                        self.retain_observation_failure(&mut first_failure);
                        if let Err(error) = result {
                            self.shared.error(error.code());
                            self.shared.stop();
                            first_failure.get_or_insert(error);
                        }
                    }
                    Ok(None) => (),
                    Err(error) => {
                        // An ordinary shutdown can set the exact owner stopping
                        // flag between this loop's dispatch check and the claim.
                        // That refusal is clean stop, not a driver failure: no
                        // claim was taken and no effect gate was reached. Every
                        // other error, and any prior failure, is preserved.
                        match error {
                            OperationError::Rejected {
                                code: OperationErrorCode::OwnerStopping,
                                ..
                            } => self.shared.stop(),
                            error => {
                                self.shared.error(error.code());
                                self.shared.stop();
                                first_failure.get_or_insert(error);
                            }
                        }
                    }
                }
            }
            thread::sleep(DRIVER_INTERVAL);
        }
    }

    fn execute_local(&mut self, action: LocalAction) -> Result<(), &'static str> {
        use vesta_state::audit::{AuditActor, AuditRecord};
        let audit = |outcome: &str| {
            let record = AuditRecord::of(
                AuditActor::system("os-local-cli".into()).map_err(|_| "local_audit_failed")?,
                match action {
                    LocalAction::Start => "local.game.start",
                    LocalAction::Stop => "local.game.stop",
                    LocalAction::Restart => "local.game.restart",
                }
                .into(),
                "configured-game".into(),
                outcome.into(),
                None,
            )
            .map_err(|_| "local_audit_failed")?;
            self.owner
                .operations()
                .audit
                .append_record(&record)
                .map_err(|_| "local_audit_failed")?;
            Ok::<(), &'static str>(())
        };
        audit("admitted")?;
        let result = self.local_effect(action);
        // Audit failures after an effect are explicitly uncertain, never a retry permit.
        let record = AuditRecord::of(
            AuditActor::system("os-local-cli".into()).map_err(|_| "local_audit_failed")?,
            "local.game.outcome".into(),
            "configured-game".into(),
            if result.is_ok() {
                "succeeded"
            } else {
                "refused_or_failed"
            }
            .into(),
            None,
        )
        .map_err(|_| "local_audit_failed")?;
        self.owner
            .operations()
            .audit
            .append_record(&record)
            .map_err(|_| "local_outcome_uncertain")?;
        result
    }

    fn local_effect(&mut self, action: LocalAction) -> Result<(), &'static str> {
        if !self.owner.healthy() {
            return Err("local_runtime_unavailable");
        }
        if action != LocalAction::Start {
            let generation = self
                .game
                .process
                .snapshot()
                .generation
                .ok_or("local_game_not_running")?;
            match self.game.process.stop(generation, self.stop_wait) {
                Ok(WaitOutcome::Exited(_)) => {}
                Ok(WaitOutcome::TimedOut(_)) => {
                    let _ = self.game.publish();
                    return Err("local_stop_timed_out_no_force_kill");
                }
                Err(error) => {
                    if error == RuntimeError::ObserveFailed {
                        self.note_observation_failure(error);
                    }
                    let _ = self.game.publish();
                    return Err("local_graceful_stop_failed");
                }
            }
        }
        if action != LocalAction::Stop {
            let result = self.game.start(|_| {
                if self.shared.stopping.load(Ordering::Acquire) || !self.owner.healthy() {
                    Err(OperationError::rejected(OperationErrorCode::OwnerStopping))
                } else {
                    Ok(())
                }
            });
            if matches!(
                result,
                Err(GuardedError::Runtime(RuntimeError::ObserveFailed))
            ) {
                self.note_observation_failure(RuntimeError::ObserveFailed);
            }
            self.game.publish().map_err(|_| "local_outcome_uncertain")?;
            result.map_err(|_| "local_start_refused")?;
        }
        self.game.publish().map_err(|_| "local_outcome_uncertain")
    }

    fn execute(&mut self, claim: Claim) -> OperationsResult<()> {
        let command = match self.owner.begin_run(&claim, &self.game.revision()) {
            Ok(command) => command,
            Err(error) => return self.refused(claim, error),
        };
        match command {
            GameCommand::Console {
                expected_generation,
                line,
            } => {
                let native = self.owner.native_effect_guard(&claim)?;
                let view = self.game.view.clone();
                match self.game.process.queue_command_guarded(
                    expected_generation,
                    &line,
                    move || native.begin_effect(&view.revision()?),
                ) {
                    Ok(receipt) => self.active = Some(Active::CommandWrite { claim, receipt }),
                    Err(error) => {
                        if error == RuntimeError::ObserveFailed {
                            self.note_observation_failure(error);
                        }
                        self.owner
                            .finish(claim, FinishOutcome::Failed { code: error.code() })?;
                    }
                }
            }
            GameCommand::Start => {
                let result = self
                    .game
                    .start(|revision| self.owner.begin_effect(&claim, revision));
                if matches!(
                    &result,
                    Err(GuardedError::Runtime(RuntimeError::ObserveFailed))
                ) {
                    self.note_observation_failure(RuntimeError::ObserveFailed);
                }
                // Publish only outside the DB writer. Even StartupUncertain
                // retains an actual generation and must never look stopped.
                let publication = self.game.publish();
                match result {
                    Ok(generation) => self.succeeded(claim, generation)?,
                    Err(GuardedError::Guard(error)) => self.refused(claim, error)?,
                    Err(GuardedError::Runtime(RuntimeError::StartupUncertain)) => {
                        if let Some(generation) = self.game.process.snapshot().generation {
                            self.active = Some(Active::Exit {
                                claim,
                                generation,
                                uncertain: Some(RuntimeError::StartupUncertain.code()),
                                stop_deadline: None,
                                restart: false,
                            });
                        } else {
                            self.owner.finish(
                                claim,
                                FinishOutcome::Uncertain {
                                    code: RuntimeError::StartupUncertain.code(),
                                },
                            )?;
                        }
                    }
                    Err(GuardedError::Runtime(error)) => {
                        self.owner
                            .finish(claim, FinishOutcome::Failed { code: error.code() })?;
                    }
                }
                publication?;
            }
            GameCommand::Stop {
                expected_generation,
            }
            | GameCommand::Restart {
                expected_generation,
            } => {
                let restart = matches!(command, GameCommand::Restart { .. });
                let native = match self.owner.native_effect_guard(&claim) {
                    Ok(native) => native,
                    Err(error) => return self.refused(claim, error),
                };
                let view = self.game.view.clone();
                // Callback owns exact kernel core/DB lease/OS lock. The reserved
                // native generation cannot change before this writer returns.
                let result = self
                    .game
                    .process
                    .queue_stop_guarded(expected_generation, move || {
                        native.begin_effect(&view.revision()?)
                    });
                match result {
                    Ok(receipt) => {
                        self.active = Some(Active::StopWrite {
                            claim,
                            generation: expected_generation,
                            receipt,
                            restart,
                        })
                    }
                    Err(error) => {
                        if error == RuntimeError::ObserveFailed {
                            // Admission's native poll is subject to the same
                            // sticky proof-loss rule as run/start/kill polls.
                            self.note_observation_failure(error);
                        }
                        // No callback was admitted and no native effect ran.
                        self.owner
                            .finish(claim, FinishOutcome::Failed { code: error.code() })?;
                    }
                }
            }
            GameCommand::ForceKill {
                expected_generation,
            } => {
                let mut effect_marked = false;
                let result = self.game.kill(expected_generation, |revision| {
                    self.owner.begin_effect(&claim, revision)?;
                    effect_marked = true;
                    Ok(())
                });
                if matches!(
                    &result,
                    Err(GuardedError::Runtime(RuntimeError::ObserveFailed))
                ) {
                    self.note_observation_failure(RuntimeError::ObserveFailed);
                }
                let publication = self.game.publish();
                match result {
                    Ok(WaitOutcome::Exited(_)) => self.succeeded(claim, expected_generation)?,
                    Ok(WaitOutcome::TimedOut(_)) => {
                        self.active = Some(Active::Exit {
                            claim,
                            generation: expected_generation,
                            uncertain: None,
                            stop_deadline: None,
                            restart: false,
                        })
                    }
                    Err(GuardedError::Guard(error)) => self.refused(claim, error)?,
                    Err(GuardedError::Runtime(error)) => {
                        if effect_marked
                            && self.game.process.snapshot().generation == Some(expected_generation)
                        {
                            self.active = Some(Active::Exit {
                                claim,
                                generation: expected_generation,
                                uncertain: Some(error.code()),
                                stop_deadline: None,
                                restart: false,
                            });
                        } else if effect_marked && error == RuntimeError::ThreadFailed {
                            // A supervised kill was sent but its reply was lost; the game
                            // being gone now does not prove this request had no effect.
                            self.owner
                                .finish(claim, FinishOutcome::Uncertain { code: error.code() })?;
                        } else {
                            self.owner
                                .finish(claim, FinishOutcome::Failed { code: error.code() })?;
                        }
                    }
                }
                publication?;
            }
        }
        Ok(())
    }

    fn advance(&mut self) -> OperationsResult<()> {
        let Some(active) = self.active.take() else {
            return Ok(());
        };
        match active {
            Active::CommandWrite { claim, mut receipt } => match receipt.try_result() {
                None => self.active = Some(Active::CommandWrite { claim, receipt }),
                Some(Ok(())) => {
                    self.owner.finish(
                        claim,
                        FinishOutcome::Succeeded {
                            result_json: Some("{\"delivery\":\"sent\"}".into()),
                        },
                    )?;
                }
                Some(Err(GuardedError::Guard(error))) => self.refused(claim, error)?,
                Some(Err(GuardedError::Runtime(error))) => {
                    self.owner
                        .finish(claim, FinishOutcome::Uncertain { code: error.code() })?;
                }
            },
            Active::StopWrite {
                claim,
                generation,
                mut receipt,
                restart,
            } => match receipt.try_result() {
                None => {
                    self.active = Some(Active::StopWrite {
                        claim,
                        generation,
                        receipt,
                        restart,
                    });
                }
                Some(Ok(())) => {
                    // Actual native write returned, unlike queue acceptance.
                    let acknowledgement = self.game.process.acknowledge_guarded_stop(generation);
                    // Only the fixed classification is recorded as uncertainty.
                    let uncertain = self
                        .observation_uncertain
                        .map(RuntimeError::code)
                        .or_else(|| acknowledgement.err().map(RuntimeError::code));
                    self.active = Some(Active::Exit {
                        claim,
                        generation,
                        uncertain,
                        restart,
                        stop_deadline: if uncertain.is_none() {
                            Instant::now().checked_add(self.stop_wait)
                        } else {
                            None
                        },
                    });
                }
                Some(Err(GuardedError::Guard(error))) => self.refused(claim, error)?,
                Some(Err(GuardedError::Runtime(error))) => {
                    // Partial native write or lost writer proof is uncertainty.
                    // Retain exact claim until actual child exit + every IO join.
                    self.active = Some(Active::Exit {
                        claim,
                        generation,
                        uncertain: Some(error.code()),
                        stop_deadline: None,
                        restart: false,
                    });
                }
            },
            Active::Exit {
                claim,
                generation,
                uncertain,
                stop_deadline,
                restart,
            } => {
                let uncertain = self
                    .observation_uncertain
                    .map(RuntimeError::code)
                    .or(uncertain);
                let stop_deadline = if uncertain.is_none() {
                    stop_deadline
                } else {
                    None
                };
                let snapshot = self.game.process.snapshot();
                if snapshot.generation.is_some() {
                    if stop_deadline_elapsed(
                        snapshot.phase,
                        uncertain,
                        stop_deadline,
                        Instant::now(),
                    ) {
                        // The actual writer request returned before this timer
                        // began. Release ONLY this completed operation's claim;
                        // runtime still owns the exact child, IO joins, DB lease
                        // and OS lock, so a fresh confirmed ForceKill can follow.
                        self.owner.finish(
                            claim,
                            FinishOutcome::Failed {
                                code: "game_stop_timeout",
                            },
                        )?;
                    } else {
                        self.active = Some(Active::Exit {
                            claim,
                            generation,
                            uncertain,
                            stop_deadline,
                            restart,
                        });
                    }
                } else if snapshot
                    .last_exit
                    .is_some_and(|exit| exit.generation == generation)
                {
                    if let Some(code) = uncertain {
                        self.owner
                            .finish(claim, FinishOutcome::Uncertain { code })?;
                    } else if restart {
                        let result = self
                            .game
                            .start(|revision| self.owner.continue_restart(&claim, revision));
                        let publication = self.game.publish();
                        match result {
                            Ok(next_generation) => self.succeeded(claim, next_generation)?,
                            Err(GuardedError::Guard(error)) => self.refused(claim, error)?,
                            Err(GuardedError::Runtime(error)) => {
                                self.owner.finish(
                                    claim,
                                    FinishOutcome::Uncertain { code: error.code() },
                                )?;
                            }
                        }
                        publication?;
                    } else {
                        self.succeeded(claim, generation)?;
                    }
                } else {
                    self.owner.finish(
                        claim,
                        FinishOutcome::Uncertain {
                            code: "game_generation_lost",
                        },
                    )?;
                }
            }
        }
        Ok(())
    }
    fn succeeded(&mut self, claim: Claim, generation: GenerationId) -> OperationsResult<()> {
        // Only server-produced fixed result facts; no PID, argv, paths or stdin.
        let result_json = Some(format!("{{\"generation\":\"{}\"}}", generation.to_hex()));
        self.owner
            .finish(claim, FinishOutcome::Succeeded { result_json })?;
        Ok(())
    }
    fn refused(&mut self, claim: Claim, error: OperationError) -> OperationsResult<()> {
        match error {
            OperationError::Rejected {
                code: OperationErrorCode::OwnerStopping,
                ..
            } => {
                self.owner.release_unstarted(claim)?;
                Ok(())
            }
            OperationError::Rejected {
                code: OperationErrorCode::CancellationRequested,
                ..
            } => {
                self.owner.finish(claim, FinishOutcome::Cancelled)?;
                Ok(())
            }
            OperationError::Rejected { code, .. } => {
                self.owner
                    .finish(claim, FinishOutcome::Failed { code: code.code() })?;
                Ok(())
            }
            error => {
                // Infrastructure/commit failure is not an ordinary authorization
                // denial. Retain recovery uncertainty if marker proof is lost.
                let code = error.code();
                let _ = self.owner.finish(claim, FinishOutcome::Uncertain { code });
                Err(error)
            }
        }
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        self.shared.stop();
        self.owner.stop_claiming();
        // Keep owner+claim+pending receipt alive during this observation-only
        // drain, including panic cleanup. GameProcess's emergency Drop kill must
        // never execute while a real native generation remains in this adapter.
        while self.game.retains_native_child() {
            let _ = self.game.observe();
            let _ = self.game.publish();
            thread::sleep(DRIVER_INTERVAL);
        }
        // All native IO handles are joined before the process became stopped.
        // A pending receipt can only now be disposed without detaching work.
    }
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
