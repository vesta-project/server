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

## Current development agent blocker

The latest startup stderr is `local_shutdown_unproven_requires_supervisor_recovery`.
`operator-session-pending` dates from the previous evening. No agent, supervisor or
Java process was present when checked. The marker is preserved; the running dev
installation has not been silently repaired or its identity/credentials rotated.
An explicit approval request to archive that evidence and start the rebuilt agent
is pending. No Minecraft start or stop is implied by entering the management shell.

Core commits: `c3e4e98` (local observations), `4b33300` (scoped CLI), `7b8c5a3`
(signed short-command packaging). Later small CLI wording fixes and launcher
changes are recorded in their own follow-up commits.

Launcher commits: `796ed64c` (explicit refresh and demand recovery), `3e894685`
(startup prefetch and dev-command consistency). No unrelated repositories or OS
settings were changed. The permission request is for the concrete, built agent;
no release publishing or public network exposure is part of this task.
