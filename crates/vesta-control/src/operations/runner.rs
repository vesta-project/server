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
};
use std::{
    fmt,
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
}
impl DriverState {
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
/// argv, cwd, URL, stdin text, handler or authority. Opening never starts a game
/// except by executing an already-admitted, currently authorized queued Start.
///
/// No manual tick is required. The bounded driver observes/polls and dispatches
/// the closed Start/Stop/ForceKill enum. There is no arbitrary public registry.
pub struct OperationRuntime {
    operations: Operations,
    shared: Arc<DriverState>,
    // Never detached on timeout. Taking this is allowed only for actual join.
    driver: Option<JoinHandle<OperationsResult<()>>>,
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
impl OperationRuntime {
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
        if stop_wait > MAX_SHUTDOWN_WAIT {
            return Err(OperationError::rejected(OperationErrorCode::InvalidRequest));
        }
        // Kernel owner is from this exact DB, acquires one OS lock, reconciles,
        // and retains a runtime DB lease before any native or driver work.
        let owner = operations.open_owner()?;
        let operations = owner.operations().clone();
        let game = GameLifecycle::new(launch);
        let shared = Arc::new(DriverState {
            stopping: owner.stop_flag(),
            observation: Mutex::new(Observation::default()),
            view: game.view.clone(),
        });
        let mut worker = Driver {
            owner,
            game,
            active: None,
            shared: shared.clone(),
            stop_wait,
            observation_uncertain: None,
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
        })
    }

    pub(super) fn operations(&self) -> &Operations {
        &self.operations
    }
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
    StopWrite {
        claim: Claim,
        generation: GenerationId,
        receipt: GuardedInput<OperationError>,
    },
    Exit {
        claim: Claim,
        generation: GenerationId,
        uncertain: Option<&'static str>,
        stop_deadline: Option<Instant>,
    },
}
impl Active {
    fn id(&self) -> &OperationId {
        match self {
            Self::StopWrite { claim, .. } | Self::Exit { claim, .. } => claim.id(),
        }
    }
}
struct Driver {
    owner: OwnerSession,
    game: GameLifecycle,
    active: Option<Active>,
    shared: Arc<DriverState>,
    stop_wait: Duration,
    // Sticky loss of native observation proof. A later successful poll cannot
    // retroactively turn an uncertain effect into a known timeout/success.
    // Typed native classification, never a database error namespace.
    observation_uncertain: Option<RuntimeError>,
}
impl Driver {
    fn note_observation_failure(&mut self, error: RuntimeError) {
        self.observation_uncertain.get_or_insert(error);
        self.game.view.invalidate_revision();
        self.shared.error(error.code());
        self.shared.stop();
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
            if let Err(error) = self.advance() {
                self.shared.error(error.code());
                self.shared.stop();
                first_failure.get_or_insert(error);
            }
            self.shared
                .active(self.active.as_ref().map(|active| active.id().clone()));
            if !self.owner.healthy() {
                self.shared.stop();
            }
            if self.shared.stopping.load(Ordering::Acquire) {
                self.owner.stop_claiming();
                if self.active.is_none() && !self.game.has_owned_generation() {
                    return first_failure.map_or(Ok(()), Err);
                }
            } else if self.active.is_none() {
                match self.owner.claim_next_with_revision(&self.game.revision()) {
                    Ok(Some(claim)) => {
                        self.shared.active(Some(claim.id().clone()));
                        if let Err(error) = self.execute(claim) {
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

    fn execute(&mut self, claim: Claim) -> OperationsResult<()> {
        let command = match self.owner.begin_run(&claim, &self.game.revision()) {
            Ok(command) => command,
            Err(error) => return self.refused(claim, error),
        };
        match command {
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
            } => {
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
                            });
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
            Active::StopWrite {
                claim,
                generation,
                mut receipt,
            } => match receipt.try_result() {
                None => {
                    self.active = Some(Active::StopWrite {
                        claim,
                        generation,
                        receipt,
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
                    });
                }
            },
            Active::Exit {
                claim,
                generation,
                uncertain,
                stop_deadline,
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
                        });
                    }
                } else if snapshot
                    .last_exit
                    .is_some_and(|exit| exit.generation == generation)
                {
                    if let Some(code) = uncertain {
                        self.owner
                            .finish(claim, FinishOutcome::Uncertain { code })?;
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
        while self.game.has_owned_generation() {
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
