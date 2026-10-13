//! The supervisor: sole owner of one managed game process, with no network listener.
//!
//! It launches only its own trusted `LaunchSpec`, drains output continuously into the
//! bounded console ring whether or not an agent is connected, and survives agent
//! restarts. Requests are closed `wire::Op`s over the protected local endpoint. It
//! never kills or restarts the game on its own; agent disconnect has no effect. A live
//! game is never killed by supervisor exit, cancellation, panic or failure: ownership is
//! released by a destructor that preserves it (see `Preserved`).
use crate::{
    Result, native, record,
    record::{GenerationRecord, LaunchIntent},
    restart::{self, RestartPolicy, Restarts},
    save::{self, SaveAck, SaveLease},
    wire::{self, Health, Op, Reply},
};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use vesta_game_runtime::{
    console::{ConsoleCapture, ConsoleStream, MAX_PAGE_LINES},
    process::{GameProcess, GenerationId, GuardedError, GuardedInput, LaunchSpec, RuntimeError},
};
use vesta_state::{StateDirectory, artifacts::ArtifactDirectory};

/// Bounds both request arrival (an agent guard runs between connect and send) and reply.
const IO_WAIT: Duration = Duration::from_secs(10);
const DELIVERY_WAIT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const WAIT_STEP: Duration = Duration::from_millis(10);
pub(crate) const MAX_CONNECTIONS: usize = 4;

/// Trusted composition input. `launch_identity` binds agents to the same definition.
pub struct SupervisorConfig {
    pub state: StateDirectory,
    pub launch: LaunchSpec,
    pub launch_identity: String,
    /// Opt-in crash restart; `None` never restarts automatically.
    pub restart: Option<RestartPolicy>,
}

/// Owns the game process; dropping it never runs the emergency kill of a live child.
/// The I/O workers of a preserved child keep draining until the process ends.
struct Preserved(Option<GameProcess>);
impl Drop for Preserved {
    fn drop(&mut self) {
        if let Some(process) = self.0.take()
            && process.snapshot().generation.is_some()
        {
            std::mem::forget(process);
        }
    }
}
impl std::ops::Deref for Preserved {
    type Target = GameProcess;
    fn deref(&self) -> &GameProcess {
        self.0
            .as_ref()
            .expect("preserved process is present until drop")
    }
}
impl std::ops::DerefMut for Preserved {
    fn deref_mut(&mut self) -> &mut GameProcess {
        self.0
            .as_mut()
            .expect("preserved process is present until drop")
    }
}

struct Core {
    process: Preserved,
    console: ConsoleCapture,
    launch: LaunchSpec,
    launch_identity: String,
    state: String,
    directory: ArtifactDirectory,
    /// Generation whose record is published and must be cleared after its exit.
    recorded: Option<GenerationId>,
    /// Generation whose launch intent is still outstanding (record not published).
    intent_for: Option<GenerationId>,
    /// A previous supervisor's game that is alive or unobservable.
    orphan: Option<GenerationRecord>,
    /// An intent from a previous supervisor without a recorded outcome.
    unresolved: bool,
    observation_failed: bool,
    stopping: bool,
    restarts: Restarts,
    /// Generation for which a stop was delivered, or a graceful stop or kill applied.
    stop_admitted: Option<GenerationId>,
    /// A stop request is in flight; its exit is classified once delivery is known.
    stop_inflight: Option<GenerationId>,
    /// Console cursor just before the current generation was launched.
    generation_floor: u64,
    save_lease: Option<SaveLease>,
    save_delivery: Option<GuardedInput<()>>,
    save_delivered: bool,
    save_completed: Option<(String, bool)>,
    save_window: Option<String>,
    hold: Option<(String, Instant)>,
    /// The lease belongs to an orphan whose stdin is gone; it cannot be undone here.
    lease_stranded: bool,
    /// Last exited generation already classified as a stop or a crash.
    handled_exit: Option<GenerationId>,
    offline: Option<vesta_game_runtime::offline::Responder>,
}
impl Core {
    /// Observe, classify exits and fire a due automatic restart.
    fn observe(&mut self) {
        self.observe_state();
        if !self.stopping && self.hold.is_none() && self.restarts.take_due(Instant::now()) {
            // A failed automatic launch counts as another crash, so a broken launch
            // definition reaches the breaker instead of retrying forever.
            if self.launch().is_err() {
                self.restarts.on_crash(Instant::now(), restart::now_ms());
            }
        }
    }
    /// Observation only: never launches.
    fn observe_state(&mut self) {
        if self.process.poll().is_err() {
            // Sticky: later successful polls cannot prove what happened meanwhile.
            self.observation_failed = true;
        }
        if self
            .hold
            .as_ref()
            .is_some_and(|(_, until)| Instant::now() >= *until)
        {
            self.hold = None;
        }
        self.restarts.retry_persist();
        self.classify_exit();
        self.check_lease();
        let live = self.process.snapshot().generation;
        if let Some(generation) = self.recorded
            && live != Some(generation)
            && record::clear(&self.directory).is_ok()
        {
            self.recorded = None;
        }
        if let Some(generation) = self.intent_for
            && live != Some(generation)
            && record::clear_intent(&self.directory).is_ok()
        {
            self.intent_for = None;
        }
        if self
            .orphan
            .as_ref()
            .is_some_and(|orphan| orphan.liveness() == native::Liveness::Gone)
            && record::clear(&self.directory).is_ok()
        {
            self.orphan = None;
        }
    }
    /// End a lease whose generation is gone (send nothing) or whose time is up.
    fn check_lease(&mut self) {
        let Some(lease) = self.save_lease.clone() else {
            return;
        };
        if self.lease_stranded {
            if self.orphan.is_none() && !self.unresolved && save::clear(&self.directory).is_ok() {
                self.save_lease = None;
                self.lease_stranded = false;
            }
            return;
        }
        let live = self.process.snapshot().generation.map(GenerationId::to_hex);
        if live.as_deref() != Some(lease.generation.as_str()) {
            // A new or exited process: never send cleanup commands to it.
            if save::clear(&self.directory).is_ok() {
                self.save_lease = None;
                self.save_delivery = None;
                self.save_delivered = false;
            }
        } else if !lease.unresolved && (lease.ending || restart::now_ms() >= lease.expires_at_ms) {
            let _ = self.end_lease();
        }
    }
    /// Keep the durable ending obligation and receipt until actual delivery.
    fn end_lease(&mut self) -> std::result::Result<Option<bool>, Reply> {
        let Some(mut lease) = self.save_lease.clone() else {
            return Ok(Some(false));
        };
        if lease.unresolved {
            return Err(refused("supervisor_save_state_unknown"));
        }
        if !lease.ending {
            lease.ending = true;
            save::replace(&self.directory, &lease).map_err(refused)?;
            self.save_lease = Some(lease.clone());
        }
        if lease.restore_autosave && !self.save_delivered {
            if let Some(receipt) = self.save_delivery.as_mut() {
                match receipt.try_result() {
                    Some(Ok(())) => {
                        self.save_delivered = true;
                        self.save_delivery = None;
                    }
                    Some(Err(_)) => {
                        self.save_delivery = None;
                    }
                    None => return Ok(None),
                }
            }
            if !self.save_delivered {
                let generation = GenerationId::parse_hex(&lease.generation)
                    .map_err(|_| refused("supervisor_record_invalid"))?;
                self.save_delivery = Some(
                    self.process
                        .queue_command_guarded(generation, "save-on", || Ok::<(), ()>(()))
                        .map_err(runtime)?,
                );
                return Ok(None);
            }
        }
        save::clear(&self.directory).map_err(refused)?;
        self.save_completed = Some((lease.token, lease.restore_autosave));
        self.save_lease = None;
        self.save_delivered = false;
        Ok(Some(lease.restore_autosave))
    }
    fn active_lease(&self, id: GenerationId, token: &str) -> std::result::Result<SaveLease, Reply> {
        self.save_lease
            .clone()
            .filter(|lease| {
                lease.token == token
                    && lease.generation == id.to_hex()
                    && self.process.snapshot().generation == Some(id)
                    && !self.lease_stranded
                    && !lease.ending
                    && restart::now_ms() < lease.expires_at_ms
            })
            .ok_or_else(|| refused("supervisor_no_save_lease"))
    }
    /// Classify each finished generation exactly once: an admitted stop or a success
    /// status is a stop; anything else is a crash handed to the restart policy.
    fn classify_exit(&mut self) {
        let snapshot = self.process.snapshot();
        let Some(exit) = snapshot.last_exit else {
            return;
        };
        if snapshot.generation == Some(exit.generation)
            || self.handled_exit == Some(exit.generation)
            || self.stop_inflight == Some(exit.generation)
        {
            return;
        }
        self.handled_exit = Some(exit.generation);
        if self.stop_admitted == Some(exit.generation) || exit.success {
            return;
        }
        self.restarts.on_crash(Instant::now(), restart::now_ms());
    }
    fn admit_stop(&mut self, generation: GenerationId) {
        self.stop_admitted = Some(generation);
        self.restarts.cancel_pending();
    }
    /// A stop request was admitted but not yet delivered.
    fn stop_requested(&mut self, generation: GenerationId) {
        self.stop_inflight = Some(generation);
        self.restarts.cancel_pending();
    }
    /// Delivery outcome of an in-flight stop. Only a delivered stop makes a later
    /// unsuccessful exit a stop rather than a crash.
    fn stop_delivered(&mut self, generation: GenerationId, delivered: bool) {
        if self.stop_inflight == Some(generation) {
            self.stop_inflight = None;
        }
        if delivered {
            self.stop_admitted = Some(generation);
        }
    }
    fn health(&self) -> Health {
        Health {
            orphan_running: self.orphan.is_some(),
            launch_unresolved: self.unresolved || self.intent_for.is_some(),
            observation_failed: self.observation_failed,
            restart_enabled: self.restarts.enabled(),
            crash_loop: self.restarts.suspended(),
            recent_crashes: self.restarts.recent_crashes(restart::now_ms()),
            restart_pending_ms: self
                .restarts
                .pending_in(Instant::now())
                .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            restart_state_unsaved: self.restarts.unsaved(),
            save_lease_ms: self
                .save_lease
                .as_ref()
                .filter(|_| !self.lease_stranded)
                .map(|lease| {
                    u64::try_from(lease.expires_at_ms.saturating_sub(restart::now_ms()).max(0))
                        .unwrap_or(0)
                }),
            save_lease_stranded: self.lease_stranded,
            save_state_unknown: self
                .save_lease
                .as_ref()
                .is_some_and(|lease| lease.unresolved),
            maintenance_hold: self.hold.is_some(),
        }
    }
    fn idle(&self) -> bool {
        self.process.snapshot().generation.is_none()
            && self.orphan.is_none()
            && self.intent_for.is_none()
            && self.restarts.pending_in(Instant::now()).is_none()
            && self.save_lease.is_none()
            && self.hold.is_none()
    }
}

type Shared = Arc<Mutex<Core>>;

fn lock(core: &Shared) -> std::sync::MutexGuard<'_, Core> {
    core.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Recover durable launch state left by a previous supervisor. Only affirmative exit
/// evidence clears a record; an intent without a record stays unresolved.
fn recover(directory: &ArtifactDirectory) -> Result<(Option<GenerationRecord>, bool)> {
    let generation = record::read(directory)?;
    let intent = record::read_intent(directory)?;
    let had_record = generation.is_some();
    let orphan = match generation {
        Some(previous) if previous.held() => Some(previous),
        Some(_) => {
            record::clear(directory)?;
            None
        }
        None => None,
    };
    let unresolved = match intent {
        None => false,
        // The record published after this intent governs; the intent is retired.
        Some(_) if had_record => {
            record::clear_intent(directory)?;
            false
        }
        Some(_) => true,
    };
    Ok((orphan, unresolved))
}

/// Run until an accepted `Shutdown` request, or `stop` resolves while idle. Returns an
/// error before serving if another supervisor owns this state or its records are bad.
pub async fn run(config: SupervisorConfig, stop: impl Future<Output = ()>) -> Result<()> {
    let directory = config
        .state
        .supervisor()
        .map_err(|_| "supervisor_state_unavailable")?;
    let lock_name = vesta_state::artifacts::ArtifactName::new("supervisor.lock")
        .map_err(|_| "supervisor_state_unavailable")?;
    let lock_file = directory
        .open_or_create(&lock_name)
        .map_err(|_| "supervisor_state_unavailable")?;
    fs2::FileExt::try_lock_exclusive(&lock_file).map_err(|_| "supervisor_already_running")?;
    let (orphan, unresolved) = recover(&directory)?;
    let restarts = Restarts::load(directory.clone(), config.restart)?;
    // A lease from an earlier supervisor: undoable only if its game is gone.
    let mut save_lease = save::read(&directory)?;
    let lease_stranded = match (&save_lease, &orphan) {
        (Some(lease), Some(orphan)) if orphan.generation == lease.generation => true,
        (Some(_), _) if unresolved => true,
        (Some(_), _) => {
            save::clear(&directory)?;
            save_lease = None;
            false
        }
        (None, _) => false,
    };
    let endpoint = native::endpoint(
        config.state.path(),
        &config
            .state
            .path()
            .join(vesta_state::artifacts::SUPERVISOR_DIR),
    )?;
    let console = ConsoleCapture::new();
    let core: Shared = Arc::new(Mutex::new(Core {
        process: Preserved(Some(GameProcess::new(console.clone()))),
        console,
        launch: config.launch,
        launch_identity: config.launch_identity,
        state: config.state.path().to_string_lossy().into_owned(),
        directory,
        recorded: None,
        intent_for: None,
        orphan,
        unresolved,
        observation_failed: false,
        stopping: false,
        restarts,
        stop_admitted: None,
        stop_inflight: None,
        generation_floor: 0,
        save_lease,
        save_delivery: None,
        save_delivered: false,
        save_completed: None,
        save_window: None,
        hold: None,
        lease_stranded,
        handled_exit: None,
        offline: None,
    }));
    let result = serve(&endpoint, core, stop).await;
    drop(lock_file);
    result
}

async fn serve(
    endpoint: &std::path::Path,
    core: Shared,
    stop: impl Future<Output = ()>,
) -> Result<()> {
    let (shutdown, mut requested) = tokio::sync::watch::channel(false);
    let observer = {
        let core = core.clone();
        let mut requested = requested.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(POLL_INTERVAL);
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    _ = requested.changed() => return,
                }
                let core = core.clone();
                if tokio::task::spawn_blocking(move || lock(&core).observe())
                    .await
                    .is_err()
                {
                    return;
                }
            }
        })
    };
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut listener = Listener::bind(endpoint)?;
    tokio::pin!(stop);
    let mut stop_requested = false;
    let result = loop {
        if *requested.borrow() {
            break Ok(());
        }
        if stop_requested {
            // Close admission under the core lock only when nothing is live.
            let mut core = lock(&core);
            if core.idle() {
                core.stopping = true;
                break Ok(());
            }
        }
        let permit = tokio::select! {
            permit = permits.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => break Err("supervisor_worker_failed"),
            },
            _ = requested.changed() => continue,
        };
        let stream = tokio::select! {
            stream = listener.accept() => stream,
            _ = requested.changed() => continue,
            _ = &mut stop, if !stop_requested => { stop_requested = true; continue }
            _ = tokio::time::sleep(Duration::from_millis(250)), if stop_requested => continue,
        };
        // Endpoint failures never abandon a live game; keep retrying the listener.
        let Ok(mut stream) = stream else {
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        };
        let core = core.clone();
        let shutdown = shutdown.clone();
        let pool = listener.pool();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = handle(&mut stream, core, shutdown).await;
            pool.recycle(stream).await;
        });
    };
    let _ = shutdown.send(true);
    let _ = observer.await;
    // Wait for every in-flight request to return before ownership is released.
    let _ = permits.acquire_many(MAX_CONNECTIONS as u32).await;
    drop(listener);
    result
}

async fn handle<S>(
    stream: &mut S,
    core: Shared,
    shutdown: tokio::sync::watch::Sender<bool>,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let bytes = tokio::time::timeout(IO_WAIT, wire::read_async(stream, wire::MAX_REQUEST_FRAME))
        .await
        .map_err(|_| "supervisor_io_failed")??;
    let reply = match wire::decode_request(&bytes) {
        Err(code) => refused(code),
        Ok(request) => {
            // An admitted request always completes, even if the client disconnects.
            let reply = tokio::task::spawn_blocking(move || execute(&core, request.op))
                .await
                .unwrap_or_else(|_| refused("supervisor_worker_failed"));
            if reply == Reply::ShuttingDown {
                let _ = shutdown.send(true);
            }
            reply
        }
    };
    let framed = wire::encode_reply(reply)?;
    tokio::time::timeout(IO_WAIT, wire::write_async(stream, &framed))
        .await
        .map_err(|_| "supervisor_io_failed")?
}

fn refused(code: &str) -> Reply {
    Reply::Refused { code: code.into() }
}

fn generation(text: &str) -> std::result::Result<GenerationId, Reply> {
    GenerationId::parse_hex(text).map_err(|_| refused("supervisor_invalid_request"))
}

fn runtime(error: RuntimeError) -> Reply {
    refused(error.code())
}

/// Effects are refused once shutdown was accepted.
fn admit(core: &Core) -> std::result::Result<(), Reply> {
    if core.stopping {
        return Err(refused("supervisor_shutting_down"));
    }
    Ok(())
}

fn execute(core: &Shared, op: Op) -> Reply {
    let result = match op {
        Op::Hello => {
            let mut core = lock(core);
            core.observe();
            Ok(Reply::Hello(Box::new(wire::Hello {
                supervisor_pid: std::process::id(),
                launch_identity: core.launch_identity.clone(),
                state: core.state.clone(),
                health: core.health(),
                snapshot: wire::snapshot_to_wire(&core.process.snapshot()),
            })))
        }
        Op::Sync { after } => sync(core, after),
        Op::Start { launch_identity } => start(core, &launch_identity),
        Op::QueueStop { generation: text } => generation(&text).and_then(|id| {
            let receipt = {
                let mut core = lock(core);
                admit(&core)?;
                let receipt = core
                    .process
                    .queue_stop_guarded(id, || Ok::<(), ()>(()))
                    .map_err(runtime)?;
                core.stop_requested(id);
                receipt
            };
            let delivered = deliver(receipt);
            let mut core = lock(core);
            core.stop_delivered(id, delivered.is_ok());
            delivered?;
            core.process.acknowledge_guarded_stop(id).map_err(runtime)?;
            Ok(Reply::Delivered)
        }),
        Op::SleepStop {
            generation: text,
            expect_latest,
        } => generation(&text).and_then(|id| {
            let receipt = {
                let mut core = lock(core);
                admit(&core)?;
                let console = core.console.clone();
                let receipt = core
                    .process
                    .queue_stop_guarded(id, move || {
                        if console.snapshot(0, 1).map(|p| p.latest_cursor).ok()
                            == Some(expect_latest)
                        {
                            Ok::<(), ()>(())
                        } else {
                            Err(())
                        }
                    })
                    .map_err(runtime)?;
                core.stop_requested(id);
                receipt
            };
            let delivered = deliver(receipt);
            let mut core = lock(core);
            core.stop_delivered(id, delivered.is_ok());
            delivered?;
            core.process.acknowledge_guarded_stop(id).map_err(runtime)?;
            Ok(Reply::Delivered)
        }),
        Op::Command {
            generation: text,
            line,
        } => generation(&text).and_then(|id| {
            // A console `stop` is an operator stop, not a crash, once delivered.
            let is_stop = line
                .trim()
                .trim_start_matches('/')
                .eq_ignore_ascii_case("stop");
            let receipt = {
                let mut core = lock(core);
                admit(&core)?;
                if core.save_window.is_some() && is_save_command(&line) {
                    return Err(refused("supervisor_save_window_active"));
                }
                let receipt = core
                    .process
                    .queue_command_guarded(id, &line, || Ok::<(), ()>(()))
                    .map_err(runtime)?;
                if is_stop {
                    core.stop_requested(id);
                }
                receipt
            };
            let delivered = deliver(receipt);
            if is_stop {
                lock(core).stop_delivered(id, delivered.is_ok());
            }
            delivered.map(|()| Reply::Delivered)
        }),
        Op::List {
            generation: text,
            expect_latest,
        } => generation(&text).and_then(|id| {
            let receipt = {
                let mut core = lock(core);
                admit(&core)?;
                let console = core.console.clone();
                // Runs in the writer immediately before the write: refuse if any
                // output exists that the agent has not mirrored yet.
                core.process
                    .queue_list_guarded(id, move || {
                        match console.snapshot(0, 1).map(|page| page.latest_cursor) {
                            Ok(latest) if latest == expect_latest => Ok(()),
                            _ => Err(()),
                        }
                    })
                    .map_err(runtime)?
            };
            deliver(receipt).map(|()| Reply::Delivered)
        }),
        Op::Offline {
            expected_generation,
            configuration,
        } => {
            let mut core = lock(core);
            core.observe_state();
            let actual = core.process.snapshot().generation.map(|g| g.to_hex());
            if core.stopping {
                Err(refused("supervisor_shutting_down"))
            } else if expected_generation != actual {
                Err(refused("offline_stale_generation"))
            } else if configuration.is_some()
                && (actual.is_some()
                    || core.orphan.is_some()
                    || core.unresolved
                    || core.observation_failed
                    || core.intent_for.is_some())
            {
                Err(refused("offline_process_unproven"))
            } else {
                if let (Some(responder), Some(config)) = (&mut core.offline, &configuration)
                    && responder.configuration().address == config.address
                {
                    if let Err(code) = responder.update(config.clone()) {
                        return refused(code);
                    }
                    return Reply::Offline {
                        wakes: responder.take_wakes(),
                    };
                }
                let same =
                    core.offline.as_ref().map(|r| r.configuration()) == configuration.as_ref();
                if !same {
                    if let Some(mut responder) = core.offline.take() {
                        if responder.close().is_err() {
                            core.observation_failed = true;
                            return refused("offline_release_uncertain");
                        }
                    }
                    if let Some(config) = configuration {
                        if let Err(code) = config.validate() {
                            return refused(code);
                        }
                        match vesta_game_runtime::offline::Responder::bind(config) {
                            Ok(responder) => core.offline = Some(responder),
                            Err(code) => return refused(code),
                        }
                    }
                }
                Ok(Reply::Offline {
                    wakes: core
                        .offline
                        .as_ref()
                        .map(|r| r.take_wakes())
                        .unwrap_or_default(),
                })
            }
        }
        Op::Stop {
            generation: text,
            wait_ms,
        } => generation(&text).and_then(|id| {
            {
                let mut core = lock(core);
                admit(&core)?;
                core.process.stop(id, Duration::ZERO).map_err(runtime)?;
                core.admit_stop(id);
            }
            wait_exit(core, id, wait_ms)
        }),
        Op::Kill {
            generation: text,
            wait_ms,
        } => generation(&text).and_then(|id| {
            {
                let mut core = lock(core);
                admit(&core)?;
                // Exact-generation validation happens inside the call; only an applied
                // kill changes restart state. The core lock keeps exit classification
                // from running in between.
                core.process
                    .force_kill_guarded(id, Duration::ZERO, |_| Ok::<(), ()>(()))
                    .map_err(|error| match error {
                        GuardedError::Runtime(error) => runtime(error),
                        GuardedError::Guard(()) => refused("supervisor_worker_failed"),
                    })?;
                core.admit_stop(id);
            }
            wait_exit(core, id, wait_ms)
        }),
        Op::ResolveLaunch => {
            let mut core = lock(core);
            core.observe();
            if !core.unresolved {
                Err(refused("supervisor_nothing_to_resolve"))
            } else if !core.idle() {
                Err(refused("supervisor_game_running"))
            } else {
                record::clear_intent(&core.directory)
                    .map_err(refused)
                    .map(|()| {
                        core.unresolved = false;
                        Reply::Resolved
                    })
            }
        }
        Op::SaveSuspend {
            generation: text,
            lease_ms,
            autosave_baseline,
        } => generation(&text).and_then(|id| save_suspend(core, id, lease_ms, autosave_baseline)),
        Op::SaveRenew {
            generation: text,
            token,
            lease_ms,
        } => generation(&text).and_then(|id| {
            let mut core = lock(core);
            core.observe_state();
            admit(&core)?;
            let lease = core.active_lease(id, &token)?;
            if lease.unresolved {
                return Err(refused("supervisor_save_state_unknown"));
            }
            let renewed = SaveLease {
                expires_at_ms: restart::now_ms().saturating_add(i64::from(lease_ms)),
                ..lease
            };
            save::replace(&core.directory, &renewed).map_err(refused)?;
            core.save_lease = Some(renewed);
            Ok(Reply::SaveRenewed)
        }),
        Op::SaveResume {
            generation: text,
            token,
        } => generation(&text).and_then(|id| save_resume(core, id, &token)),
        Op::Hold { lease_ms } => {
            let mut core = lock(core);
            core.observe_state();
            admit(&core).and_then(|()| {
                if core.process.snapshot().generation.is_some()
                    || core.orphan.is_some()
                    || core.unresolved
                    || core.intent_for.is_some()
                    || core.observation_failed
                {
                    return Err(refused("supervisor_hold_not_stopped"));
                }
                if core.hold.is_some() {
                    return Err(refused("supervisor_hold_active"));
                }
                let token = save::token().map_err(refused)?;
                core.hold = Some((
                    token.clone(),
                    Instant::now() + Duration::from_millis(u64::from(lease_ms)),
                ));
                Ok(Reply::Held { token })
            })
        }
        Op::HoldRenew { token, lease_ms } => {
            let mut core = lock(core);
            core.observe_state();
            if core.hold.as_ref().is_some_and(|(t, _)| t == &token) {
                core.hold = Some((
                    token,
                    Instant::now() + Duration::from_millis(u64::from(lease_ms)),
                ));
                Ok(Reply::HoldRenewed)
            } else {
                Err(refused("supervisor_no_hold"))
            }
        }
        Op::HoldRelease { token } => {
            let mut core = lock(core);
            core.observe_state();
            if core.hold.as_ref().is_some_and(|(t, _)| t == &token) {
                core.hold = None;
                Ok(Reply::HoldReleased)
            } else {
                Err(refused("supervisor_no_hold"))
            }
        }
        Op::Configure { restart } => {
            let policy = restart.map(RestartPolicy::from_wire).transpose();
            match policy {
                Ok(policy) => {
                    lock(core).restarts.set_policy(policy);
                    Ok(Reply::Configured)
                }
                Err(code) => Err(refused(code)),
            }
        }
        Op::ResetRestarts => {
            let mut core = lock(core);
            core.restarts
                .reset()
                .map(|()| Reply::RestartsReset)
                .map_err(refused)
        }
        Op::Shutdown => {
            let mut core = lock(core);
            core.observe();
            if !core.idle() {
                Err(refused("supervisor_game_running"))
            } else {
                // Closed under the same lock: no later Start can be admitted.
                core.stopping = true;
                Ok(Reply::ShuttingDown)
            }
        }
    };
    result.unwrap_or_else(|reply| reply)
}

fn sync(core: &Shared, after: u64) -> std::result::Result<Reply, Reply> {
    let mut core = lock(core);
    core.observe();
    let page = core
        .console
        .snapshot(after, MAX_PAGE_LINES)
        .map_err(|error| refused(error.code()))?;
    let mut lines = Vec::new();
    let mut text = 0usize;
    let mut next_cursor = after;
    for line in &page.lines {
        text += line.text.len();
        if text > wire::MAX_SYNC_TEXT_BYTES && !lines.is_empty() {
            break;
        }
        lines.push(wire::line_to_wire(line));
        next_cursor = line.cursor;
    }
    Ok(Reply::Sync(Box::new(wire::SyncPage {
        snapshot: wire::snapshot_to_wire(&core.process.snapshot()),
        lines,
        next_cursor,
        latest_cursor: page.latest_cursor,
        gap: page.gap,
        health: core.health(),
        generation_floor: core.generation_floor,
    })))
}

fn start(core: &Shared, launch_identity: &str) -> std::result::Result<Reply, Reply> {
    let mut guard = lock(core);
    let core = &mut *guard;
    // Classify any exit first, then let this explicit start supersede whatever
    // restart that classification scheduled.
    core.observe_state();
    admit(core)?;
    if core.hold.is_some() {
        return Err(refused("supervisor_maintenance_hold"));
    }
    core.restarts.cancel_pending();
    if core.health().crash_loop {
        return Err(refused("supervisor_crash_loop"));
    }
    if launch_identity != core.launch_identity {
        return Err(refused("supervisor_launch_mismatch"));
    }
    let (generation, uncertain) = core.launch()?;
    if uncertain {
        return Err(runtime(RuntimeError::StartupUncertain));
    }
    Ok(Reply::Started {
        generation: generation.to_hex(),
    })
}

impl Core {
    /// Launch the trusted definition with durable intent and record. Returns the
    /// generation and whether its I/O attachment was uncertain.
    fn launch(&mut self) -> std::result::Result<(GenerationId, bool), Reply> {
        if self.hold.is_some() {
            return Err(refused("supervisor_maintenance_hold"));
        }
        if self.orphan.is_some() {
            return Err(refused("supervisor_orphan_running"));
        }
        if self.unresolved || self.intent_for.is_some() {
            return Err(refused("supervisor_launch_unresolved"));
        }
        if self.observation_failed {
            return Err(refused(RuntimeError::ObserveFailed.code()));
        }
        if self.recorded.is_some() || self.process.snapshot().generation.is_some() {
            return Err(refused(RuntimeError::AlreadyOwned.code()));
        }
        // Every launch, including autonomous crash restart, relinquishes the
        // responder before a game process can bind. A failed join is sticky.
        if let Some(mut responder) = self.offline.take()
            && responder.close().is_err()
        {
            self.observation_failed = true;
            return Err(refused("offline_release_uncertain"));
        }
        // Lines after this cursor belong to the new generation.
        let floor = self
            .console
            .snapshot(0, 1)
            .map(|page| page.latest_cursor)
            .map_err(|error| refused(error.code()))?;
        // Durable before any spawn: a crash from here on leaves an unresolved intent.
        record::write_intent(
            &self.directory,
            &LaunchIntent {
                version: 1,
                launch_identity: self.launch_identity.clone(),
            },
        )
        .map_err(refused)?;
        let started = self
            .process
            .start_guarded(&self.launch, |_| Ok::<(), ()>(()));
        let uncertain = matches!(
            started,
            Err(GuardedError::Runtime(RuntimeError::StartupUncertain))
        );
        let generation = match started {
            Ok(generation) => generation,
            // A child exists but its I/O did not attach: still record and own it.
            Err(GuardedError::Runtime(RuntimeError::StartupUncertain)) => {
                match self.process.snapshot().generation {
                    Some(generation) => generation,
                    None => {
                        self.unresolved = true;
                        return Err(runtime(RuntimeError::StartupUncertain));
                    }
                }
            }
            Err(error) => {
                // Nothing was spawned: retire the intent (or keep refusing if that fails).
                if record::clear_intent(&self.directory).is_err() {
                    self.unresolved = true;
                }
                return Err(match error {
                    GuardedError::Runtime(error) => runtime(error),
                    GuardedError::Guard(()) => refused("supervisor_worker_failed"),
                });
            }
        };
        self.intent_for = Some(generation);
        self.generation_floor = floor;
        let published = self.process.native_id().and_then(|pid| {
            native::process_start(pid).map(|process_start| GenerationRecord {
                version: 1,
                generation: generation.to_hex(),
                pid,
                process_start,
                boot_id: native::boot_id(),
                launch_identity: self.launch_identity.clone(),
            })
        });
        match published.map(|value| record::write(&self.directory, &value)) {
            Some(Ok(())) => {
                self.recorded = Some(generation);
                if record::clear_intent(&self.directory).is_ok() {
                    self.intent_for = None;
                }
            }
            // Owned and observable, but restart continuity is unproven: the intent stays,
            // refusing further launches here and in any replacement supervisor.
            _ => return Err(refused("supervisor_start_unrecorded")),
        }
        Ok((generation, uncertain))
    }
}

const SAVE_ACK_WAIT: Duration = Duration::from_secs(10);
const FLUSH_WAIT: Duration = Duration::from_secs(120);

/// Deliver one fixed save command to the exact generation; returns the console
/// cursor from before the write, so acknowledgements are read only after it.
fn is_save_command(line: &str) -> bool {
    let command = line
        .trim()
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("");
    ["save-all", "save-off", "save-on"]
        .iter()
        .any(|s| command.eq_ignore_ascii_case(s))
}

fn send_save_command(
    core: &Shared,
    id: GenerationId,
    token: &str,
    line: &str,
    after: Option<u64>,
) -> std::result::Result<u64, Reply> {
    let owner = Arc::downgrade(core);
    let instance = token.to_owned();
    let (receipt, cursor) = {
        let mut core = lock(core);
        core.active_lease(id, token)?;
        let cursor = match after {
            Some(cursor) => cursor,
            None => {
                core.console
                    .snapshot(0, 1)
                    .map_err(|e| refused(e.code()))?
                    .latest_cursor
            }
        };
        let receipt = core
            .process
            .queue_command_guarded(id, line, move || {
                let owner = owner.upgrade().ok_or(())?;
                lock(&owner)
                    .active_lease(id, &instance)
                    .map(|_| ())
                    .map_err(|_| ())
            })
            .map_err(runtime)?;
        (receipt, cursor)
    };
    deliver(receipt)?;
    Ok(cursor)
}

/// Generation/gaps/expiry abort correlation. Return the acknowledgement cursor,
/// not a later snapshot: an unrelated earlier flush must not acknowledge ours.
fn await_ack(
    core: &Shared,
    id: GenerationId,
    token: &str,
    mut after: u64,
    accept: &[SaveAck],
    wait: Duration,
) -> std::result::Result<(SaveAck, u64), Reply> {
    let until = Instant::now() + wait;
    while Instant::now() < until {
        let page = {
            let mut core = lock(core);
            core.observe_state();
            core.active_lease(id, token)?;
            core.console
                .snapshot(after, MAX_PAGE_LINES)
                .map_err(|e| refused(e.code()))?
        };
        if page.gap {
            return Err(refused("supervisor_save_console_gap"));
        }
        for line in &page.lines {
            if let Some(ack) = save::classify(&line.text).filter(|ack| accept.contains(ack)) {
                return Ok((ack, line.cursor));
            }
        }
        after = page.next_cursor;
        if page.lines.is_empty() {
            std::thread::sleep(WAIT_STEP);
        }
    }
    Err(refused("supervisor_save_unacknowledged"))
}

fn save_suspend(
    core: &Shared,
    id: GenerationId,
    lease_ms: u32,
    _baseline: Option<bool>,
) -> std::result::Result<Reply, Reply> {
    let token = {
        let mut core = lock(core);
        core.observe_state();
        admit(&core)?;
        let snapshot = core.process.snapshot();
        if snapshot.generation != Some(id)
            || snapshot.phase != vesta_game_runtime::process::ProcessPhase::Running
        {
            return Err(refused(RuntimeError::StaleGeneration.code()));
        }
        if core.save_lease.is_some() || core.save_window.is_some() {
            return Err(refused("supervisor_save_lease_active"));
        }
        if core.stop_inflight.is_some() {
            return Err(refused("supervisor_stop_in_progress"));
        }
        let token = save::token().map_err(refused)?;
        let lease = SaveLease {
            version: 1,
            token: token.clone(),
            generation: id.to_hex(),
            expires_at_ms: restart::now_ms().saturating_add(i64::from(lease_ms)),
            restore_autosave: false,
            unresolved: true,
            ending: false,
        };
        save::write(&core.directory, &lease).map_err(refused)?;
        core.save_lease = Some(lease);
        core.save_window = Some(token.clone());
        token
    };
    let result = (|| {
        let after = send_save_command(core, id, &token, "save-off", None)?;
        let (ack, off_cursor) = await_ack(
            core,
            id,
            &token,
            after,
            &[SaveAck::Disabled, SaveAck::AlreadyOff],
            SAVE_ACK_WAIT,
        )?;
        let restore = ack == SaveAck::Disabled;
        {
            let mut core = lock(core);
            let mut lease = core.active_lease(id, &token)?;
            lease.restore_autosave = restore;
            lease.unresolved = false;
            // Keep the known obligation in memory even if persistence fails. The
            // older durable unresolved marker remains conservative on recovery.
            core.save_lease = Some(lease.clone());
            save::replace(&core.directory, &lease).map_err(refused)?;
        }
        let after = send_save_command(core, id, &token, "save-all flush", Some(off_cursor))?;
        await_ack(core, id, &token, after, &[SaveAck::Saved], FLUSH_WAIT)?;
        let core = lock(core);
        core.active_lease(id, &token)?;
        Ok(Reply::SaveSuspended {
            token: token.clone(),
            restore_autosave: restore,
        })
    })();
    let mut core = lock(core);
    let result = result.and_then(|reply| core.active_lease(id, &token).map(|_| reply));
    if result.is_err()
        && core
            .save_lease
            .as_ref()
            .is_some_and(|lease| lease.token == token && !lease.unresolved)
    {
        let _ = core.end_lease();
    }
    if core.save_window.as_deref() == Some(&token) {
        core.save_window = None;
    }
    result
}

fn save_resume(core: &Shared, id: GenerationId, token: &str) -> std::result::Result<Reply, Reply> {
    let until = Instant::now() + DELIVERY_WAIT;
    // Validate the instance before observing (expiry observation can finish it).
    {
        let core = lock(core);
        if core.lease_stranded
            || core
                .save_lease
                .as_ref()
                .is_none_or(|l| l.token != token || l.generation != id.to_hex())
        {
            return Err(refused("supervisor_no_save_lease"));
        }
    }
    loop {
        {
            let mut core = lock(core);
            core.observe_state();
            if let Some((done, restored)) = &core.save_completed
                && done == token
            {
                return Ok(Reply::SaveResumed {
                    autosave_restored: *restored,
                });
            }
            if core
                .save_lease
                .as_ref()
                .is_none_or(|l| l.token != token || l.generation != id.to_hex())
            {
                return Err(refused("supervisor_no_save_lease"));
            }
            if let Some(autosave_restored) = core.end_lease()? {
                return Ok(Reply::SaveResumed { autosave_restored });
            }
        }
        if Instant::now() >= until {
            return Err(refused("supervisor_delivery_unknown"));
        }
        std::thread::sleep(WAIT_STEP);
    }
}

fn deliver(mut receipt: GuardedInput<()>) -> std::result::Result<(), Reply> {
    let until = Instant::now() + DELIVERY_WAIT;
    loop {
        match receipt.try_result() {
            Some(Ok(())) => return Ok(()),
            Some(Err(GuardedError::Runtime(error))) => return Err(runtime(error)),
            // The writer's own precondition refused before writing anything.
            Some(Err(GuardedError::Guard(()))) => {
                return Err(refused("supervisor_console_unmirrored"));
            }
            None if Instant::now() >= until => {
                return Err(refused("supervisor_delivery_unknown"));
            }
            None => std::thread::sleep(WAIT_STEP),
        }
    }
}

fn wait_exit(core: &Shared, id: GenerationId, wait_ms: u32) -> std::result::Result<Reply, Reply> {
    let until = Instant::now() + Duration::from_millis(u64::from(wait_ms));
    loop {
        let snapshot = {
            let mut core = lock(core);
            core.observe();
            core.process.snapshot()
        };
        if snapshot.generation != Some(id) {
            return match snapshot.last_exit.filter(|exit| exit.generation == id) {
                Some(exit) => Ok(Reply::Exited {
                    exit: wire::exit_to_wire(&exit),
                }),
                None => Err(refused(RuntimeError::StaleGeneration.code())),
            };
        }
        if Instant::now() >= until {
            return Ok(Reply::TimedOut {
                snapshot: wire::snapshot_to_wire(&snapshot),
            });
        }
        std::thread::sleep(WAIT_STEP);
    }
}

/// Console stream mapping shared with the client mirror.
pub(crate) fn stream(stderr: bool) -> ConsoleStream {
    if stderr {
        ConsoleStream::Stderr
    } else {
        ConsoleStream::Stdout
    }
}

#[cfg(windows)]
mod endpoint {
    use super::MAX_CONNECTIONS;
    use crate::Result;
    use tokio::net::windows::named_pipe::NamedPipeServer;

    /// Connected requests plus one listening instance. The OS enforces this limit.
    const MAX_INSTANCES: usize = MAX_CONNECTIONS + 1;

    /// A finished request disconnects its instance before closing it, which detaches
    /// any client end the caller retained, so retained client handles cannot hold
    /// instances and exhaust the limit.
    #[derive(Clone)]
    pub(super) struct Pool;
    impl Pool {
        pub(super) async fn recycle(&self, instance: NamedPipeServer) {
            let _ = instance.disconnect();
            drop(instance);
        }
    }

    pub(super) struct Listener {
        endpoint: std::path::PathBuf,
        current: Option<NamedPipeServer>,
    }
    impl Listener {
        pub(super) fn bind(endpoint: &std::path::Path) -> Result<Self> {
            let first = crate::pipe_security::create(endpoint, true, MAX_INSTANCES)
                .map_err(|_| "supervisor_already_running")?;
            Ok(Self {
                endpoint: endpoint.to_owned(),
                current: Some(first),
            })
        }
        pub(super) fn pool(&self) -> Pool {
            Pool
        }
        /// Cancel safe: the listening instance stays in `self` until a client is
        /// connected, so a cancelled accept never leaves the endpoint without one.
        pub(super) async fn accept(&mut self) -> Result<NamedPipeServer> {
            if self.current.is_none() {
                self.current = Some(crate::pipe_security::create(
                    &self.endpoint,
                    false,
                    MAX_INSTANCES,
                )?);
            }
            let connected = match &self.current {
                Some(current) => current.connect().await,
                None => return Err("supervisor_accept_failed"),
            };
            let current = self.current.take().ok_or("supervisor_accept_failed")?;
            if connected.is_err() {
                // A broken instance is discarded; the next accept creates another.
                return Err("supervisor_accept_failed");
            }
            // Keep an instance listening before handing out the connected one.
            self.current = crate::pipe_security::create(&self.endpoint, false, MAX_INSTANCES).ok();
            Ok(current)
        }
    }
}

#[cfg(target_os = "linux")]
mod endpoint {
    use crate::{Result, native};
    use tokio::net::{UnixListener, UnixStream};

    #[derive(Clone)]
    pub(super) struct Pool;
    impl Pool {
        pub(super) async fn recycle(&self, stream: UnixStream) {
            drop(stream);
        }
    }

    pub(super) struct Listener {
        inner: UnixListener,
        path: std::path::PathBuf,
        device: u64,
        inode: u64,
    }
    impl Listener {
        /// Called only while holding the exclusive supervisor lock.
        pub(super) fn bind(endpoint: &std::path::Path) -> Result<Self> {
            use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
            let parent = endpoint.parent().ok_or("supervisor_acl_refused")?;
            let metadata =
                std::fs::symlink_metadata(parent).map_err(|_| "supervisor_acl_refused")?;
            if metadata.uid() != native::linux_uid() || metadata.permissions().mode() & 0o077 != 0 {
                return Err("supervisor_acl_refused");
            }
            let inner = match UnixListener::bind(endpoint) {
                Ok(inner) => inner,
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                    // A crashed supervisor's socket: reclaim it only if it is our own
                    // socket inode and nothing answers on it. Anything else refuses.
                    let stale = std::fs::symlink_metadata(endpoint)
                        .map_err(|_| "supervisor_already_running")?;
                    if !stale.file_type().is_socket() || stale.uid() != native::linux_uid() {
                        return Err("supervisor_acl_refused");
                    }
                    match std::os::unix::net::UnixStream::connect(endpoint) {
                        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
                        _ => return Err("supervisor_already_running"),
                    }
                    let again = std::fs::symlink_metadata(endpoint)
                        .map_err(|_| "supervisor_already_running")?;
                    if again.dev() != stale.dev() || again.ino() != stale.ino() {
                        return Err("supervisor_already_running");
                    }
                    std::fs::remove_file(endpoint).map_err(|_| "supervisor_already_running")?;
                    UnixListener::bind(endpoint).map_err(|_| "supervisor_already_running")?
                }
                Err(_) => return Err("supervisor_bind_failed"),
            };
            let metadata =
                std::fs::symlink_metadata(endpoint).map_err(|_| "supervisor_acl_refused")?;
            std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| "supervisor_acl_refused")?;
            Ok(Self {
                inner,
                path: endpoint.to_owned(),
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        pub(super) fn pool(&self) -> Pool {
            Pool
        }
        pub(super) async fn accept(&mut self) -> Result<UnixStream> {
            loop {
                let (stream, _) = self
                    .inner
                    .accept()
                    .await
                    .map_err(|_| "supervisor_accept_failed")?;
                if stream
                    .peer_cred()
                    .is_ok_and(|peer| peer.uid() == native::linux_uid())
                {
                    return Ok(stream);
                }
            }
        }
    }
    impl Drop for Listener {
        fn drop(&mut self) {
            use std::os::unix::fs::{FileTypeExt, MetadataExt};
            // Remove only the socket this supervisor created.
            if std::fs::symlink_metadata(&self.path).is_ok_and(|m| {
                m.file_type().is_socket() && m.dev() == self.device && m.ino() == self.inode
            }) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod endpoint {
    use crate::Result;
    #[derive(Clone)]
    pub(super) struct Pool;
    impl Pool {
        pub(super) async fn recycle(&self, _stream: tokio::io::DuplexStream) {}
    }
    pub(super) struct Listener;
    impl Listener {
        pub(super) fn bind(_endpoint: &std::path::Path) -> Result<Self> {
            Err("supervisor_platform_unsupported")
        }
        pub(super) fn pool(&self) -> Pool {
            Pool
        }
        pub(super) async fn accept(&mut self) -> Result<tokio::io::DuplexStream> {
            Err("supervisor_platform_unsupported")
        }
    }
}

use endpoint::Listener;

#[cfg(test)]
mod lease_tests {
    use super::*;
    use std::{io::BufRead, sync::mpsc};
    use vesta_game_runtime::process::GracefulStop;

    #[test]
    fn inert_lease_game() {
        if std::fs::read("lease-fixture").ok().as_deref() != Some(b"owned-lease-fixture") {
            return;
        }
        for line in std::io::stdin().lock().lines() {
            if line.is_ok_and(|line| line == "stop") {
                return;
            }
        }
    }

    struct Fixture {
        core: Shared,
        _temp: tempfile::TempDir,
        id: GenerationId,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let game = temp.path().join("game");
            std::fs::create_dir(&game).unwrap();
            std::fs::write(game.join("lease-fixture"), b"owned-lease-fixture").unwrap();
            let program = game.join(if cfg!(windows) {
                "owned-supervised-fixture.exe"
            } else {
                "owned-supervised-fixture"
            });
            std::fs::copy(std::env::current_exe().unwrap(), &program).unwrap();
            let launch = LaunchSpec::new(
                program,
                vec![
                    "--exact".into(),
                    "server::lease_tests::inert_lease_game".into(),
                    "--nocapture".into(),
                ],
                game,
                GracefulStop::MinecraftCommand,
            )
            .unwrap();
            let state = StateDirectory::new(temp.path().join("state"), None).unwrap();
            state.prepare().unwrap();
            let directory = state.supervisor().unwrap();
            let console = ConsoleCapture::new();
            let mut process = GameProcess::new(console.clone());
            let id = process
                .start_guarded(&launch, |_| Ok::<(), ()>(()))
                .unwrap();
            let core = Arc::new(Mutex::new(Core {
                process: Preserved(Some(process)),
                console,
                launch,
                launch_identity: "test".into(),
                state: "test".into(),
                directory: directory.clone(),
                recorded: None,
                intent_for: None,
                orphan: None,
                unresolved: false,
                observation_failed: false,
                stopping: false,
                restarts: Restarts::load(directory, None).unwrap(),
                stop_admitted: None,
                stop_inflight: None,
                generation_floor: 0,
                save_lease: None,
                save_delivery: None,
                save_delivered: false,
                save_completed: None,
                save_window: None,
                hold: None,
                lease_stranded: false,
                handled_exit: None,
                offline: None,
            }));
            Self {
                core,
                _temp: temp,
                id,
            }
        }
        fn lease(&self, expires_at_ms: i64) -> String {
            let mut core = lock(&self.core);
            let token = save::token().unwrap();
            let lease = SaveLease {
                version: 1,
                token: token.clone(),
                generation: self.id.to_hex(),
                expires_at_ms,
                restore_autosave: true,
                unresolved: false,
                ending: false,
            };
            save::write(&core.directory, &lease).unwrap();
            core.save_lease = Some(lease);
            token
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Tests own this child: unlike production preservation, drop the
            // GameProcess explicitly so assertion failures never strand fixtures.
            drop(lock(&self.core).process.0.take());
        }
    }

    #[test]
    fn restoration_keeps_marker_until_delivery_and_resume_waits_outside_lock() {
        let fixture = Fixture::new();
        let token = fixture.lease(restart::now_ms() + 60_000);
        let (release, blocked) = mpsc::channel();
        let (entered, waiting) = mpsc::channel();
        let mut barrier = lock(&fixture.core)
            .process
            .queue_command_guarded(fixture.id, "barrier", move || {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                Ok::<(), ()>(())
            })
            .unwrap();
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        {
            let mut core = lock(&fixture.core);
            assert_eq!(core.end_lease().unwrap(), None);
            assert!(core.save_delivery.is_some());
            assert!(save::read(&core.directory).unwrap().unwrap().ending);
            assert_eq!(core.end_lease().unwrap(), None);
            assert!(core.save_lease.is_some());
        }
        let core = fixture.core.clone();
        let id = fixture.id;
        let waiter = std::thread::spawn(move || save_resume(&core, id, &token));
        std::thread::sleep(Duration::from_millis(30));
        assert!(!waiter.is_finished());
        // Health remains available while SaveResume is awaiting the writer.
        assert!(lock(&fixture.core).health().save_lease_ms.is_some());
        release.send(()).unwrap();
        assert_eq!(
            waiter.join().unwrap().unwrap(),
            Reply::SaveResumed {
                autosave_restored: true
            }
        );
        assert!(lock(&fixture.core).save_lease.is_none());
        assert!(
            save::read(&lock(&fixture.core).directory)
                .unwrap()
                .is_none()
        );
        assert!(matches!(barrier.try_result(), Some(Ok(()))));
    }
    #[test]
    fn explicit_supervisor_start_cannot_bypass_a_suspended_crash_loop() {
        let fixture = Fixture::new();
        {
            let mut core = lock(&fixture.core);
            let policy = crate::restart::RestartPolicy::new(
                1,
                Duration::from_secs(600),
                Duration::from_secs(1),
                Duration::from_secs(10),
            )
            .unwrap();
            core.restarts = Restarts::load(core.directory.clone(), Some(policy)).unwrap();
            core.restarts.on_crash(Instant::now(), restart::now_ms());
            assert!(core.health().crash_loop);
        }
        assert!(
            matches!(start(&fixture.core,"test"),Err(Reply::Refused{code}) if code=="supervisor_crash_loop")
        );
        assert_eq!(
            lock(&fixture.core).process.snapshot().generation,
            Some(fixture.id)
        );
        lock(&fixture.core).restarts.reset().unwrap();
        assert!(!lock(&fixture.core).health().crash_loop);
    }

    #[test]
    fn failed_admission_retries_without_clearing_ending_obligation() {
        let fixture = Fixture::new();
        fixture.lease(restart::now_ms() + 60_000);
        let (release, blocked) = mpsc::channel();
        let (entered, waiting) = mpsc::channel();
        let barrier = lock(&fixture.core)
            .process
            .queue_command_guarded(fixture.id, "barrier", move || {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                Ok::<(), ()>(())
            })
            .unwrap();
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut queued = Vec::new();
        loop {
            match lock(&fixture.core)
                .process
                .queue_command_guarded(fixture.id, "fill", || Ok::<(), ()>(()))
            {
                Ok(receipt) => queued.push(receipt),
                Err(RuntimeError::StdinBusy) => break,
                Err(error) => panic!("unexpected queue error: {error:?}"),
            }
        }
        {
            let mut core = lock(&fixture.core);
            assert!(core.end_lease().is_err());
            assert!(core.save_lease.as_ref().unwrap().ending);
            assert!(save::read(&core.directory).unwrap().unwrap().ending);
        }
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            let mut core = lock(&fixture.core);
            core.observe_state();
            if core.save_lease.is_none() {
                break;
            }
            drop(core);
            assert!(Instant::now() < until);
            std::thread::sleep(WAIT_STEP);
        }
        drop((queued, barrier));
    }

    #[test]
    fn console_gap_aborts_even_when_page_contains_a_matching_ack() {
        let fixture = Fixture::new();
        let token = fixture.lease(restart::now_ms() + 60_000);
        let console = lock(&fixture.core).console.clone();
        let after = console.snapshot(0, 1).unwrap().latest_cursor;
        console.mark_gap().unwrap();
        console
            .ingest(
                ConsoleStream::Stdout,
                "[12:00:00] [Server thread/INFO]: Saved the game",
                false,
            )
            .unwrap();
        assert_eq!(
            await_ack(
                &fixture.core,
                fixture.id,
                &token,
                after,
                &[SaveAck::Saved],
                Duration::from_secs(1)
            )
            .unwrap_err(),
            refused("supervisor_save_console_gap")
        );
    }

    #[test]
    fn expired_instance_is_refused_by_the_writer_before_delivery() {
        let fixture = Fixture::new();
        let token = fixture.lease(restart::now_ms() + 60_000);
        let (release, blocked) = mpsc::channel();
        let (entered, waiting) = mpsc::channel();
        let barrier = lock(&fixture.core)
            .process
            .queue_command_guarded(fixture.id, "barrier", move || {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                Ok::<(), ()>(())
            })
            .unwrap();
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        let core = fixture.core.clone();
        let id = fixture.id;
        let command = std::thread::spawn(move || {
            send_save_command(&core, id, &token, "save-all flush", None)
        });
        std::thread::sleep(Duration::from_millis(30));
        lock(&fixture.core)
            .save_lease
            .as_mut()
            .unwrap()
            .expires_at_ms = 0;
        release.send(()).unwrap();
        assert!(command.join().unwrap().is_err());
        drop(barrier);
    }
}
