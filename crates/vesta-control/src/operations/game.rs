//! Closed host lifecycle, not a handler registry or operation-selected launch plan.
//! The private driver is the sole mutable owner. A guarded stdin request reserves
//! that generation until its actual writer acknowledgement; it cannot be replaced
//! while the request or its IO workers are still owned.
use super::readiness::{ReadinessView, ReadinessWatch};
use super::{OperationError, OperationErrorCode, OperationsResult, SourceRevision};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use vesta_game_runtime::readiness::{Readiness, ReadinessConfig};
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
    /// Supervisor conditions; `None` when this agent owns the game itself.
    supervision: RwLock<Option<vesta_supervisor::wire::Health>>,
    readiness: RwLock<ReadinessView>,
    native_identity: RwLock<Option<(GenerationId, u32, u64)>>,
    supervisor_record_source: RwLock<Option<(String, String)>>,
}
impl LiveGameView {
    pub(super) fn is_supervised(&self) -> bool {
        self.supervisor_record_source
            .read()
            .map(|source| source.is_some())
            .unwrap_or(true)
    }
    fn new(snapshot: ProcessSnapshot) -> Self {
        Self {
            snapshot: RwLock::new(snapshot),
            revision_usable: AtomicBool::new(true),
            started: RwLock::new(None),
            supervision: RwLock::new(None),
            native_identity: RwLock::new(None),
            supervisor_record_source: RwLock::new(None),
            readiness: RwLock::new(ReadinessView {
                readiness: Readiness::Stopped,
                probed: false,
                players: None,
                generation: None,
            }),
        }
    }
    pub(super) fn native_identity(&self) -> Option<(GenerationId, u32, u64)> {
        let generation = self.snapshot().ok()?.generation?;
        if let Some((state, identity)) = self.supervisor_record_source.read().ok()?.as_ref() {
            let state = vesta_state::StateDirectory::new(state, None).ok()?;
            let record = vesta_supervisor::record::read(&state.supervisor().ok()?).ok()??;
            return (record.generation == generation.to_hex()
                && !record.sandbox_owner
                && record.launch_identity == *identity
                && record.liveness() == vesta_supervisor::native::Liveness::Alive)
                .then_some((generation, record.pid, record.process_start));
        }
        let value = *self.native_identity.read().ok()?;
        value.filter(|(g, _, _)| *g == generation)
    }
    pub(super) fn readiness(&self) -> OperationsResult<ReadinessView> {
        self.readiness
            .read()
            .map(|value| *value)
            .map_err(|_| StateError::new("state_game_view_unavailable").into())
    }
    pub(super) fn supervision(&self) -> OperationsResult<Option<vesta_supervisor::wire::Health>> {
        self.supervision
            .read()
            .map(|value| *value)
            .map_err(|_| StateError::new("state_game_view_unavailable").into())
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

/// Maintenance exclusion for game starts. `block` returns once any start that was
/// already past the check has published its generation; until the guard drops, every
/// start (operations, local CLI, restart continuation) is refused.
#[derive(Clone, Default)]
pub struct StartGate {
    inner: Arc<std::sync::Mutex<u32>>,
}
impl std::fmt::Debug for StartGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StartGate")
    }
}
impl StartGate {
    pub fn block(&self) -> StartBlock {
        *self.inner.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        StartBlock { gate: self.clone() }
    }
    pub fn blocked(&self) -> bool {
        *self.inner.lock().unwrap_or_else(|p| p.into_inner()) > 0
    }
}
pub struct StartBlock {
    gate: StartGate,
}
impl Drop for StartBlock {
    fn drop(&mut self) {
        let mut count = self.gate.inner.lock().unwrap_or_else(|p| p.into_inner());
        *count = count.saturating_sub(1);
    }
}

pub(super) struct GameLifecycle {
    pub(super) content_files: Option<crate::files::FileBrowser>,
    // No caller can access mutable process ownership, launch paths or stdin.
    pub(super) process: ProcessBackend,
    pub(super) console: ConsoleCapture,
    launch: Option<LaunchSpec>,
    pub(super) view: Arc<LiveGameView>,
    readiness: ReadinessWatch,
    /// Console cursor just before the latest local launch: later lines are its output.
    local_floor: Option<(GenerationId, u64)>,
    local_identity: Option<(GenerationId, u32, u64)>,
    /// Held by maintenance (stopped backups) to refuse every start, including restarts.
    pub(super) start_block: StartGate,
    offline: Option<vesta_game_runtime::offline::Responder>,
    offline_uncertain: bool,
}
impl GameLifecycle {
    pub(super) fn replace_stopped_launch(
        &mut self,
        launch: LaunchSpec,
        identity: String,
        staged: String,
        token: Option<String>,
    ) -> Result<(), &'static str> {
        let snapshot = self.process.snapshot();
        if !self.start_block.blocked()
            || snapshot.generation.is_some()
            || snapshot.phase != ProcessPhase::Stopped
        {
            return Err("launch_replacement_not_stopped");
        }
        match &mut self.process {
            ProcessBackend::Local(_) => self.launch = Some(launch),
            ProcessBackend::Supervised(remote) => {
                remote.replace_stopped_launch(
                    identity,
                    staged,
                    token.ok_or("launch_replacement_no_hold")?,
                )?;
                let (state, identity) = remote.binding();
                *self
                    .view
                    .supervisor_record_source
                    .write()
                    .map_err(|_| "launch_replacement_unavailable")? =
                    Some((state.to_owned(), identity.to_owned()));
            }
        }
        self.publish()
            .map_err(|_| "launch_replacement_unavailable")?;
        Ok(())
    }
    pub(super) fn new(launch: LaunchSpec) -> Self {
        let console = ConsoleCapture::new();
        let process = GameProcess::new(console.clone());
        let view = Arc::new(LiveGameView::new(process.snapshot()));
        Self {
            readiness: ReadinessWatch::new(ReadinessConfig::default(), console.clone()),
            local_floor: None,
            local_identity: None,
            start_block: StartGate::default(),
            content_files: None,
            offline: None,
            offline_uncertain: false,
            process: ProcessBackend::Local(process),
            console,
            launch: Some(launch),
            view,
        }
    }
    /// A game owned by the separate supervisor. Its launch definition lives there.
    pub(super) fn supervised(remote: RemoteProcess) -> Self {
        let console = remote.console();
        let mut view = LiveGameView::new(remote.snapshot());
        let (state, identity) = remote.metrics_record_source();
        view.supervisor_record_source = RwLock::new(Some((state.to_owned(), identity.to_owned())));
        let view = Arc::new(view);
        Self {
            readiness: ReadinessWatch::new(ReadinessConfig::default(), console.clone()),
            local_floor: None,
            local_identity: None,
            start_block: StartGate::default(),
            content_files: None,
            offline: None,
            offline_uncertain: false,
            process: ProcessBackend::Supervised(remote),
            console,
            launch: None,
            view,
        }
    }
    pub(super) fn publish(&self) -> OperationsResult<()> {
        if let ProcessBackend::Supervised(remote) = &self.process {
            *self
                .view
                .supervision
                .write()
                .map_err(|_| StateError::new("state_game_view_unavailable"))? =
                Some(remote.health());
        }
        let snapshot = self.process.snapshot();
        let identity = match &self.process {
            ProcessBackend::Local(_) => self.local_identity,
            ProcessBackend::Supervised(_) => None,
        }
        .filter(|(g, _, _)| {
            snapshot.generation == Some(*g)
                && matches!(
                    snapshot.phase,
                    ProcessPhase::Running | ProcessPhase::Stopping
                )
        });
        *self
            .view
            .native_identity
            .write()
            .map_err(|_| StateError::new("state_game_view_unavailable"))? = identity;
        self.view.publish(snapshot)
    }
    /// Share the host's maintenance gate (stopped backups block starts through it).
    pub(super) fn with_start_gate(mut self, gate: StartGate) -> Self {
        self.start_block = gate;
        self
    }
    /// Trusted host readiness settings (probe address, timeouts).
    pub(super) fn with_readiness(mut self, config: ReadinessConfig) -> Self {
        self.readiness = ReadinessWatch::new(config, self.console.clone());
        self
    }
    /// Advance readiness from console output, generation changes and probes.
    pub(super) fn track_readiness(&mut self) -> OperationsResult<()> {
        // Only a live process can be starting or ready; a draining exit is stopped.
        let snapshot = self.process.snapshot();
        let generation = snapshot.generation.filter(|_| {
            matches!(
                snapshot.phase,
                ProcessPhase::Running | ProcessPhase::Stopping
            )
        });
        let floor = generation.and_then(|generation| match &self.process {
            ProcessBackend::Local(_) => self
                .local_floor
                .filter(|(g, _)| *g == generation)
                .map(|(_, floor)| floor),
            ProcessBackend::Supervised(remote) => remote.console_floor(generation),
        });
        let view = self.readiness.track(generation, floor);
        *self
            .view
            .readiness
            .write()
            .map_err(|_| StateError::new("state_game_view_unavailable"))? = view;
        Ok(())
    }
    pub(super) fn observe(&mut self) -> Result<ProcessSnapshot, RuntimeError> {
        let snapshot = self.process.poll()?;
        if matches!(
            snapshot.phase,
            ProcessPhase::Stopped | ProcessPhase::Draining
        ) {
            self.local_identity = None;
        }
        Ok(snapshot)
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
        // The gate is held from the block check until the new generation is published,
        // so a maintenance block either sees this start or this start sees the block.
        let gate = self.start_block.clone();
        let blocked = gate.inner.lock().unwrap_or_else(|p| p.into_inner());
        if *blocked > 0 {
            return Err(GuardedError::Guard(OperationError::rejected(
                OperationErrorCode::MaintenanceActive,
            )));
        }
        let started = self.start_unblocked(guard);
        let _ = self.publish();
        drop(blocked);
        started
    }
    fn start_unblocked(
        &mut self,
        guard: impl FnOnce(&SourceRevision) -> OperationsResult<()>,
    ) -> Result<GenerationId, GuardedError<super::OperationError>> {
        if let Some(files) = &self.content_files {
            let snapshot = self.process.snapshot();
            if snapshot.phase != ProcessPhase::Stopped || snapshot.generation.is_some() {
                return Err(RuntimeError::AlreadyOwned.into());
            }
            files
                .apply_pending_content()
                .map_err(|_| GuardedError::Runtime(RuntimeError::ObserveFailed))?;
        }
        if self.offline_uncertain {
            return Err(RuntimeError::ObserveFailed.into());
        }
        // Test-only observation of the real start barrier. Never spawn an owned
        // fixture executable just to prove that pending content blocks start.
        #[cfg(test)]
        if RECOVERY_START_PROBE.with(|slot| slot.get().is_some()) {
            RECOVERY_START_PROBE.with(|slot| slot.set(Some(slot.get().unwrap() + 1)));
            return Err(RuntimeError::ObserveFailed.into());
        }
        if let Some(mut responder) = self.offline.take()
            && responder.close().is_err()
        {
            self.view.invalidate_revision();
            self.offline_uncertain = true;
            return Err(RuntimeError::ObserveFailed.into());
        }
        let guard =
            |snapshot: ProcessSnapshot| guard(&SourceRevision::for_game(snapshot.generation));
        match (&mut self.process, &self.launch) {
            // &mut GameProcess is held before entering the DB authorization callback.
            // Native preflight/RNG/worker preparation happen before that callback.
            (ProcessBackend::Local(process), Some(launch)) => {
                let floor = self.console.snapshot(0, 1).map(|page| page.latest_cursor);
                let before = process.snapshot().generation;
                let started = process.start_guarded(launch, guard);
                // Capture once immediately after spawn, while ownership retains the
                // unreaped child/native handle. Never learn a replacement PID later.
                if process.snapshot().generation != before {
                    self.local_identity = process.native_game_id().and_then(|pid| {
                        let generation = process.snapshot().generation?;
                        let start = vesta_supervisor::native::process_start(pid)?;
                        Some((generation, pid, start))
                    });
                }
                if let (Ok(generation), Ok(floor)) = (&started, floor) {
                    self.local_floor = Some((*generation, floor));
                }
                started
            }
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
    pub(super) fn configure_offline(
        &mut self,
        configuration: Option<vesta_game_runtime::offline::Configuration>,
    ) -> Result<Vec<std::net::IpAddr>, &'static str> {
        if let ProcessBackend::Supervised(remote) = &self.process {
            return remote.configure_offline(configuration);
        }
        if configuration.is_some() && self.process.snapshot().generation.is_some() {
            return Err("offline_process_unproven");
        }
        if let (Some(responder), Some(config)) = (&mut self.offline, &configuration)
            && responder.configuration().address == config.address
        {
            responder.update(config.clone())?;
            return Ok(responder.take_wakes());
        }
        if self.offline.as_ref().map(|r| r.configuration()) != configuration.as_ref() {
            if let Some(mut responder) = self.offline.take()
                && responder.close().is_err()
            {
                self.offline_uncertain = true;
                self.view.invalidate_revision();
                return Err("offline_release_uncertain");
            }
            if let Some(config) = configuration {
                self.offline = Some(vesta_game_runtime::offline::Responder::bind(config)?);
            }
        }
        Ok(self
            .offline
            .as_ref()
            .map(|r| r.take_wakes())
            .unwrap_or_default())
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
    pub(super) fn queue_sleep_stop_guarded<E, F>(
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
            Self::Supervised(remote) => remote.queue_sleep_stop_guarded(generation, guard),
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

#[cfg(test)]
mod metrics_tests {
    use super::*;
    #[test]
    fn metrics_supervised_identity_checks_protected_generation_launch_and_native_start() {
        let root = tempfile::tempdir().unwrap();
        let state = vesta_state::StateDirectory::new(root.path().join("state"), None).unwrap();
        state.prepare().unwrap();
        let directory = state.supervisor().unwrap();
        let generation = GenerationId::parse_hex(&"01".repeat(16)).unwrap();
        let pid = std::process::id();
        let start = vesta_supervisor::native::process_start(pid).unwrap();
        let mut record = vesta_supervisor::record::GenerationRecord {
            version: 1,
            generation: generation.to_hex(),
            pid,
            process_start: start,
            boot_id: vesta_supervisor::native::boot_id(),
            launch_identity: "02".repeat(32),
            sandbox_owner: false,
        };
        let mut snapshot = GameProcess::new(ConsoleCapture::new()).snapshot();
        snapshot.generation = Some(generation);
        snapshot.phase = ProcessPhase::Running;
        let mut view = LiveGameView::new(snapshot);
        view.supervisor_record_source = RwLock::new(Some((
            root.path().join("state").to_str().unwrap().to_owned(),
            record.launch_identity.clone(),
        )));
        vesta_supervisor::record::write(&directory, &record).unwrap();
        assert_eq!(view.native_identity(), Some((generation, pid, start)));
        for changed in 0..4 {
            vesta_supervisor::record::clear(&directory).unwrap();
            match changed {
                0 => record.generation = "03".repeat(16),
                1 => {
                    record.generation = generation.to_hex();
                    record.launch_identity = "04".repeat(32);
                }
                2 => {
                    record.launch_identity = "02".repeat(32);
                    record.process_start = start.wrapping_add(1);
                }
                _ => {
                    record.process_start = start;
                    record.sandbox_owner = true;
                }
            }
            vesta_supervisor::record::write(&directory, &record).unwrap();
            assert!(view.native_identity().is_none());
        }
        vesta_supervisor::record::clear(&directory).unwrap();
        assert!(view.native_identity().is_none());
    }
}

#[cfg(test)]
thread_local! {static RECOVERY_START_PROBE:std::cell::Cell<Option<usize>>=const{std::cell::Cell::new(None)};}
#[cfg(test)]
pub(crate) fn recovery_start_probe(
    files: crate::files::FileBrowser,
    root: &std::path::Path,
) -> usize {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            RECOVERY_START_PROBE.with(|slot| slot.set(None));
        }
    }
    let _reset = Reset;
    RECOVERY_START_PROBE.with(|slot| {
        assert!(slot.get().is_none());
        slot.set(Some(0));
    });
    let inert = root.join(".recovery-never-executable.fixture");
    std::fs::write(&inert, b"inert fixture, not executable").unwrap();
    let launch = LaunchSpec::new(
        inert,
        Vec::<String>::new(),
        root.to_path_buf(),
        vesta_game_runtime::process::GracefulStop::MinecraftCommand,
    )
    .unwrap();
    let mut lifecycle = GameLifecycle::new(launch);
    lifecycle.content_files = Some(files);
    let result = lifecycle.start_unblocked(|_| Ok(()));
    assert!(result.is_err(), "probe must never spawn");
    assert_eq!(lifecycle.process.snapshot().phase, ProcessPhase::Stopped);
    RECOVERY_START_PROBE.with(|slot| slot.get().unwrap())
}
