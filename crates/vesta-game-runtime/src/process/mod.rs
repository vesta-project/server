//! Trusted host-side direct-child ownership. Not remote authorization or readiness.
//!
//! A timeout retains the child and all I/O workers. Finalization requires observed
//! child exit AND joined workers. Descendants holding pipes can keep it Draining.
//! Drop is synchronous last-resort kill/reap/join and may block indefinitely;
//! explicit stop/kill/wait are the bounded, observable application paths.
mod io;
mod launch;

pub use launch::{LaunchSpec, MAX_ARG_BYTES, MAX_ARGS, MAX_ARGV_BYTES, MAX_PATH_BYTES};
pub(crate) use launch::{MAX_REQUIRED_FILES, validate_args};

use crate::console::ConsoleCapture;
use std::{
    fmt,
    process::{Child, ExitStatus},
    thread,
    time::{Duration, Instant},
};

pub const MAX_COMMAND_BYTES: usize = 8_192;
pub const MAX_WAIT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GracefulStop {
    Unsupported,
    MinecraftCommand,
}

/// Process identity lookup, never a PID or permission. No caller constructor.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct GenerationId([u8; 16]);
impl GenerationId {
    /// Canonical opaque lookup data, never native identity or authorization.
    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(32);
        for byte in self.0 {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 15)]));
        }
        encoded
    }

    /// Only the exact lowercase, nonzero 128-bit representation is accepted.
    /// Successful parsing supplies a lookup key, not a process/effect permit.
    pub fn parse_hex(encoded: &str) -> Result<Self, RuntimeError> {
        if encoded.len() != 32 {
            return Err(RuntimeError::StaleGeneration);
        }
        fn digit(byte: u8) -> Result<u8, RuntimeError> {
            match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                _ => Err(RuntimeError::StaleGeneration),
            }
        }
        let mut bytes = [0; 16];
        for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = (digit(pair[0])? << 4) | digit(pair[1])?;
        }
        if bytes == [0; 16] {
            return Err(RuntimeError::StaleGeneration);
        }
        Ok(Self(bytes))
    }
}
impl fmt::Debug for GenerationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GenerationId([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessPhase {
    Stopped,
    Running,
    Stopping,
    Draining,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExitSummary {
    pub generation: GenerationId,
    pub success: bool,
    pub code: Option<i32>,
    pub output_failed: bool,
    pub stdin_failed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessSnapshot {
    pub generation: Option<GenerationId>,
    pub phase: ProcessPhase,
    pub output_failed: bool,
    pub stdin_failed: bool,
    pub last_exit: Option<ExitSummary>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitOutcome {
    Exited(ExitSummary),
    TimedOut(ProcessSnapshot),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    InvalidLaunch,
    InvalidCommand,
    InvalidTimeout,
    AlreadyOwned,
    StaleGeneration,
    UnsupportedStop,
    GenerationUnavailable,
    SpawnFailed,
    ThreadFailed,
    /// A child exists: observation/recovery is required, not a no-effect failure.
    StartupUncertain,
    ObserveFailed,
    KillFailed,
    StdinBusy,
    StdinUnavailable,
}
impl RuntimeError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLaunch => "game_invalid_launch",
            Self::InvalidCommand => "game_invalid_command",
            Self::InvalidTimeout => "game_invalid_timeout",
            Self::AlreadyOwned => "game_already_owned",
            Self::StaleGeneration => "game_stale_generation",
            Self::UnsupportedStop => "game_unsupported_stop",
            Self::GenerationUnavailable => "game_generation_unavailable",
            Self::SpawnFailed => "game_spawn_failed",
            Self::ThreadFailed => "game_thread_failed",
            Self::StartupUncertain => "game_startup_uncertain",
            Self::ObserveFailed => "game_observe_failed",
            Self::KillFailed => "game_kill_failed",
            Self::StdinBusy => "game_stdin_busy",
            Self::StdinUnavailable => "game_stdin_unavailable",
        }
    }
}
impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Game runtime failed")
    }
}
impl std::error::Error for RuntimeError {}

/// The guard's exact error is preserved; it cannot be confused with a native
/// failure after the durable effect marker. No reusable permit is returned.
#[derive(Debug, Eq, PartialEq)]
pub enum GuardedError<E> {
    Guard(E),
    Runtime(RuntimeError),
}
impl<E> From<RuntimeError> for GuardedError<E> {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(error)
    }
}

/// Acknowledgement of the actual owned stdin request, not queue acceptance.
/// Timeout or dropping the receipt does not cancel/detach its writer or callback.
pub struct GuardedInput<E> {
    receiver: std::sync::mpsc::Receiver<Result<(), GuardedError<E>>>,
    completed: bool,
}
impl<E> fmt::Debug for GuardedInput<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuardedInput")
            .field("completed", &self.completed)
            .finish_non_exhaustive()
    }
}
impl<E> GuardedInput<E> {
    /// A receipt whose result is produced by another owner of the same request,
    /// for example a supervisor client worker. Dropping the sender is uncertainty.
    pub fn from_receiver(receiver: std::sync::mpsc::Receiver<Result<(), GuardedError<E>>>) -> Self {
        Self {
            receiver,
            completed: false,
        }
    }

    /// Return one exact result after guard/write returns. A vanished worker is
    /// uncertainty, never an acknowledgement of a no-effect failure.
    pub fn try_result(&mut self) -> Option<Result<(), GuardedError<E>>> {
        if self.completed {
            return None;
        }
        let result = match self.receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(GuardedError::Runtime(RuntimeError::ThreadFailed))
            }
        };
        self.completed = true;
        Some(result)
    }
}

struct OwnedChild {
    child: Child,
    generation: GenerationId,
    stop: GracefulStop,
    stopping: bool,
    graceful_admitted: bool,
    exit: Option<ExitStatus>,
    io: io::IoThreads,
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.io.close_input();
        // No detached child/worker and no timeout interpreted as termination.
        // Repeated native failure can make last-resort destruction unbounded.
        while self.exit.is_none() {
            let _ = self.child.kill();
            match self.child.wait() {
                Ok(status) => self.exit = Some(status),
                Err(_) => thread::sleep(POLL_INTERVAL),
            }
        }
        // IoThreads::drop closes dispatch channels and joins every owned worker.
    }
}

/// One exclusively owned generation. Console text remains sensitive host data.
///
/// Mutable ownership serializes effects; this is not a management worker/actor.
/// Use zero-duration stop/wait plus polling for a responsive application loop.
/// Start refuses an existing generation instead of providing a restart API.
pub struct GameProcess {
    console: ConsoleCapture,
    owned: Option<OwnedChild>,
    last_exit: Option<ExitSummary>,
}
impl fmt::Debug for GameProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GameProcess")
            .field("owned", &self.owned.is_some())
            .field("last_exit", &self.last_exit)
            .finish_non_exhaustive()
    }
}
impl GameProcess {
    pub fn new(console: ConsoleCapture) -> Self {
        Self {
            console,
            owned: None,
            last_exit: None,
        }
    }

    pub fn start(&mut self, spec: LaunchSpec) -> Result<GenerationId, RuntimeError> {
        self.poll()?;
        if self.owned.is_some() {
            return Err(RuntimeError::AlreadyOwned);
        }
        spec.preflight()?;
        let generation = self.new_generation()?;
        // All fallible thread creation is before the external process effect.
        let io = io::IoThreads::prepare(&self.console)?;
        let child = spec
            .command()
            .spawn()
            .map_err(|_| RuntimeError::SpawnFailed)?;
        self.own_spawned(child, generation, spec.stop(), io)
    }

    /// All fallible preparation precedes the guard. A successful guard is
    /// immediately followed by native spawn; no permit can be retained/reused.
    /// `spec` is literal trusted host configuration, never operation input.
    pub fn start_guarded<E>(
        &mut self,
        spec: &LaunchSpec,
        guard: impl FnOnce(ProcessSnapshot) -> Result<(), E>,
    ) -> Result<GenerationId, GuardedError<E>> {
        self.poll()?;
        if self.owned.is_some() {
            return Err(RuntimeError::AlreadyOwned.into());
        }
        spec.preflight()?;
        let generation = self.new_generation()?;
        let io = io::IoThreads::prepare(&self.console)?;
        let mut command = spec.command();
        guard(self.snapshot()).map_err(GuardedError::Guard)?;
        let child = command.spawn().map_err(|_| RuntimeError::SpawnFailed)?;
        self.own_spawned(child, generation, spec.stop(), io)
            .map_err(GuardedError::Runtime)
    }

    fn own_spawned(
        &mut self,
        child: Child,
        generation: GenerationId,
        stop: GracefulStop,
        io: io::IoThreads,
    ) -> Result<GenerationId, RuntimeError> {
        self.owned = Some(OwnedChild {
            child,
            generation,
            stop,
            stopping: false,
            graceful_admitted: false,
            exit: None,
            io,
        });
        let owned = self.owned.as_mut().ok_or(RuntimeError::StartupUncertain)?;
        if owned.io.attach(&mut owned.child).is_err() {
            owned.stopping = true;
            return Err(RuntimeError::StartupUncertain);
        }
        Ok(generation)
    }

    pub fn poll(&mut self) -> Result<ProcessSnapshot, RuntimeError> {
        self.poll_with(|child| child.try_wait().map_err(|_| RuntimeError::ObserveFailed))
    }

    fn poll_with(
        &mut self,
        observe: impl FnOnce(&mut Child) -> Result<Option<ExitStatus>, RuntimeError>,
    ) -> Result<ProcessSnapshot, RuntimeError> {
        if let Some(owned) = self.owned.as_mut() {
            if owned.exit.is_none() {
                owned.exit = observe(&mut owned.child)?;
                if owned.exit.is_some() {
                    owned.io.close_input();
                }
            }
            owned.io.reap_finished();
            if let Some(status) = owned.exit {
                if owned.io.finished() {
                    self.last_exit = Some(ExitSummary {
                        generation: owned.generation,
                        success: status.success(),
                        code: status.code(),
                        output_failed: owned.io.output_failed(),
                        stdin_failed: owned.io.stdin_failed(),
                    });
                    // Actual exit and every join completed. Drop does not signal again.
                    self.owned.take();
                }
            }
        }
        Ok(self.snapshot())
    }

    /// Queue admission only, not proof of stdin delivery or command execution.
    pub fn queue_line(&mut self, generation: GenerationId, line: &str) -> Result<(), RuntimeError> {
        if !valid_command(line) {
            return Err(RuntimeError::InvalidCommand);
        }
        self.poll()?;
        self.current(generation)?.io.queue(line)
    }

    /// Admit a fixed stop request to the existing bounded writer. The callback
    /// runs in that owned worker immediately before actual stdin write, not now.
    /// Its captures must retain/recheck any host authority through completion.
    pub fn queue_stop_guarded<E, F>(
        &mut self,
        generation: GenerationId,
        guard: F,
    ) -> Result<GuardedInput<E>, RuntimeError>
    where
        E: Send + 'static,
        F: FnMut() -> Result<(), E> + Send + 'static,
    {
        self.poll()?;
        let owned = self.current(generation)?;
        if owned.stop == GracefulStop::Unsupported {
            return Err(RuntimeError::UnsupportedStop);
        }
        if owned.exit.is_some() {
            return Err(RuntimeError::StaleGeneration);
        }
        // Admission is NOT graceful delivery, stopping evidence or an effect.
        owned.io.guarded_stop(guard)
    }

    /// Guarded single-line console delivery; ACK proves write completion only.
    pub fn queue_command_guarded<E, F>(
        &mut self,
        generation: GenerationId,
        line: &str,
        guard: F,
    ) -> Result<GuardedInput<E>, RuntimeError>
    where
        E: Send + 'static,
        F: FnMut() -> Result<(), E> + Send + 'static,
    {
        if !valid_command(line) || line.len() > 1024 {
            return Err(RuntimeError::InvalidCommand);
        }
        self.poll()?;
        let owned = self.current(generation)?;
        if owned.exit.is_some() || owned.stopping {
            return Err(RuntimeError::StaleGeneration);
        }
        owned.io.guarded_command(line, guard)
    }

    /// Admit only the literal player-list command to the existing owned writer.
    /// The guard runs immediately before write; ACK means delivery, not a causal
    /// console response or readiness. This does not mark the process stopping.
    pub fn queue_list_guarded<E, F>(
        &mut self,
        generation: GenerationId,
        guard: F,
    ) -> Result<GuardedInput<E>, RuntimeError>
    where
        E: Send + 'static,
        F: FnMut() -> Result<(), E> + Send + 'static,
    {
        self.poll()?;
        let owned = self.current(generation)?;
        if owned.exit.is_some() || owned.stopping {
            return Err(RuntimeError::StaleGeneration);
        }
        owned.io.guarded_list(guard)
    }

    /// Record actual guarded-write acknowledgement without dispatching anything.
    /// A guard denial leaves graceful admission and phase unchanged.
    pub fn acknowledge_guarded_stop(
        &mut self,
        generation: GenerationId,
    ) -> Result<(), RuntimeError> {
        if self.completed(generation).is_some() {
            return Ok(());
        }
        let owned = self.current(generation)?;
        owned.graceful_admitted = true;
        owned.stopping = true;
        Ok(())
    }

    pub fn stop(
        &mut self,
        generation: GenerationId,
        timeout: Duration,
    ) -> Result<WaitOutcome, RuntimeError> {
        let deadline = deadline(timeout)?;
        self.poll()?;
        if let Some(exit) = self.completed(generation) {
            return Ok(WaitOutcome::Exited(exit));
        }
        let owned = self.current(generation)?;
        if owned.stop == GracefulStop::Unsupported {
            return Err(RuntimeError::UnsupportedStop);
        }
        if owned.exit.is_none() && !owned.graceful_admitted {
            owned.io.queue("stop")?;
            owned.graceful_admitted = true;
            owned.stopping = true;
        }
        self.wait_until(generation, deadline)
    }

    pub fn force_kill(
        &mut self,
        generation: GenerationId,
        timeout: Duration,
    ) -> Result<WaitOutcome, RuntimeError> {
        self.kill_with(generation, timeout, |child| {
            child.kill().map_err(|_| RuntimeError::KillFailed)
        })
    }

    /// Exact-generation preflight precedes authorization; actual native kill
    /// immediately follows it. Timeouts retain the child and every IO handle.
    pub fn force_kill_guarded<E>(
        &mut self,
        generation: GenerationId,
        timeout: Duration,
        guard: impl FnOnce(ProcessSnapshot) -> Result<(), E>,
    ) -> Result<WaitOutcome, GuardedError<E>> {
        let until = deadline(timeout)?;
        self.poll()?;
        if self.completed(generation).is_some() {
            // A guarded effect API cannot report effect success for observation
            // alone. Ordinary force_kill retains its completed-wait semantics.
            return Err(RuntimeError::StaleGeneration.into());
        }
        let snapshot = self.snapshot();
        let owned = self.current(generation)?;
        if owned.exit.is_some() {
            return Err(RuntimeError::StaleGeneration.into());
        }
        guard(snapshot).map_err(GuardedError::Guard)?;
        // Retain ownership even when native kill fails.
        owned.stopping = true;
        owned.child.kill().map_err(|_| RuntimeError::KillFailed)?;
        self.wait_until(generation, until)
            .map_err(GuardedError::Runtime)
    }

    fn kill_with(
        &mut self,
        generation: GenerationId,
        timeout: Duration,
        kill: impl FnOnce(&mut Child) -> Result<(), RuntimeError>,
    ) -> Result<WaitOutcome, RuntimeError> {
        let deadline = deadline(timeout)?;
        self.poll()?;
        if let Some(exit) = self.completed(generation) {
            return Ok(WaitOutcome::Exited(exit));
        }
        let owned = self.current(generation)?;
        if owned.exit.is_none() {
            // Retain stopping ownership even when native kill fails.
            owned.stopping = true;
            kill(&mut owned.child)?;
        }
        self.wait_until(generation, deadline)
    }

    pub fn wait(
        &mut self,
        generation: GenerationId,
        timeout: Duration,
    ) -> Result<WaitOutcome, RuntimeError> {
        self.wait_until(generation, deadline(timeout)?)
    }

    fn wait_until(
        &mut self,
        generation: GenerationId,
        until: Instant,
    ) -> Result<WaitOutcome, RuntimeError> {
        loop {
            let snapshot = self.poll()?;
            if let Some(exit) = self.completed(generation) {
                return Ok(WaitOutcome::Exited(exit));
            }
            self.current(generation)?;
            let now = Instant::now();
            if now >= until {
                return Ok(WaitOutcome::TimedOut(snapshot));
            }
            thread::sleep(POLL_INTERVAL.min(until.saturating_duration_since(now)));
        }
    }

    fn current(&mut self, generation: GenerationId) -> Result<&mut OwnedChild, RuntimeError> {
        self.owned
            .as_mut()
            .filter(|owned| owned.generation == generation)
            .ok_or(RuntimeError::StaleGeneration)
    }

    fn completed(&self, generation: GenerationId) -> Option<ExitSummary> {
        self.last_exit
            .filter(|exit| exit.generation == generation && self.owned.is_none())
    }

    fn new_generation(&self) -> Result<GenerationId, RuntimeError> {
        self.generation_with(|bytes| {
            getrandom::fill(bytes).map_err(|_| RuntimeError::GenerationUnavailable)
        })
    }

    fn generation_with(
        &self,
        mut fill: impl FnMut(&mut [u8; 16]) -> Result<(), RuntimeError>,
    ) -> Result<GenerationId, RuntimeError> {
        for _ in 0..8 {
            let mut bytes = [0; 16];
            fill(&mut bytes)?;
            let id = GenerationId(bytes);
            if bytes != [0; 16] && self.last_exit.is_none_or(|exit| exit.generation != id) {
                return Ok(id);
            }
        }
        Err(RuntimeError::GenerationUnavailable)
    }

    /// Native process ID of the owned, not-yet-exited child. Supervisor records pair
    /// it with the native start time; a PID alone never identifies a generation.
    pub fn native_id(&self) -> Option<u32> {
        self.owned
            .as_ref()
            .filter(|owned| owned.exit.is_none())
            .map(|owned| owned.child.id())
    }

    /// Bounded observation of retained ownership, including observation failures.
    /// Stopped is published only after actual child exit and all workers joined.
    pub fn snapshot(&self) -> ProcessSnapshot {
        match &self.owned {
            Some(owned) => ProcessSnapshot {
                generation: Some(owned.generation),
                phase: if owned.exit.is_some() {
                    ProcessPhase::Draining
                } else if owned.stopping {
                    ProcessPhase::Stopping
                } else {
                    ProcessPhase::Running
                },
                output_failed: owned.io.output_failed(),
                stdin_failed: owned.io.stdin_failed(),
                last_exit: self.last_exit,
            },
            None => ProcessSnapshot {
                generation: None,
                phase: ProcessPhase::Stopped,
                output_failed: self.last_exit.is_some_and(|exit| exit.output_failed),
                stdin_failed: self.last_exit.is_some_and(|exit| exit.stdin_failed),
                last_exit: self.last_exit,
            },
        }
    }
}

fn deadline(timeout: Duration) -> Result<Instant, RuntimeError> {
    if timeout > MAX_WAIT {
        return Err(RuntimeError::InvalidTimeout);
    }
    Instant::now()
        .checked_add(timeout)
        .ok_or(RuntimeError::InvalidTimeout)
}

pub(crate) fn forbidden_char(c: char) -> bool {
    c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')
}
fn valid_command(line: &str) -> bool {
    !line.trim().is_empty() && line.len() <= MAX_COMMAND_BYTES && !line.chars().any(forbidden_char)
}

#[cfg(test)]
mod fault_tests;
#[cfg(test)]
mod guarded_tests;
#[cfg(test)]
mod tests;
