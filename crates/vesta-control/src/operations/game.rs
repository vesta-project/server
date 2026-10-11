//! Closed host lifecycle, not a handler registry or operation-selected launch plan.
//! The private driver is the sole mutable owner. A guarded stdin request reserves
//! that generation until its actual writer acknowledgement; it cannot be replaced
//! while the request or its IO workers are still owned.
use super::{OperationError, OperationErrorCode, OperationsResult, SourceRevision};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use vesta_game_runtime::{
    console::ConsoleCapture,
    process::{
        GameProcess, GenerationId, GuardedError, GuardedInput, LaunchSpec, ProcessPhase,
        ProcessSnapshot, RuntimeError, WaitOutcome,
    },
};
use vesta_state::StateError;
use vesta_supervisor::RemoteProcess;

/// Independently published, bounded native ownership facts. Reading this lock in
/// a DB transaction never acquires the process owner or calls process.poll().
pub(super) struct LiveGameView {
    snapshot: RwLock<ProcessSnapshot>,
    revision_usable: AtomicBool,
    started: RwLock<Option<(GenerationId, std::time::Instant)>>,
}
impl LiveGameView {
    fn new(snapshot: ProcessSnapshot) -> Self {
        Self {
            snapshot: RwLock::new(snapshot),
            revision_usable: AtomicBool::new(true),
            started: RwLock::new(None),
        }
    }
    pub(super) fn snapshot(&self) -> OperationsResult<ProcessSnapshot> {
        self.snapshot
            .read()
            .map(|value| *value)
            .map_err(|_| StateError::new("state_game_view_unavailable").into())
    }
    pub(super) fn revision(&self) -> OperationsResult<SourceRevision> {
        if !self.revision_usable.load(Ordering::Acquire) {
            return Err(OperationError::rejected(
                OperationErrorCode::SourceRevisionUnavailable,
            ));
        }
        Ok(revision(self.snapshot()?))
    }
    pub(super) fn invalidate_revision(&self) {
        // Retained ownership facts remain observable, but lost native observation
        // proof cannot mint/consume destructive confirmations from a stale view.
        self.revision_usable.store(false, Ordering::Release);
    }
    pub(super) fn observation(&self) -> OperationsResult<(ProcessSnapshot, Option<u64>)> {
        let started = self
            .started
            .read()
            .map_err(|_| StateError::new("state_game_view_unavailable"))?;
        let snapshot = self.snapshot()?;
        Ok((
            snapshot,
            started
                .as_ref()
                .map(|(_, instant)| instant.elapsed().as_secs()),
        ))
    }
    fn publish(&self, snapshot: ProcessSnapshot) -> OperationsResult<()> {
        let mut started = self
            .started
            .write()
            .map_err(|_| StateError::new("state_game_view_unavailable"))?;
        match snapshot.generation {
            Some(generation) if started.as_ref().is_none_or(|(old, _)| *old != generation) => {
                *started = Some((generation, std::time::Instant::now()))
            }
            None => *started = None,
            _ => {}
        }
        *self
            .snapshot
            .write()
            .map_err(|_| StateError::new("state_game_view_unavailable"))? = snapshot;
        Ok(())
    }
}

fn revision(snapshot: ProcessSnapshot) -> SourceRevision {
    // Exited-but-draining ownership is not a live destructive-confirmation
    // target. It still remains owned and refuses replacement/start natively.
    let generation = match snapshot.phase {
        ProcessPhase::Running | ProcessPhase::Stopping => snapshot.generation,
        ProcessPhase::Stopped | ProcessPhase::Draining => None,
    };
    SourceRevision::for_game(generation)
}

pub(super) struct GameLifecycle {
    // No caller can access mutable process ownership, launch paths or stdin.
    pub(super) process: ProcessBackend,
    pub(super) console: ConsoleCapture,
    launch: Option<LaunchSpec>,
    pub(super) view: Arc<LiveGameView>,
}
impl GameLifecycle {
    pub(super) fn new(launch: LaunchSpec) -> Self {
        let console = ConsoleCapture::new();
        let process = GameProcess::new(console.clone());
        let view = Arc::new(LiveGameView::new(process.snapshot()));
        Self {
            process: ProcessBackend::Local(process),
            console,
            launch: Some(launch),
            view,
        }
    }
    /// A game owned by the separate supervisor. Its launch definition lives there.
    pub(super) fn supervised(remote: RemoteProcess) -> Self {
        let console = remote.console();
        let view = Arc::new(LiveGameView::new(remote.snapshot()));
        Self {
            process: ProcessBackend::Supervised(remote),
            console,
            launch: None,
            view,
        }
    }
    pub(super) fn publish(&self) -> OperationsResult<()> {
        self.view.publish(self.process.snapshot())
    }
    pub(super) fn observe(&mut self) -> Result<ProcessSnapshot, RuntimeError> {
        self.process.poll()
    }
    pub(super) fn revision(&self) -> SourceRevision {
        revision(self.process.snapshot())
    }
    pub(super) fn has_owned_generation(&self) -> bool {
        self.process.snapshot().generation.is_some()
    }
    /// Whether this agent owns the game process itself (not a supervisor).
    pub(super) fn is_local(&self) -> bool {
        matches!(self.process, ProcessBackend::Local(_))
    }
    /// Whether runtime shutdown must wait for the game: only an agent-owned child.
    /// A supervised game deliberately keeps running after the agent stops.
    pub(super) fn retains_native_child(&self) -> bool {
        matches!(self.process, ProcessBackend::Local(_)) && self.has_owned_generation()
    }
    pub(super) fn start(
        &mut self,
        guard: impl FnOnce(&SourceRevision) -> OperationsResult<()>,
    ) -> Result<GenerationId, GuardedError<super::OperationError>> {
        let guard =
            |snapshot: ProcessSnapshot| guard(&SourceRevision::for_game(snapshot.generation));
        match (&mut self.process, &self.launch) {
            // &mut GameProcess is held before entering the DB authorization callback.
            // Native preflight/RNG/worker preparation happen before that callback.
            (ProcessBackend::Local(process), Some(launch)) => process.start_guarded(launch, guard),
            (ProcessBackend::Supervised(remote), _) => remote.start_guarded(guard),
            (ProcessBackend::Local(_), None) => Err(RuntimeError::InvalidLaunch.into()),
        }
    }
    pub(super) fn kill(
        &mut self,
        generation: GenerationId,
        guard: impl FnOnce(&SourceRevision) -> OperationsResult<()>,
    ) -> Result<WaitOutcome, GuardedError<super::OperationError>> {
        let guard =
            |snapshot: ProcessSnapshot| guard(&SourceRevision::for_game(snapshot.generation));
        match &mut self.process {
            ProcessBackend::Local(process) => {
                process.force_kill_guarded(generation, std::time::Duration::ZERO, guard)
            }
            ProcessBackend::Supervised(remote) => {
                remote.force_kill_guarded(generation, std::time::Duration::ZERO, guard)
            }
        }
    }
}

/// The one process owner a lifecycle drives: this agent's own child, or a game owned by
/// the separate supervisor. Both expose the same guarded, generation-bound effects.
pub(super) enum ProcessBackend {
    Local(GameProcess),
    Supervised(RemoteProcess),
}
impl ProcessBackend {
    pub(super) fn snapshot(&self) -> ProcessSnapshot {
        match self {
            Self::Local(process) => process.snapshot(),
            Self::Supervised(remote) => remote.snapshot(),
        }
    }
    pub(super) fn poll(&mut self) -> Result<ProcessSnapshot, RuntimeError> {
        match self {
            Self::Local(process) => process.poll(),
            Self::Supervised(remote) => remote.poll(),
        }
    }
    pub(super) fn stop(
        &mut self,
        generation: GenerationId,
        timeout: std::time::Duration,
    ) -> Result<WaitOutcome, RuntimeError> {
        match self {
            Self::Local(process) => process.stop(generation, timeout),
            Self::Supervised(remote) => remote.stop(generation, timeout),
        }
    }
    pub(super) fn queue_command_guarded<E, F>(
        &mut self,
        generation: GenerationId,
        line: &str,
        guard: F,
    ) -> Result<GuardedInput<E>, RuntimeError>
    where
        E: Send + 'static,
        F: FnMut() -> Result<(), E> + Send + 'static,
    {
        match self {
            Self::Local(process) => process.queue_command_guarded(generation, line, guard),
            Self::Supervised(remote) => remote.queue_command_guarded(generation, line, guard),
        }
    }
    pub(super) fn queue_stop_guarded<E, F>(
        &mut self,
        generation: GenerationId,
        guard: F,
    ) -> Result<GuardedInput<E>, RuntimeError>
    where
        E: Send + 'static,
        F: FnMut() -> Result<(), E> + Send + 'static,
    {
        match self {
            Self::Local(process) => process.queue_stop_guarded(generation, guard),
            Self::Supervised(remote) => remote.queue_stop_guarded(generation, guard),
        }
    }
    pub(super) fn queue_list_guarded<E, F>(
        &mut self,
        generation: GenerationId,
        guard: F,
    ) -> Result<GuardedInput<E>, RuntimeError>
    where
        E: Send + 'static,
        F: FnMut() -> Result<(), E> + Send + 'static,
    {
        match self {
            Self::Local(process) => process.queue_list_guarded(generation, guard),
            Self::Supervised(remote) => remote.queue_list_guarded(generation, guard),
        }
    }
    pub(super) fn acknowledge_guarded_stop(
        &mut self,
        generation: GenerationId,
    ) -> Result<(), RuntimeError> {
        match self {
            Self::Local(process) => process.acknowledge_guarded_stop(generation),
            Self::Supervised(remote) => remote.acknowledge_guarded_stop(generation),
        }
    }
}
