# Build and test from a temporary development shell

From this repository, run:

On Windows, use `./scripts/dev-shell.ps1` from a standalone PowerShell terminal.
It builds the helper with Cargo, then starts it after Cargo exits. This avoids
Cargo versions whose kill-on-close Windows job prevents independent agent
restarts. Exit any existing `cargo dev` shell before using this entry point.
Arguments such as `--release` and `--server-root` are forwarded unchanged.
Linux can continue using `cargo dev` below.

```powershell
cargo dev
# Equivalent long form:
cargo run -p vesta-dev --
# Use optimized binaries for performance measurements:
cargo dev --release
```

This builds `vesta`, `vesta-agent`, `vesta-supervisor`, and `vesta-setup`, copies
them into a fresh temporary build directory, and opens a child PowerShell session
on Windows or Bash session on Linux. Its PATH and `VESTA_SERVER_ROOT` are local
to that session. `exit` returns to the original terminal with its environment
unchanged. A child program cannot add PATH entries to its parent terminal; the
child shell is what makes a single Cargo command sufficient. No profile, global
PATH, service, firewall, server configuration, or data is modified.
The first compilation can take several minutes; subsequent builds use Cargo's
incremental/cache behavior. The session uses a clean shell without personal
profile scripts, keeping command resolution predictable.

Inside the session:

```powershell
vesta --help
vesta list
vesta manage dev-server status
vesta manage dev-server
vesta-setup preview
vesta-rebuild
# Build first, then gracefully restart just this server's agent/supervisor:
vesta-rebuild --restart dev-server
exit
```

`vesta-rebuild` repeats the build with the same debug/release profile, stages a
complete fresh set of executables, and changes this shell's PATH to that set.
A failed compile or incomplete copy leaves the previous selection in place.
Cargo supplies the executable paths, so custom `CARGO_TARGET_DIR` and Cargo target
directory configuration are supported. This workflow builds for the native host;
do not set Cargo's build target to an incompatible platform for an interactive run.

Without `--restart`, running agents and supervisors continue using their original build. Rebuilding
does not restart anything, interrupt Minecraft, or clear recovery markers. Copies
are deliberately staged outside Cargo's output directory so a Windows process
does not lock the next build's destination. All four binaries stay together so an
agent can locate the matching supervisor beside itself.

### Rebuild and restart a selected development server

`vesta-rebuild --restart <server_id|name>` builds and stages the complete tool set
before requesting any interruption. It uses the selected session registry; it
does not discover or restart unrelated processes. A failed build leaves running
processes and the previous command selection alone.

After a successful build it reads live local status, requests the existing
graceful agent shutdown (which stops Minecraft normally), then waits for the
managed agent lease, runtime lock and separate supervisor lock to be released.
It preserves recovery markers and refuses unproven shutdown. No process is ever
force-killed. It checks that the host and launch documents did not change, then
starts the staged agent with its matching staged supervisor and the same data.
Minecraft is started only if it was running in the pre-restart observation;
otherwise it stays stopped. Avoid simultaneous lifecycle/configuration changes
while the restart is underway. A stopping/draining game is refused. An offline
agent requires an explicit listener choice and proven quiescent ownership; only
the agent is brought online and Minecraft stays stopped. The command verifies the replacement agent's
PID over the protected local endpoint before restoring the game. Reporting
"started" confirms a native generation, not Minecraft readiness.

Current agents report their listener mode so it can be preserved. When updating
an older agent that lacks this metadata, the command refuses before shutdown and
asks for a one-time explicit selection:

```powershell
# Normal configured TLS listener (use this for the existing launcher demo):
vesta-rebuild --restart dev-server --configured-listener
# An agent deliberately launched with serve --local-only:
vesta-rebuild --restart dev-server --local-only
```

For current agents, a contradictory listener override is refused. Restart logs
are stored in a private directory under the session's `restarts` folder; the
command prints the exact path. A failed or uncertain shutdown/start is never
automatically retried or force-killed. If activation fails after shutdown, the
server can remain stopped or the replacement agent can remain running: inspect
the printed log/status before taking another action. The newly built commands
remain selected in the dev shell so recovery uses the new build.

Agents built before the local shutdown command was added cannot accept it.
Stop that older agent once using Ctrl+C in its terminal, then repeat the rebuild
with an explicit listener choice. Do not force-kill it or remove recovery markers.
The helper distinguishes an unsupported local request from a runtime refusal or
uncertain delivery, and checks independent process launch before stopping a host.

This is development tooling, not installation or signed release activation.
Already-open dev shells need to be replaced with a fresh `cargo dev` session
once to load the updated tool and shell function.

## Select server data

The registry defaults to `VESTA_SERVER_ROOT` if already set, otherwise the existing
test-server registry `%TEMP%\vesta-dev-server\servers` on Windows or
`${TMPDIR:-/tmp}/vesta-dev-server/servers` on Linux. Set it explicitly when needed:

```powershell
cargo dev --server-root 'D:\Vesta test\servers'
```

The session does not create a server, accept the EULA, start an agent, or select
an arbitrary server automatically. To create the existing disposable Minecraft
test fixture, use [the test-server script](dev-test-server.md); it requires an
explicit EULA acknowledgment and compatible Java. `vesta create --help` describes
the normal server creation command.

If an agent is already running, the staged CLI can manage it. To run a stopped
test agent in this session (PowerShell):

```powershell
vesta list --json
# Substitute the srv_... installation ID from the list:
vesta agent serve "$env:VESTA_SERVER_ROOT\instances\<srv_id>\host.toml"
```

`serve` stays in the foreground; use a second dev session for management commands.
Ctrl+C requests its normal orderly shutdown. In `in-process` mode, stop Minecraft
and observe it stopped before shutting down the agent. In `separate` mode, an
agent restart can reattach to the existing supervisor and Minecraft. Replacing a
running supervisor itself requires a separate coordinated stop; rebuilding does
not replace that process. Never force-kill an agent as a normal rebuild step.

## Setup UI and installation

`vesta-setup preview` runs the production setup renderer's welcome preview.
`vesta-setup --help` lists preview states and installer arguments. Actual setup
installation still requires the signed bundle, hash pin, and release key described
in [host setup](host-setup.md); building the binary does not bypass verification
or make an unsigned development installation. Setup and agent onboarding remain
distinct workflows.

## Build without entering a shell

```powershell
cargo dev build
cargo dev build --release
```

These print the staged binary directory and return without changing PATH. Each
session/build is under the OS temporary directory's `vesta-dev-builds` directory;
the banner and `VESTA_DEV_BIN` identify the selected path. Snapshots are retained
for processes still using them and for debugging. Remove old session directories
manually after their agents/supervisors have stopped. Server data is stored in
the separate registry and is not part of this build cache.

Windows PowerShell 5.1 is the Windows shell dependency; Linux uses Bash.
Windows checks covered command
resolution for all four executables, live status against the existing dev agent,
rebuild switching, failed-build retention, setup preview, unchanged parent PATH,
and Ctrl+C leaving scoped management while preserving the dev shell. Restart
unit/ownership tests, protected local status metadata and PowerShell argument/
failure handling also pass. Ubuntu 24.04 WSL verifies Bash argument/failure
handling and the real agent restart flow with an owned inert game, both running
and stopped, in both supervision modes. The opt-in Windows detached-process test
was refused by the tool session's job containment (`Access is denied`, OS error
5); independent lifetime safeguards were retained rather than bypassed.

The opt-in lifecycle test requires a complete staged native build in
`VESTA_DEV_TEST_BIN` and runs with:

```text
cargo test -p vesta-dev real_restart_preserves_running_and_stopped_games --locked -- --ignored --nocapture --test-threads=1
```

It creates its own registry and inert test executable; it does not use a real
Minecraft installation or the existing demo. Failed fixtures are preserved for
inspection rather than removing data belonging to a potentially surviving process.
