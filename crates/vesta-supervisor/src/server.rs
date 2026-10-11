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
}
impl Core {
    fn observe(&mut self) {
        if self.process.poll().is_err() {
            // Sticky: later successful polls cannot prove what happened meanwhile.
            self.observation_failed = true;
        }
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
    fn health(&self) -> Health {
        Health {
            orphan_running: self.orphan.is_some(),
            launch_unresolved: self.unresolved || self.intent_for.is_some(),
            observation_failed: self.observation_failed,
        }
    }
    fn idle(&self) -> bool {
        self.process.snapshot().generation.is_none()
            && self.orphan.is_none()
            && self.intent_for.is_none()
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
            tokio::time::sleep(Duration::from_millis(250)).await;
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
                core.process
                    .queue_stop_guarded(id, || Ok::<(), ()>(()))
                    .map_err(runtime)?
            };
            deliver(receipt)?;
            lock(core)
                .process
                .acknowledge_guarded_stop(id)
                .map_err(runtime)?;
            Ok(Reply::Delivered)
        }),
        Op::Command {
            generation: text,
            line,
        } => generation(&text).and_then(|id| {
            let receipt = {
                let mut core = lock(core);
                admit(&core)?;
                core.process
                    .queue_command_guarded(id, &line, || Ok::<(), ()>(()))
                    .map_err(runtime)?
            };
            deliver(receipt).map(|()| Reply::Delivered)
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
        Op::Stop {
            generation: text,
            wait_ms,
        } => generation(&text).and_then(|id| {
            {
                let mut core = lock(core);
                admit(&core)?;
                core.process.stop(id, Duration::ZERO).map_err(runtime)?;
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
                core.process
                    .force_kill_guarded(id, Duration::ZERO, |_| Ok::<(), ()>(()))
                    .map_err(|error| match error {
                        GuardedError::Runtime(error) => runtime(error),
                        GuardedError::Guard(()) => refused("supervisor_worker_failed"),
                    })?;
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
    })))
}

fn start(core: &Shared, launch_identity: &str) -> std::result::Result<Reply, Reply> {
    let mut guard = lock(core);
    let core = &mut *guard;
    core.observe();
    admit(core)?;
    if launch_identity != core.launch_identity {
        return Err(refused("supervisor_launch_mismatch"));
    }
    if core.orphan.is_some() {
        return Err(refused("supervisor_orphan_running"));
    }
    if core.unresolved || core.intent_for.is_some() {
        return Err(refused("supervisor_launch_unresolved"));
    }
    if core.observation_failed {
        return Err(refused(RuntimeError::ObserveFailed.code()));
    }
    if core.recorded.is_some() || core.process.snapshot().generation.is_some() {
        return Err(refused(RuntimeError::AlreadyOwned.code()));
    }
    // Durable before any spawn: a crash from here on leaves an unresolved intent.
    record::write_intent(
        &core.directory,
        &LaunchIntent {
            version: 1,
            launch_identity: core.launch_identity.clone(),
        },
    )
    .map_err(refused)?;
    let started = core
        .process
        .start_guarded(&core.launch, |_| Ok::<(), ()>(()));
    let uncertain = matches!(
        started,
        Err(GuardedError::Runtime(RuntimeError::StartupUncertain))
    );
    let generation = match started {
        Ok(generation) => generation,
        // A child exists but its I/O did not attach: still record and own it.
        Err(GuardedError::Runtime(RuntimeError::StartupUncertain)) => {
            match core.process.snapshot().generation {
                Some(generation) => generation,
                None => {
                    core.unresolved = true;
                    return Err(runtime(RuntimeError::StartupUncertain));
                }
            }
        }
        Err(error) => {
            // Nothing was spawned: retire the intent (or keep refusing if that fails).
            if record::clear_intent(&core.directory).is_err() {
                core.unresolved = true;
            }
            return Err(match error {
                GuardedError::Runtime(error) => runtime(error),
                GuardedError::Guard(()) => refused("supervisor_worker_failed"),
            });
        }
    };
    core.intent_for = Some(generation);
    let published = core.process.native_id().and_then(|pid| {
        native::process_start(pid).map(|process_start| GenerationRecord {
            version: 1,
            generation: generation.to_hex(),
            pid,
            process_start,
            boot_id: native::boot_id(),
            launch_identity: core.launch_identity.clone(),
        })
    });
    match published.map(|value| record::write(&core.directory, &value)) {
        Some(Ok(())) => {
            core.recorded = Some(generation);
            if record::clear_intent(&core.directory).is_ok() {
                core.intent_for = None;
            }
        }
        // Owned and observable, but restart continuity is unproven: the intent stays,
        // refusing further launches here and in any replacement supervisor.
        _ => return Err(refused("supervisor_start_unrecorded")),
    }
    if uncertain {
        return Err(runtime(RuntimeError::StartupUncertain));
    }
    Ok(Reply::Started {
        generation: generation.to_hex(),
    })
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
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::net::windows::named_pipe::NamedPipeServer;

    /// Connected requests plus one listening instance.
    const MAX_INSTANCES: usize = MAX_CONNECTIONS + 1;

    /// Bounded pipe instances. A finished request disconnects its instance before
    /// closing it, which detaches any client end the caller retained, so retained
    /// client handles cannot hold instances and exhaust the limit.
    #[derive(Clone)]
    pub(super) struct Pool {
        endpoint: Arc<std::path::PathBuf>,
        created: Arc<AtomicUsize>,
    }
    impl Pool {
        fn take(&self) -> Result<NamedPipeServer> {
            if self.created.fetch_add(1, Ordering::AcqRel) >= MAX_INSTANCES {
                self.created.fetch_sub(1, Ordering::AcqRel);
                return Err("supervisor_endpoint_busy");
            }
            crate::pipe_security::create(&self.endpoint, false, MAX_INSTANCES).inspect_err(|_| {
                self.created.fetch_sub(1, Ordering::AcqRel);
            })
        }
        fn discard(&self) {
            self.created.fetch_sub(1, Ordering::AcqRel);
        }
        pub(super) async fn recycle(&self, instance: NamedPipeServer) {
            let _ = instance.disconnect();
            drop(instance);
            self.discard();
        }
    }

    pub(super) struct Listener {
        pool: Pool,
        current: Option<NamedPipeServer>,
    }
    impl Listener {
        pub(super) fn bind(endpoint: &std::path::Path) -> Result<Self> {
            let first = crate::pipe_security::create(endpoint, true, MAX_INSTANCES)
                .map_err(|_| "supervisor_already_running")?;
            Ok(Self {
                pool: Pool {
                    endpoint: Arc::new(endpoint.to_owned()),
                    created: Arc::new(AtomicUsize::new(1)),
                },
                current: Some(first),
            })
        }
        pub(super) fn pool(&self) -> Pool {
            self.pool.clone()
        }
        pub(super) async fn accept(&mut self) -> Result<NamedPipeServer> {
            let current = match self.current.take() {
                Some(current) => current,
                None => self.pool.take()?,
            };
            if current.connect().await.is_err() {
                // A broken instance is discarded; the next accept takes another.
                drop(current);
                self.pool.discard();
                return Err("supervisor_accept_failed");
            }
            // Keep an instance listening before handing out the connected one.
            self.current = self.pool.take().ok();
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
