# Per-server process supervisor (P3, first slice)

Status: implemented on Windows and covered by tests there (2026-10-11). The Linux code exists but has not been compiled or run. This page covers the first P3 slice, not the whole P3 gate.

## What it does

`vesta-supervisor` is a separate executable that owns one managed game process. It:

- launches only the launch definition named by the validated host configuration
  (`HostConfiguration` → `supervised_launch`). No executable, path, argument or
  environment ever comes from IPC.
- drains stdout/stderr continuously into the bounded console ring (10,000 lines /
  2 MiB), whether or not an agent is connected.
- delivers stdin lines and the fixed `stop` and `list` commands, acknowledging only
  after the actual write.
- observes exit and joins all I/O workers before reporting a generation stopped.
- has no network listener. Its only control surface is a protected local endpoint.
- never kills, stops or restarts the game on its own. Agent disconnect, agent exit,
  supervisor `Ctrl+C`, cancellation, panic or supervisor failure leave the game
  running. The process owner's destructor preserves a live child instead of running
  the emergency kill.

## Turning it on

Supervision is opt-in per server in `host.toml`:

```toml
[process]
supervision = "separate"   # default: "in-process"
```

Only the exact strings `in-process` and `separate` are accepted. With `separate`,
`vesta-agent serve` finds the running supervisor for that state directory, or
starts `vesta-supervisor` (next to the agent binary) detached from the agent's
console and process group, breaking away from any enclosing Windows job. If the
agent runs inside a job that forbids breakaway and would kill its processes on close,
`serve` refuses (`supervisor_job_lifetime_unproven`) rather than start a supervisor
that could die with the terminal. In that case, start `vesta-supervisor --host <host.toml>
--base <dir>` yourself from a separate terminal or service, then run `serve`. The agent
then binds to the supervisor by launch identity and opens the host. `Host::open` refuses `separate` without an attached supervisor
(`host_supervisor_required`) and refuses a supervisor in `in-process` mode
(`host_supervisor_not_configured`). The attached supervisor must serve this
configuration's exact state directory and launch identity
(`host_supervisor_binding_mismatch`). In `in-process` mode, opening refuses while a
supervisor answers, a recorded supervised game is alive or unobservable, or a launch
is unresolved (`host_supervised_game_running`). Switching modes can never put a second
game beside a surviving one.

## Protected endpoint and protocol

- Windows: named pipe `\\.\pipe\vesta-supervisor-<sha256(account SID, state path)>`,
  with a protected DACL granting only the current account SID. Remote clients are rejected. The client
  checks that the pipe server process runs as the same SID. There are at most six pipe instances
  (four concurrent requests). Each finished request disconnects its instance before
  closing it, so a client that keeps its handle cannot hold an instance.
- Linux: `<state>/supervisor/supervisor.sock`, mode 0600, in the owner-only
  supervisor directory. Peer uid is checked in both directions. While holding the
  exclusive supervisor lock, a stale socket left by a crashed supervisor is reclaimed
  only if it is our own socket inode and nothing answers on it. Anything else refuses,
  and only the socket inode this supervisor created is removed on exit.
- Same-account processes remain trusted. This is not OS-identity isolation.

Frames are a 4-byte big-endian length followed by JSON. Requests are capped at 16 KiB and
responses at 4 MiB, and console text per sync is capped at 512 KiB. Version 1 operations are
closed: `hello`, `sync{after}`, `start{launch_identity}`, `queue_stop`, `stop{wait_ms}`
(graceful only), `kill{wait_ms}` (explicit), `command{line ≤ 1024}`,
`list{expect_latest}`, `resolve_launch` and `shutdown`. An accepted `shutdown` closes
admission under the same lock, so no later effect can be admitted. Unknown fields, operations and versions are refused. Replies carry fixed
codes only. Each request uses a fresh connection. A failure after the request was
written is reported as `supervisor_outcome_unknown` and must not be retried blindly.

## Launch binding and duplicate-launch prevention

- Launch identity is a SHA-256 over the exact launch-definition bytes and the paths
  that give them meaning (base, launch document, game, state, working directory). The agent attaches
  only to a supervisor reporting the same identity, and `start` with a different
  identity is refused (`supervisor_launch_mismatch`).
- One supervisor per state directory: an exclusive lock on
  `<state>/supervisor/supervisor.lock`, plus a first-instance pipe or socket bind.
- Before any spawn, a launch intent (`launch-intent.json`) is published. After the
  spawn, the generation record (`generation.json`) is published and the intent retired:
  generation, PID, native process start time (Windows creation FILETIME; Linux
  `/proc/<pid>/stat` start time plus kernel boot ID) and launch identity. Both files are
  written to a staging name, synced and hard-linked into place, so a published record
  is always complete. A spawn whose I/O failed to attach is still recorded. The
  record is removed only after the exact process is observed exited.
- An intent with no record means the supervisor crashed between intent and record.
  A child may exist that nothing can identify, so launching stays refused
  (`supervisor_launch_unresolved`). After checking no such game is running, the operator
  runs `vesta-agent resolve-launch <name> --confirm <name>`. That clears only the intent,
  only while nothing is live, and never starts, stops or kills anything.
- Liveness has three outcomes: running (PID and start time match), gone (no
  such process, different start time, a different boot, or a signalled process handle — never
  exit code 259) and unknown (the query failed). Unknown is treated as still running.
- On supervisor start, a record whose PID and start time still match a live process
  means a game outlived its supervisor (an orphan). `start` is refused,
  `shutdown` is refused, and `hello`/`sync` report `orphan_running`. The orphan is never
  killed. Once it exits on its own, the record is cleared and launching is allowed
  again. A PID alone is never treated as identity, which guards against PID reuse.

## Agent behaviour

The operation runner drives either its own `GameProcess` or a `RemoteProcess`
(`ProcessBackend`) through the same guarded, generation-bound calls:

- Start/kill guards run immediately before the request is sent. Queued stop,
  command and list guards run on a single writer worker immediately before sending,
  mirroring the local single stdin writer. The interval between guard and write now
  includes one local IPC exchange.
- Each guard runs after the connection is prepared and authenticated, immediately
  before the request is written. If nothing was sent (supervisor not running or busy), the
  result is a failure with no effect. If the request was written and the reply was lost, the result is
  uncertain (`StartupUncertain`, `ThreadFailed`), never success or denial. This includes
  a kill whose game is gone by the time the agent looks.
- Console lines carry the supervisor's cursor. A jump means output was evicted
  before it was mirrored, and the agent's console reports a gap rather than a false
  continuous stream. A player-list command is written only if the supervisor has
  no output past the position the agent mirrored when its guard ran
  (`supervisor_console_unmirrored` otherwise), so every line after the horizon follows the write.
- A native observation failure inside the supervisor is sticky and reported to the
  agent (`observation_failed`). The agent treats it as lost observation proof.
- Observation polls the supervisor at most every 50 ms and mirrors its console ring
  into the agent's local capture by cursor. Lost contact is an observation failure.
  The runner's existing sticky rule then stops dispatch until the agent is restarted.
- Runtime shutdown waits only for an agent-owned child. A supervised game keeps
  running. An operation still waiting for the supervised game to exit is finished as
  uncertain (`game_supervised_outcome_unobserved`) instead of blocking shutdown. After shutdown, the agent asks the supervisor to exit, which it does only if
  no game or orphan is live (`supervisor_released` / `supervisor_retained`).
- `serve` in supervised mode writes a supervised session marker. After an agent crash,
  the next supervised `serve` archives that marker and reattaches. An in-process marker
  (a crashed agent that owned its game) still refuses, in either mode.
- If startup fails after attaching, the agent still asks the supervisor to exit if idle.
- `vesta-agent delete` refuses (`local_supervised_game_running`) while a supervisor
  answers, a recorded game is alive or unobservable, or a launch is unresolved. It checks
  both the default and the configured state directory, and refuses if the host
  configuration cannot be read.

## Evidence

All of the following ran on Windows:

- `crates/vesta-supervisor/tests/supervisor.rs`: adoption after agent disconnect
  without relaunch, and output drained while no agent was attached. Also: command delivery
  and echo mirroring, shutdown refused while running, graceful stop clears the record,
  guard refusal sends nothing, explicit kill, mismatched identity, second supervisor
  refused, and a recorded orphan blocks start and shutdown until it exits.
- `apps/vesta-agent/tests/supervisor_e2e.rs` uses the real `vesta-supervisor`
  executable and a real `Host`. The game survives agent shutdown, and a new agent adopts
  the same generation and refuses a second start. After a supervisor crash (killed),
  the game survives with its record, the replacement supervisor reports the orphan and
  refuses shutdown, and the block clears only after the game exits on its own.
- Unit tests cover the closed protocol, framing bounds, start-time identity and the
  create-new record.

## Review

An independent review (Sol, 2026-10-11) of the first version found 30 issues; the 22 in
this slice were all fixed with regressions where testable: spawn-to-record gap,
unknown liveness, cancellation kill, mode-switch bypass, job lifetime, shutdown blocking,
crash reattach, guard timing, host binding, identity context, record publication, hidden
observation failures, lost kill replies, shutdown/start race, stale Linux socket, pipe
exhaustion, console gaps, player-list horizon, delete state path, startup leak, Linux boot
identity and exit code 259. The supervised-shutdown and lost-kill-reply runner paths are
covered by source review, not a dedicated test. The other 8 findings concerned the earlier
permissions, gzip and download work and are tracked in the management API contract.

## Not in this slice (remaining P3 work)

- Separate OS identities for game and control plane, and service-account launch helpers.
- Crash-restart policy with bounded backoff and crash-loop suspension, and start-on-agent-start.
- Readiness detection, and log catch-up by file identity/offset after supervisor loss.
- Autosave/save-lease primitive and its crash recovery.
- OS service installation and shutdown-deadline integration, plus supervisor self-update
  (activation only while stopped).
- Degraded control of an orphan through connector/RCON adapters. Today an orphan is
  observable only through its own logs, and stdin control needs an approved restart.
- Process-tree containment of game descendants.
- Linux execution evidence. The Linux branches were written for the same contract
  but were not compiled or run here. Unix socket path length limits apply.
- Repeated agent/supervisor/host failure drills (the P3 gate).
