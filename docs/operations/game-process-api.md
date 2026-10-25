# Rust owned game-process increment — frozen test interface

Date: 2026-10-08. Status: **implemented ordinary-library contract; not itself executed evidence**.
See [the process checkpoint](../archive/RUST_GAME_PROCESS_CHECKPOINT.md) for actual Windows commands/results,
review fixes, failed-attempt history and remaining gaps.

This continues the approved migration using an owned direct-child process and bounded console
component, not Java classes or a new supervisor/remote API. Inert Rust fixture processes are
permitted tests; real Minecraft/provider/installer execution is not authorized. The caller is
trusted host application code, not authenticated remote authority. Activation/installation must
never call stop/restart automatically. This component alone is not independent supervisor/IPC/crash
continuity, process-tree confinement, readiness, durable operation/effect composition, or signed
distribution. The subsequently approved closed host operation composition is documented in
[game-operations-api.md](game-operations-api.md), with executed scope recorded separately
in [../archive/RUST_INTEGRATED_MIGRATION_PASS.md](../archive/RUST_INTEGRATED_MIGRATION_PASS.md).

## Public API (`vesta_game_runtime::process`)

```rust
pub const MAX_ARGS: usize = 256;
pub const MAX_ARG_BYTES: usize = 8_192;
pub const MAX_ARGV_BYTES: usize = 32_768;
pub const MAX_PATH_BYTES: usize = 4_096;
pub const MAX_COMMAND_BYTES: usize = 8_192;
pub const MAX_WAIT: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GracefulStop { Unsupported, MinecraftCommand }

pub struct LaunchSpec { /* private, redacted Debug */ }
impl LaunchSpec {
    pub fn new(program: PathBuf, args: Vec<String>, cwd: PathBuf,
               stop: GracefulStop) -> Result<Self, RuntimeError>;
}

// Opaque generation lookup, not permission, PID or native attachment.
// The 2026-10-09 operation integration adds strict parse_hex/to_hex lookup encoding.
// Manual Debug renders GenerationId([redacted]), not the private bytes.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct GenerationId(/* private */);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessPhase { Stopped, Running, Stopping, Draining }
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
pub enum WaitOutcome { Exited(ExitSummary), TimedOut(ProcessSnapshot) }

// Manual Debug shows owned-presence and historical last_exit, not paths/argv/command text.
pub struct GameProcess { /* one owned generation; no Clone */ }
impl GameProcess {
    pub fn new(console: ConsoleCapture) -> Self;
    pub fn start(&mut self, spec: LaunchSpec) -> Result<GenerationId, RuntimeError>;
    pub fn poll(&mut self) -> Result<ProcessSnapshot, RuntimeError>;
    // Admission only, not proof stdin write/command effect occurred.
    pub fn queue_line(&mut self, generation: GenerationId, line: &str)
        -> Result<(), RuntimeError>;
    pub fn stop(&mut self, generation: GenerationId, timeout: Duration)
        -> Result<WaitOutcome, RuntimeError>;
    pub fn force_kill(&mut self, generation: GenerationId, timeout: Duration)
        -> Result<WaitOutcome, RuntimeError>;
    pub fn wait(&mut self, generation: GenerationId, timeout: Duration)
        -> Result<WaitOutcome, RuntimeError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    InvalidLaunch, InvalidCommand, InvalidTimeout, AlreadyOwned,
    StaleGeneration, UnsupportedStop, GenerationUnavailable,
    SpawnFailed, ThreadFailed, StartupUncertain, ObserveFailed, KillFailed,
    StdinBusy, StdinUnavailable,
}
impl RuntimeError { pub const fn code(self) -> &'static str; }
// Fixed Display: "Game runtime failed". No native source chain; Debug contains only classification.
```

## Required semantics

- Absolute, bounded UTF-8 program/cwd paths; existing program file/cwd directory checked at
  construction and again at start. Controls and U+2028/U+2029 in paths/args refuse; empty literal
  args are valid. No shell, interpolation, JVM argfile parsing or library policy. The environment
  is inherited without an override interface. `.bat`/`.cmd` launch is refused on all platforms
  before spawn. Validation is not native confinement or executable provenance. Native spawn
  may still refuse an otherwise valid spec.
- Arguments are passed individually. Bounds apply per-argument, to the aggregate UTF-8 bytes
  and to a conservative always-quoted Windows command budget (32,767 UTF-16 units including
  quoting/backslash expansion, separators and NUL). The conservative budget applies on every
  platform; it is not native execution evidence for other platforms.
- Each real spawn receives an opaque random 128-bit generation, never a PID/local counter.
  RNG refusal precedes effects. A live/stopping/pipe-draining generation prevents replacement;
  actual child exit alone is not pipe-thread exit. Failed stop/kill/observe does not release
  ownership or permit restart. Thread setup precedes spawn; a post-spawn attach failure is
  `StartupUncertain`, retains the child/generation/I/O in Stopping and requires observation/recovery.
  `AlreadyOwned` is an inert refusal, not Java's already-running convenience no-op.
- Stdout/stderr are piped and drained immediately by owned threads, even with no snapshots/viewer.
  Capture faults are visible and do not abandon a still-readable pipe: drain/discard after a
  capture-storage fault. Native read failures remain faults, never a fabricated success.
- Stdin uses one owned writer thread and a nonblocking queue of eight pending lines, plus at
  most one writer-held line. Each line is at most 8,192 UTF-8 bytes before the appended newline.
  Blank/whitespace-only lines, controls (including NUL/CR/LF), U+2028/U+2029 and excessive bytes
  refuse; no newline injection. Unsupported graceful stop refuses before enqueue or
  phase changes. Minecraft stop sends only fixed `stop`; arbitrary caller-selected stop scripts
  are not admitted. Queue-full/disconnected errors do not imply delivery or child exit.
- Commands/stop/kill/wait are generation-bound. A stale generation cannot affect a replacement.
  Poll/timeout observe actual OS child exit; no PID/open-port/output marker is readiness.
- `stop` timeout retains Stopping/child/writer/readers, forbids restart and never escalates itself.
  `force_kill` targets only the retained direct-child handle, never a caller PID; timeout still
  retains ownership. Zero timeout is valid and still observes actual state; it can return Exited
  or TimedOut, or a genuine generation/observation error. Timeouts greater than MAX_WAIT refuse
  before effects.
  The most recently completed generation remains wait-observable while no replacement is owned;
  last_exit stays historical during a later generation, whose stale effects/waits refuse.
- Finalization only after actual exit and all three I/O threads have returned/joined. Descendants
  holding inherited pipe handles can keep Draining; bounded waits must retain ownership, not
  detach. Process-tree termination/IPC is unfinished, not silently certified by direct-child kill.
- Drop is last-resort synchronous direct-child kill/wait plus owned I/O joins, not a bounded
  shutdown API. It can block on uncooperative/inherited handles. It cannot claim crash continuity.
  Normal application paths must use explicit generation-bound wait/stop/kill and inspect outcomes.

Test using only a clearly named inert fixture binary under tests/fixtures; it is not an agent or
supervisor executable. Current execution target is Windows; design for intended platforms but
record other OS/MSRV/architecture as unexecuted. Parent owns implementation, manifests/builds/
fixture interface and verification. Bunny may own a separate public integration test batch after
fixture modes are frozen. Sol independent review follows source handoff, not assumed pass.


## Process-family cleanup (2026-10-25)

Each launch now owns a private Unix process group or a Windows kill-on-close Job
Object. On Windows the child is spawned suspended, assigned to its job, then
resumed; assignment/resume failure retains uncertain startup ownership for cleanup.
No uncontained Windows child is allowed to execute application code first.

Force-kill (including its guarded path) and final owner cleanup terminate that
family, including when the direct child has exited but inherited output pipes keep
the generation Draining. Worker joins and actual exit observation still precede
Stopped and the next generation. Unix retains the exited group leader until final
cleanup so its group ID cannot be reused while Draining. Ordinary graceful exit
continues draining output.
This is process lifetime management, not a separate-user/plugin security sandbox;
Unix descendants that deliberately leave the process group are outside this boundary.

Native contracts: [Windows job objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
and [Rust Unix process groups](https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html#method.process_group).
