# Disposable launcher test server

## Daily Windows development commands

From `V:\server-connections-mvp` after the initial installation:

```powershell
.\dev.ps1                # build and update dev-server, then return
.\dev.ps1 restart        # same update operation
.\dev.ps1 shell          # optional tools shell
# Inside the optional shell:
vesta manage dev-server status
vesta manage dev-server agent stop
```

Normal updates preserve Minecraft's running/stopped state. See the
[development guide](dev-session.md). Use the initial-installation script below
only when creating the disposable fixture; it is not the daily update command.

### Legacy scratch recovery

The older `scripts/dev.ps1` remains available for explicit scratch recovery.
Use `.\scripts\dev.ps1 recover` for recovery without starting the agent. Recovery
is restricted to a marked scratch registry, takes registry/instance locks, refuses
linked paths and unknown marker contents, and requires successful OS process
enumeration showing **no JVM, agent, supervisor or setup host anywhere on the host**. The protected
launch must name a plain literal Java executable; inert children, wrappers and
linked/custom executable layouts require manual investigation. It
archives the exact pending marker in `operator-sessions/dev-recovered-*`;
it never deletes game data or infers stop from released locks alone. Stop other
JVMs cleanly before retrying if it refuses. Production orphan recovery remains
supervisor/operator work; this command is only for the disposable dev instance.
`start` refuses a configured automatic start/sleep policy. Set `[process]`
`on-agent-start = "stay_stopped"` (or omit the process table) for this workflow.
Do not use `Stop-Process` for routine agent shutdown: the local `shutdown`
command waits for runtime/I/O completion and archives the session marker.

On 2026-10-25 the real `%TEMP%\vesta-dev-server` marker was archived under these
checks, the debug agent rebuilt and staged, and the agent restarted with Minecraft
stopped. Identity, invitations, TLS material and game data were retained.

From the repository root, Windows PowerShell **5.1**:

```powershell
.\scripts\dev-test-server.ps1 -AcceptEula
# Keep running after this command returns:
.\scripts\dev-test-server.ps1 -AcceptEula -Background
# Override an older JAVA_HOME for current Minecraft:
.\scripts\dev-test-server.ps1 -AcceptEula -Java 'C:\Program Files\Eclipse Adoptium\jdk-25\bin\java.exe' -Background
```

Linux:

```sh
./scripts/dev-test-server.sh --accept-eula
./scripts/dev-test-server.sh --accept-eula --background --java /path/to/jdk/bin/java
```

Read <https://www.minecraft.net/eula> before passing `-AcceptEula` /
`--accept-eula`. Without it the real-Minecraft path prints that link and stops
before building, installing, or accepting anything. The ordinary agent installer
still leaves EULA acceptance pending; this script writes `eula=true` only after
explicit consent. The script acknowledges installer first-run warnings and the
official Mojang legacy SHA-1 integrity metadata. Downloads still use the agent's
normal HTTPS, size, integrity, Java compatibility and installation checks.

Requirements: Rust/Cargo plus the native build toolchain, outbound HTTPS for the
first Minecraft install, and a compatible Java executable. Linux also requires
Python 3 (3.8+) and curl. No OpenSSL, administrator account or global certificate
installation is required. The TLS helper is a development example in
`vesta-setup`, reusing its existing certificate generator.

The script builds a debug `vesta-agent` plus the helper, copies the runtime into
scratch `bin/` so it does not lock Cargo's build output on Windows, installs a real vanilla
Minecraft server with `vesta-agent create`, creates TLS material privately, starts
`serve`, waits for the instance's local control socket, runs
`pair dev-server --role owner`, prints the link and TLS SHA-256 fingerprint, and
starts Minecraft through the local CLI. The launcher can use the pairing link as
printed. Compare the displayed fingerprint when approving first-contact trust.
The script waits for agent readiness; Minecraft world generation can continue
after `local_game_operation_complete; readiness_not_observed`. Check the launcher's
readiness or `game/logs/latest.log` for Minecraft's `Done` line.

Java resolution is explicit `-Java` / `--java`, then `JAVA_HOME`, then `java` on
PATH. The installer refuses incompatible runtimes. An existing `JAVA_HOME` can
be older than PATH: pass the desired executable when that happens. The latest
release is resolved from Mojang's version manifest **at first installation**.
Reusing the same scratch root preserves the installed version, world, server
identity, TLS certificate and trust; use a different root or reset to reinstall.

Parameters (PowerShell / Linux):

- `-Root PATH` / `--root PATH`: dedicated scratch directory; defaults to
  `%TEMP%\vesta-dev-server` / `${TMPDIR:-/tmp}/vesta-dev-server`.
- `-Port 8443` / `--port 8443`: agent HTTPS port. The bind is always `127.0.0.1`,
  and `public-endpoint` is `https://127.0.0.1:<port>/`.
- `-GamePort 25565` / `--game-port 25565`: Minecraft's loopback port at initial
  creation. Reset to change it on an existing root.
- `-Version VERSION` / `--version VERSION`: exact Minecraft version, default
  `latest`. Changing a scratch root's requested version/mode requires reset.
- `-Loader fabric -LoaderVersion VERSION` /
  `--loader fabric --loader-version VERSION`: optional Fabric installation via
  the same agent installer. Omit loader version to use its default resolution.
- `-Release` / `--release`: build release binaries. Honors `CARGO_TARGET_DIR`.
- `-Background` / `--background`: return after printing pairing/stop instructions.
  Otherwise the script remains attached; Ctrl+C stops the disposable server.
- `-Inert` / `--inert`: explicitly use the same owned child executable as the
  TLS integration tests. No Java, Minecraft download, real Minecraft network
  listener, or EULA acceptance. It answers `list`, echoes commands and exits on
  `stop`. It does not simulate world, plugin, log-readiness or gameplay behavior.
  It writes a fixture-only `eula=true` to satisfy the local CLI's generic gate.
  Installation/network failures never silently switch to this mode.
- `-Reset [-Force]` / `--reset [--force]`: **delete and exit**. Only a marked
  scratch root is eligible, the server must be stopped, linked paths are refused,
  and the default confirmation requires typing `DELETE`. `Force` skips that
  confirmation, not the path/marker/running-process checks.

The scratch directory contains private TLS keys, management state and worlds.
Never commit it or share pairing output/log captures. A pairing invitation is
short-lived; obtain a fresh one with the printed executable and install root:

```powershell
& "$env:TEMP\vesta-dev-server\bin\vesta-agent.exe" pair dev-server --install-root "$env:TEMP\vesta-dev-server\servers" --role owner
& "$env:TEMP\vesta-dev-server\bin\vesta-agent.exe" stop dev-server --install-root "$env:TEMP\vesta-dev-server\servers"
# Only after the game stopped, use the exact agent PID printed by the script:
Stop-Process -Id <agent-pid>
.\scripts\dev-test-server.ps1 -Reset
```

```sh
/tmp/vesta-dev-server/bin/vesta-agent pair dev-server --install-root /tmp/vesta-dev-server/servers --role owner
/tmp/vesta-dev-server/bin/vesta-agent stop dev-server --install-root /tmp/vesta-dev-server/servers
kill -INT <agent-pid>
./scripts/dev-test-server.sh --reset
```

Use the printed paths when overriding root, target directory or release profile.
Reset refuses a live recorded PID rather than terminating an unrelated process.
Agent stdout/stderr are captured at `agent.stdout.log` and `agent.stderr.log` in
scratch. The scripts do not install services, register startup tasks, change a
firewall, expose a public listener, or modify the launcher.

## Verification on 2026-10-21

Windows PowerShell 5.1 real-server run used:

```powershell
.\scripts\dev-test-server.ps1 -AcceptEula -Background `
  -Java 'C:\Program Files\Eclipse Adoptium\jdk-25.0.3.9-hotspot\bin\java.exe' `
  -Port 18443 -GamePort 25585 `
  -Root "$env:TEMP\vesta-dev-server-activity-smoke"
```

Mojang's manifest resolved vanilla **26.3** (Java 25), server JAR **62,294,556
bytes**, SHA-1 `33680f5f2ac32864d6d7cf5e56a705fdb3e05f4c`. The existing installer
downloaded and published it, the configuration check passed, the HTTPS agent
started, and `pair` returned an owner `vesta://pair?...` link with the matching
TLS fingerprint. Minecraft bound `127.0.0.1:25585` and logged
`Done (4.955s)!`. A public HTTPS `/api/v3/status` request succeeded. Generated
keys, invitation codes, JARs and worlds stayed outside the repository.

During development the first attempt selected Java 21 from `JAVA_HOME` and was
refused. Repeating with explicit Java 25 exposed the legacy-checksum acknowledgment;
the scripts now pass and print that acknowledgment. The configuration smoke check
also caught required listener limits and Windows path formatting before launch;
both were fixed before the successful run. PowerShell redirected native-stderr
handling was corrected so Cargo progress is not treated as a failed build.

A subsequent real-server run verified the final scratch `bin/` staging behavior;
Minecraft logged `Done (2.341s)` and the public endpoint reached `online`.
An independent `-Inert -Background` run
without `-AcceptEula` produced a pairing link and the fixture's `tls-inert-ready`
marker. Both children stopped through `vesta-agent stop`; their agents were then
stopped and scratch roots removed using `-Reset -Force`. Reset while the real
agent was live correctly refused. Bash syntax was checked with `bash -n`; the
Linux script was not executed end to end on this Windows host. Fabric installation
was not exercised.
