//! Agent-side view of a supervisor-owned game with the same guarded semantics the
//! operation runner uses for a local `GameProcess`.
//!
//! Each guard runs after an authenticated connection is prepared and immediately
//! before its request is written; the supervisor then performs the effect without
//! further authorization. The window between guard and native effect is therefore one
//! local request write plus supervisor scheduling, not connection setup or retries.
//! A request that was written but whose reply was lost is uncertain, never denied.
//! The agent's console capture mirrors the supervisor ring: lost upstream output is
//! preserved as a gap, and a player-list write happens only when the mirror is current.
use crate::{
    Result,
    client::{self, SupervisorClient},
    server::stream,
    wire::{self, Op, Reply},
};
use std::{
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use vesta_game_runtime::{
    console::ConsoleCapture,
    process::{
        GenerationId, GuardedError, GuardedInput, ProcessPhase, ProcessSnapshot, RuntimeError,
        WaitOutcome,
    },
};

const SYNC_INTERVAL: Duration = Duration::from_millis(50);
const SYNC_WAIT: Duration = Duration::from_secs(2);
const START_WAIT: Duration = Duration::from_secs(10);
const MAX_COMMAND_BYTES: usize = 1024;

/// Supervisor console position already mirrored into the local capture. Ingestion and
/// a player-list horizon both hold this lock, so neither interleaves with the other.
struct Mirror {
    console: ConsoleCapture,
    cursor: u64,
}

pub struct RemoteProcess {
    client: Arc<SupervisorClient>,
    launch_identity: String,
    state: String,
    mirror: Arc<Mutex<Mirror>>,
    console: ConsoleCapture,
    snapshot: ProcessSnapshot,
    synced: Option<Instant>,
    health: wire::Health,
    writer: Option<JoinHandle<()>>,
}
impl std::fmt::Debug for RemoteProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteProcess")
            .field("snapshot", &self.snapshot)
            .field("health", &self.health)
            .finish_non_exhaustive()
    }
}

/// Map a supervisor refusal to the runtime classification it carries, if any.
fn classify(code: &str) -> Option<RuntimeError> {
    use RuntimeError::*;
    [
        InvalidLaunch,
        InvalidCommand,
        InvalidTimeout,
        AlreadyOwned,
        StaleGeneration,
        UnsupportedStop,
        GenerationUnavailable,
        SpawnFailed,
        ThreadFailed,
        StartupUncertain,
        ObserveFailed,
        KillFailed,
        StdinBusy,
        StdinUnavailable,
    ]
    .into_iter()
    .find(|error| error.code() == code)
}

/// Refusals that prove nothing was spawned or written.
fn refused_without_effect(code: &str) -> Option<RuntimeError> {
    match code {
        "supervisor_launch_mismatch"
        | "supervisor_orphan_running"
        | "supervisor_launch_unresolved"
        | "supervisor_shutting_down" => Some(RuntimeError::AlreadyOwned),
        // The list precondition refused before writing: retry later, no effect.
        "supervisor_console_unmirrored" => Some(RuntimeError::StdinBusy),
        _ => classify(code).filter(|error| {
            !matches!(
                error,
                RuntimeError::StartupUncertain | RuntimeError::ThreadFailed
            )
        }),
    }
}

fn valid_command(line: &str) -> bool {
    !line.trim().is_empty()
        && line.len() <= MAX_COMMAND_BYTES
        && !line
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

impl RemoteProcess {
    /// Attach to a running supervisor bound to the same launch definition. Nothing is
    /// launched; an existing generation is adopted for observation and control.
    pub fn attach(client: Arc<SupervisorClient>, launch_identity: String) -> Result<Self> {
        let hello = match client.call(Op::Hello, SYNC_WAIT)? {
            Reply::Hello(hello) => hello,
            _ => return Err("supervisor_invalid_response"),
        };
        if hello.launch_identity != launch_identity {
            return Err("supervisor_launch_mismatch");
        }
        let console = ConsoleCapture::new();
        let mut remote = Self {
            client,
            launch_identity,
            state: hello.state,
            mirror: Arc::new(Mutex::new(Mirror {
                console: console.clone(),
                cursor: 0,
            })),
            console,
            snapshot: wire::snapshot_from_wire(&hello.snapshot)?,
            synced: None,
            health: hello.health,
            writer: None,
        };
        remote.sync(true).map_err(|_| "supervisor_not_running")?;
        Ok(remote)
    }

    /// The canonical state directory and launch identity this supervisor serves.
    pub fn binding(&self) -> (&str, &str) {
        (&self.state, &self.launch_identity)
    }

    /// The local mirror of the supervisor's console ring.
    pub fn console(&self) -> ConsoleCapture {
        self.console.clone()
    }

    /// A game from an earlier supervisor is alive (or unobservable) without stdin.
    pub fn orphan_running(&self) -> bool {
        self.health.orphan_running
    }

    /// A launch intent without a recorded outcome blocks launching until resolved.
    pub fn launch_unresolved(&self) -> bool {
        self.health.launch_unresolved
    }

    pub fn snapshot(&self) -> ProcessSnapshot {
        self.snapshot
    }

    /// Throttled observation. Lost contact or a supervisor-side observation failure is
    /// an observation failure, never "stopped".
    pub fn poll(&mut self) -> std::result::Result<ProcessSnapshot, RuntimeError> {
        self.reap_writer();
        if self.synced.is_some_and(|at| at.elapsed() < SYNC_INTERVAL) {
            return Ok(self.snapshot);
        }
        self.sync(false)
    }

    /// `force` waits for an in-progress player-list horizon; otherwise a busy mirror
    /// simply defers this observation.
    fn sync(&mut self, force: bool) -> std::result::Result<ProcessSnapshot, RuntimeError> {
        let mirror = self.mirror.clone();
        let mut mirror = if force {
            mirror.lock().unwrap_or_else(|poison| poison.into_inner())
        } else {
            match mirror.try_lock() {
                Ok(mirror) => mirror,
                Err(std::sync::TryLockError::WouldBlock) => return Ok(self.snapshot),
                Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
            }
        };
        let page = match self.client.call(
            Op::Sync {
                after: mirror.cursor,
            },
            SYNC_WAIT,
        ) {
            Ok(Reply::Sync(page)) => page,
            _ => return Err(RuntimeError::ObserveFailed),
        };
        let snapshot =
            wire::snapshot_from_wire(&page.snapshot).map_err(|_| RuntimeError::ObserveFailed)?;
        for line in &page.lines {
            if line.cursor <= mirror.cursor {
                return Err(RuntimeError::ObserveFailed);
            }
            if line.cursor > mirror.cursor + 1 {
                // Output evicted from the supervisor ring before it was mirrored.
                mirror
                    .console
                    .mark_gap()
                    .map_err(|_| RuntimeError::ObserveFailed)?;
            }
            mirror
                .console
                .ingest(stream(line.stderr), &line.text, line.truncated)
                .map_err(|_| RuntimeError::ObserveFailed)?;
            mirror.cursor = line.cursor;
        }
        self.snapshot = snapshot;
        self.health = page.health;
        self.synced = Some(Instant::now());
        if page.health.observation_failed {
            return Err(RuntimeError::ObserveFailed);
        }
        Ok(snapshot)
    }

    fn reap_writer(&mut self) {
        if self.writer.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(writer) = self.writer.take()
        {
            let _ = writer.join();
        }
    }

    pub fn start_guarded<E>(
        &mut self,
        guard: impl FnOnce(ProcessSnapshot) -> std::result::Result<(), E>,
    ) -> std::result::Result<GenerationId, GuardedError<E>> {
        let snapshot = self.sync(true)?;
        if snapshot.generation.is_some()
            || self.health.orphan_running
            || self.health.launch_unresolved
        {
            return Err(RuntimeError::AlreadyOwned.into());
        }
        let prepared = self
            .client
            .prepare()
            .map_err(|_| RuntimeError::SpawnFailed)?;
        guard(snapshot).map_err(GuardedError::Guard)?;
        let reply = prepared.send(
            Op::Start {
                launch_identity: self.launch_identity.clone(),
            },
            START_WAIT,
        );
        let _ = self.sync(true);
        match reply {
            Ok(Reply::Started { generation }) => GenerationId::parse_hex(&generation)
                .map_err(|_| RuntimeError::StartupUncertain.into()),
            // Nothing was written: the guard's effect marker records a failed start.
            Err(error) if client::unsent(error) => Err(RuntimeError::SpawnFailed.into()),
            Ok(Reply::Refused { code }) => Err(refused_without_effect(&code)
                .unwrap_or(RuntimeError::StartupUncertain)
                .into()),
            _ => Err(RuntimeError::StartupUncertain.into()),
        }
    }

    /// Graceful stop and bounded wait; never escalates to kill.
    pub fn stop(
        &mut self,
        generation: GenerationId,
        timeout: Duration,
    ) -> std::result::Result<WaitOutcome, RuntimeError> {
        let wait_ms =
            u32::try_from(timeout.as_millis()).map_err(|_| RuntimeError::InvalidTimeout)?;
        if wait_ms > wire::MAX_WAIT_MS {
            return Err(RuntimeError::InvalidTimeout);
        }
        let reply = self.client.call(
            Op::Stop {
                generation: generation.to_hex(),
                wait_ms,
            },
            timeout,
        );
        let _ = self.sync(true);
        match reply {
            Err(error) if client::unsent(error) => Err(RuntimeError::StdinUnavailable),
            reply => outcome(reply),
        }
    }

    pub fn force_kill_guarded<E>(
        &mut self,
        generation: GenerationId,
        timeout: Duration,
        guard: impl FnOnce(ProcessSnapshot) -> std::result::Result<(), E>,
    ) -> std::result::Result<WaitOutcome, GuardedError<E>> {
        let wait_ms =
            u32::try_from(timeout.as_millis()).map_err(|_| RuntimeError::InvalidTimeout)?;
        if wait_ms > wire::MAX_WAIT_MS {
            return Err(RuntimeError::InvalidTimeout.into());
        }
        let snapshot = self.sync(true)?;
        if snapshot.generation != Some(generation) || snapshot.phase == ProcessPhase::Draining {
            return Err(RuntimeError::StaleGeneration.into());
        }
        let prepared = self
            .client
            .prepare()
            .map_err(|_| RuntimeError::KillFailed)?;
        guard(snapshot).map_err(GuardedError::Guard)?;
        let reply = prepared.send(
            Op::Kill {
                generation: generation.to_hex(),
                wait_ms,
            },
            timeout,
        );
        let _ = self.sync(true);
        match reply {
            Err(error) if client::unsent(error) => Err(RuntimeError::KillFailed.into()),
            Ok(Reply::Refused { code }) => Err(refused_without_effect(&code)
                .unwrap_or(RuntimeError::ThreadFailed)
                .into()),
            Ok(reply @ (Reply::Exited { .. } | Reply::TimedOut { .. })) => {
                outcome(Ok(reply)).map_err(GuardedError::Runtime)
            }
            // Written, reply lost: the kill may have happened. Never a no-effect failure.
            _ => Err(RuntimeError::ThreadFailed.into()),
        }
    }

    pub fn queue_stop_guarded<E, F>(
        &mut self,
        generation: GenerationId,
        guard: F,
    ) -> std::result::Result<GuardedInput<E>, RuntimeError>
    where
        E: Send + 'static,
        F: FnMut() -> std::result::Result<(), E> + Send + 'static,
    {
        self.poll()?;
        if self.snapshot.generation != Some(generation)
            || self.snapshot.phase == ProcessPhase::Draining
        {
            return Err(RuntimeError::StaleGeneration);
        }
        self.dispatch(
            Op::QueueStop {
                generation: generation.to_hex(),
            },
            guard,
        )
    }

    pub fn queue_command_guarded<E, F>(
        &mut self,
        generation: GenerationId,
        line: &str,
        guard: F,
    ) -> std::result::Result<GuardedInput<E>, RuntimeError>
    where
        E: Send + 'static,
        F: FnMut() -> std::result::Result<(), E> + Send + 'static,
    {
        if !valid_command(line) {
            return Err(RuntimeError::InvalidCommand);
        }
        self.require_running(generation)?;
        self.dispatch(
            Op::Command {
                generation: generation.to_hex(),
                line: line.to_owned(),
            },
            guard,
        )
    }

    pub fn queue_list_guarded<E, F>(
        &mut self,
        generation: GenerationId,
        guard: F,
    ) -> std::result::Result<GuardedInput<E>, RuntimeError>
    where
        E: Send + 'static,
        F: FnMut() -> std::result::Result<(), E> + Send + 'static,
    {
        self.require_running(generation)?;
        self.dispatch(
            Op::List {
                generation: generation.to_hex(),
                // Replaced on the writer with the mirrored position held by the guard.
                expect_latest: 0,
            },
            guard,
        )
    }

    /// The supervisor recorded graceful admission when it acknowledged the write.
    pub fn acknowledge_guarded_stop(
        &mut self,
        generation: GenerationId,
    ) -> std::result::Result<(), RuntimeError> {
        if self.snapshot.generation == Some(generation)
            && self.snapshot.phase == ProcessPhase::Running
        {
            self.snapshot.phase = ProcessPhase::Stopping;
        }
        Ok(())
    }

    fn require_running(
        &mut self,
        generation: GenerationId,
    ) -> std::result::Result<(), RuntimeError> {
        self.poll()?;
        if self.snapshot.generation != Some(generation)
            || self.snapshot.phase != ProcessPhase::Running
        {
            return Err(RuntimeError::StaleGeneration);
        }
        Ok(())
    }

    /// One owned writer request at a time, like the local single stdin writer. On that
    /// worker: prepare the connection, run the guard, then send. For a player list the
    /// guard runs while the mirror is held, and the supervisor writes only if nothing
    /// was emitted after the mirrored position, so every later line follows the write.
    fn dispatch<E, F>(
        &mut self,
        op: Op,
        mut guard: F,
    ) -> std::result::Result<GuardedInput<E>, RuntimeError>
    where
        E: Send + 'static,
        F: FnMut() -> std::result::Result<(), E> + Send + 'static,
    {
        self.reap_writer();
        if self.writer.is_some() {
            return Err(RuntimeError::StdinBusy);
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let client = self.client.clone();
        let mirror = self.mirror.clone();
        let writer = std::thread::Builder::new()
            .name("vesta-supervisor-writer".into())
            .spawn(move || {
                let result = (|| {
                    let prepared = client
                        .prepare()
                        .map_err(|_| GuardedError::Runtime(RuntimeError::StdinUnavailable))?;
                    let op = match op {
                        Op::List { generation, .. } => {
                            let mirror = mirror.lock().unwrap_or_else(|poison| poison.into_inner());
                            guard().map_err(GuardedError::Guard)?;
                            Op::List {
                                generation,
                                expect_latest: mirror.cursor,
                            }
                        }
                        op => {
                            guard().map_err(GuardedError::Guard)?;
                            op
                        }
                    };
                    match prepared.send(op, client::DELIVERY_WAIT) {
                        Ok(Reply::Delivered) => Ok(()),
                        Ok(Reply::Refused { code }) => Err(GuardedError::Runtime(
                            refused_without_effect(&code).unwrap_or(RuntimeError::ThreadFailed),
                        )),
                        Err(error) if client::unsent(error) => {
                            Err(GuardedError::Runtime(RuntimeError::StdinUnavailable))
                        }
                        // Sent with no reply: delivery is unknown, never denied.
                        _ => Err(GuardedError::Runtime(RuntimeError::ThreadFailed)),
                    }
                })();
                let _ = sender.send(result);
            })
            .map_err(|_| RuntimeError::ThreadFailed)?;
        self.writer = Some(writer);
        Ok(GuardedInput::from_receiver(receiver))
    }
}

fn outcome(reply: Result<Reply>) -> std::result::Result<WaitOutcome, RuntimeError> {
    match reply {
        Ok(Reply::Exited { exit }) => wire::exit_from_wire(&exit)
            .map(WaitOutcome::Exited)
            .map_err(|_| RuntimeError::ObserveFailed),
        Ok(Reply::TimedOut { snapshot }) => wire::snapshot_from_wire(&snapshot)
            .map(WaitOutcome::TimedOut)
            .map_err(|_| RuntimeError::ObserveFailed),
        Ok(Reply::Refused { code }) => Err(classify(&code).unwrap_or(RuntimeError::ObserveFailed)),
        _ => Err(RuntimeError::ObserveFailed),
    }
}

impl Drop for RemoteProcess {
    fn drop(&mut self) {
        // Bounded by the client's request deadlines; never detached.
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}
