# Build and test from a temporary development shell

From this repository, run:

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
exit
```

`vesta-rebuild` repeats the build with the same debug/release profile, stages a
complete fresh set of executables, and changes this shell's PATH to that set.
A failed compile or incomplete copy leaves the previous selection in place.
Cargo supplies the executable paths, so custom `CARGO_TARGET_DIR` and Cargo target
directory configuration are supported. This workflow builds for the native host;
do not set Cargo's build target to an incompatible platform for an interactive run.

Running agents and supervisors continue using their original build. Rebuilding
does not restart anything, interrupt Minecraft, or clear recovery markers. Copies
are deliberately staged outside Cargo's output directory so a Windows process
does not lock the next build's destination. All four binaries stay together so an
agent can locate the matching supervisor beside itself.

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

Windows PowerShell 5.1 is the Windows shell dependency; Linux uses Bash. Linux
execution still needs platform verification. Windows checks covered command
resolution for all four executables, live status against the existing dev agent,
rebuild switching, failed-build retention, setup preview, unchanged parent PATH,
and Ctrl+C leaving scoped management while preserving the dev shell. Bash syntax
was checked on Windows using Git Bash; this is not Linux runtime verification.
