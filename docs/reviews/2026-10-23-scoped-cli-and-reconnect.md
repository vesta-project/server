# Scoped CLI and launcher recovery follow-up

Owner direction: server operations belong under `vesta manage <server_id>`, with
an interactive terminal or one-shot command. Preserve individual server ownership
and explicit entry/exit. Earlier reconnect/manual refresh and startup CPU concerns
remain part of this follow-up.

## Implemented

- `vesta list` / `servers list`, optional JSON, selected registry root; no database
  opening, global server pings or directory creation while enumerating.
- Stable local `srv_...` selection plus name aliases, frozen per management session.
  A redirected alias cannot silently change the target. Roots cannot be switched
  inside a session. Existing standalone raw configurations are not silently adopted.
- `vesta manage <id>`: scoped line-editor prompt, help and explicit exit/back/quit;
  EOF and supported Ctrl+C/D behavior. Plain piped input is bounded and preserves
  failure exit codes. Commands are not persisted to history. Quotes preserve Windows
  and Unix paths. This is a structured client, not an OS command shell.
- Same command parser and effect implementations for one-shot commands, including
  lifecycle, configuration, access, pairing/devices/owners, backup and recovery.
- Protected native `status` / `player_list`. Player queries reuse generation,
  writer, timeout and console bounds; remote queries still require authenticated
  subjects at every existing authorization gate. Only LocalRuntime's trusted OS
  adapter can issue the local read. No forged principal or new HTTP grant.
- New signed release bundles include `vesta`; old signed three-component bundles
  remain accepted. Unknown fourth artifacts are refused. Existing `vesta-agent`
  scripts continue to work. See [CLI guide](../operations/scoped-cli.md).
- Launcher Refresh button in server header/Connection, plus card context menu.
  Calls the real native refresh, coalesces concurrent clicks, exposes pending state,
  retains trust/cooldown admission, and cannot restore a removed connection. A
  successful explicit recovery restarts ended subscription workers with demand.
- `bun run tauri dev` now aliases the same wrapper as `tauri:dev`; directly invokes
  the installed CLI without recursion, using the same dev profile/build directory.
- Background startup no longer prefetches Browse or runs Java/storage Settings
  scans. Actual page/hover intent still warms those caches. Idle callbacks are
  cancelled on disposal. Browser idleness is not host CPU or storage idleness.

## Validation and limits

- Agent: 29 unit tests plus three existing CLI and three new real-binary/scoped
  IO tests pass. Fresh native IPC tests cover live/stopped status, unavailable and
  successful player lists, and wrong-server refusal.
- Kernel: 20 player-query tests pass, including local fixed-list writing, timeout,
  generation binding and existing remote revocation checks. All 41 runner tests
  pass, including lifecycle/IO ownership and sleep. An initial incorrect test filter
  ran zero; corrected it to `operations::runner::tests` and verified 41 actual tests.
- Setup: 17 tests pass, including signed short-command installation, old inventory
  compatibility and refusing unknown artifacts. Three release packager tests pass.
- Strict agent/control/setup Clippy and workspace formatting pass. Release binaries
  `target/release/vesta(.exe)` and `vesta-agent(.exe)` built successfully.
- Actual owned Windows terminal: help and EOF/Ctrl+D work with `TERM=xterm-256color`;
  Ctrl+C exits cleanly too. `TERM=dumb` uses cooked OS input, explicitly documented.
  No runnable Minecraft was present in these terminal fixtures.
- Launcher: 52 focused checks pass; new refresh/startup scenarios capture installed
  Solid diagnostics. The startup bundle runs in an isolated realm to avoid creating
  a multiple-Solid diagnostic in the test runner. TypeScript and the native rewrite
  check pass. Native check retains existing unused/dead-code warnings and one
  incremental-cache access warning from the shared development target. Focused lint
  reports existing reactive callback warnings in server-page/card; no suppression.
- New wrapper's `bun run tauri --version` passes. The running raw dev session was
  observed using `src-tauri/target/debug`, bypassing wrapper isolation. The second
  same-named launcher process is its crash handler, not evidence of another GUI.
- This is not a measured elimination of the reported 50% cold-start spike. Full
  fresh-start native CPU/IO attribution remains needed; compiler CPU and a previous
  native window reconnecting to Vite must be separated from application startup.
- Collaborative browser snapshots timed out twice with `Preview automation snapshot
  timed out after 15000ms.` Logic/type/native checks passed; no visual acceptance
  claim is made from that failed browser session.

## Development agent recovery

Startup had failed with `local_shutdown_unproven_requires_supervisor_recovery`.
The user explicitly approved archiving the previous evening's pending marker and
restarting the rebuilt development agent. Immediately before recovery, no agent,
supervisor or Java process was running and port 8443 was free. The marker's bytes
matched the expected shutdown-not-proven evidence. It was moved intact to
`operator-sessions/e80a3694-b915-4690-941e-950ddd9de04d` in the existing dev instance
`srv_ef79ab69c41c4194996ba927e055da8c`.

At 2026-10-23 11:56 local time, rebuilt agent/CLI/supervisor binaries were staged
under the dedicated temporary dev root's `bin`; the replaced binary was preserved
under `bin/history/20261023-115645`. Agent PID 4388 owns `127.0.0.1:8443`.
`vesta manage dev-server status --install-root <dev-root>/servers` succeeds with
agent online and Minecraft stopped. No Java process is running. New logs are
`agent.recovered-20261023-115645.stdout.log` and the initially empty corresponding
stderr log. Existing configuration, world, TLS material and identity were retained.
This verifies local listener and management readiness; launcher reconnection still
needs observation in the user's running launcher.

Core commits: `c3e4e98` (local observations), `4b33300` (scoped CLI), `7b8c5a3`
(signed short-command packaging). Later small CLI wording fixes and launcher
changes are recorded in their own follow-up commits.

Launcher commits: `796ed64c` (explicit refresh and demand recovery), `3e894685`
(startup prefetch and dev-command consistency). No unrelated repositories or OS
settings were changed. Recovery applied only the concrete, built development agent;
no release publishing or public network exposure is part of this task.

## Reusable development shell

Added `cargo dev` / `cargo run -p vesta-dev --` and the
[development shell guide](../operations/dev-session.md). The helper builds all
four server commands, reads their real executable paths from Cargo's artifact
messages, and copies a complete set to a unique temporary snapshot. The child
PowerShell/Bash session gets PATH and the chosen registry for its lifetime.
`vesta-rebuild` selects another complete snapshot only after successful build and
copy. Running agents/supervisors keep their previous executable and sibling set;
no runtime restart, marker recovery, or installer trust bypass is implicit.

Verification: two staging/artifact tests pass, including incomplete-copy retention
and immutable previous binaries; strict helper Clippy and workspace formatting
pass. Actual Windows PowerShell 5.1 shell resolves all four executables, lists
`dev-server`, reads agent-online/Minecraft-stopped status, shows the production
setup welcome preview, and switches PATH to a new snapshot on rebuild. Injecting
a missing Cargo executable leaves the selected build unchanged. Parent PATH and
registry remain unchanged. The existing agent remains PID 4388 on port 8443.

Interrupt testing found Windows broadcasting Ctrl+C to the waiting helper too.
An isolated console adapter now handles only Ctrl+C/Break in that waiting parent;
child commands retain ordinary interrupt handling. Retested leaving `vesta manage`
with Ctrl+C: shell stays alive, rebuild and live status still work, and explicit
`exit` closes it. Build interruption before shell startup retains default behavior.
Bash syntax passes via Git Bash; Linux runtime behavior and full signed installer
installation were not exercised. No test shell is intentionally left running.

## Management response presentation

Scoped commands now use labeled human-readable status, player, account/device,
access and pairing sections when stdout is a terminal. `--json` selects the
original data response for one command; piped output and legacy entrypoints retain
their existing machine behavior. Native command authority, effects and wire DTOs
are unchanged. Unknown observations stay unknown and stored strings have terminal
control characters escaped. Lifecycle terminal acknowledgments no longer expose
the internal fixed-code wording or imply observed Minecraft readiness.

Validation: 33 agent unit tests and six local/scoped CLI integration tests pass;
strict all-target agent Clippy and workspace formatting pass. Actual Windows
management terminal shows formatted `status` and stopped `player_list`, then raw
structured `status --json`, and exits normally. No live server mutation was needed
for this check. Rebuild the CLI and re-enter management to use the presentation;
the running agent does not require a restart. Modpack readiness is recorded
separately in [the focused review](2026-10-23-modpack-readiness.md).
