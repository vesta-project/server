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
        GameProcess, GenerationId, GuardedError, LaunchSpec, ProcessPhase, ProcessSnapshot,
        RuntimeError, WaitOutcome,
    },
};
use vesta_state::StateError;

/// Independently published, bounded native ownership facts. Reading this lock in
/// a DB transaction never acquires the process owner or calls process.poll().
pub(super) struct LiveGameView {
    snapshot: RwLock<ProcessSnapshot>,
    revision_usable: AtomicBool,
}
impl LiveGameView {
    fn new(snapshot: ProcessSnapshot) -> Self {
        Self {
            snapshot: RwLock::new(snapshot),
            revision_usable: AtomicBool::new(true),
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
    fn publish(&self, snapshot: ProcessSnapshot) -> OperationsResult<()> {
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
    pub(super) process: GameProcess,
    launch: LaunchSpec,
    pub(super) view: Arc<LiveGameView>,
}
impl GameLifecycle {
    pub(super) fn new(launch: LaunchSpec) -> Self {
        let process = GameProcess::new(ConsoleCapture::new());
        let view = Arc::new(LiveGameView::new(process.snapshot()));
        Self {
            process,
            launch,
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
    pub(super) fn start(
        &mut self,
        guard: impl FnOnce(&SourceRevision) -> OperationsResult<()>,
    ) -> Result<GenerationId, GuardedError<super::OperationError>> {
        // &mut GameProcess is held before entering the DB authorization callback.
        // Native preflight/RNG/worker preparation happen before that callback.
        self.process.start_guarded(&self.launch, |snapshot| {
            guard(&SourceRevision::for_game(snapshot.generation))
        })
    }
    pub(super) fn kill(
        &mut self,
        generation: GenerationId,
        guard: impl FnOnce(&SourceRevision) -> OperationsResult<()>,
    ) -> Result<WaitOutcome, GuardedError<super::OperationError>> {
        self.process
            .force_kill_guarded(generation, std::time::Duration::ZERO, |snapshot| {
                guard(&SourceRevision::for_game(snapshot.generation))
            })
    }
}
