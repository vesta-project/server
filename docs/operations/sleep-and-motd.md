# Sleep, temporary status and MOTD

Backend endpoints are available for frontend integration. The hosting screens and
visual MOTD editor still need to be built and tested.

## Sleep policy

`GET /api/v3/manage/sleep/policy` requires `ConfigView`. Initial revision is `0`;
sleep, offline status and wake all default off. `PUT` requires `ConfigEdit` and
the current `expected_revision`; stale writes return 409 `sleep_policy_conflict`.

```json
{
  "expected_revision": 0,
  "policy": {
    "enabled": true,
    "offline_status": true,
    "wake_on_join": true,
    "idle_seconds": 600,
    "warning_seconds": 30,
    "start_cooldown_seconds": 30
  }
}
```

The configuring principal/device supplies durable execution authority. Sleep also
requires `LifecycleStop`, `ConsoleRead`, `ConsoleWrite` and `PlayersView`; wake
requires `LifecycleStart`. Authority, current policy and exact intent are checked
before effects. Revoking start authority disables wake while keeping status
available. Audit failure rolls back configuration. Reconfiguration preserves the
durable start cooldown and sequence, invalidating old queued intents.

Idle time is continuous verified zero-player time **before** the warning. Countdown
starts after the warning is acknowledged, then adds `warning_seconds`. Bounds:
60–86400 idle seconds, 5–300 warning seconds and 10–3600 cooldown seconds.
Zero requires a fresh status probe and generation-bound `list` reconciliation.
Missing, truncated or gapped output, joins and unrelated console lines reset the
evidence; this is deliberately conservative on noisy/unrecognized servers.
The management roster sends at most one `list` seed per running generation. A
failed/timed-out seed remains unknown: subsequent reads scan for a late response
without sending another command. Only an actual loss of captured console output
permits one reseed. Complete join/leave events then maintain the roster, including
Minecraft 26.3's anchored server INFO `System chat:` form. Metrics and HTTP read
frequency do not cause periodic `list` commands. Explicit local `player_list` and
the low-level console query API remain deliberate fresh queries.
Incompatible operations, backups, pending file effects and stopped-backup holds
defer sleep. The final native stop guard rechecks evidence/policy; supervised stop
also checks the supervisor's own console horizon.

Both the sleep policy response and authenticated management status (including
native status-stream snapshots) include a read-only `sleep_blocker` diagnostic:

```json
{"sleep_blocker":{"reason":"counting_down","remaining_seconds":42}}
```

Reasons are `disabled`, `not_ready`, `players_online`, `player_count_unknown`,
`operation_in_progress`, `hold`, `authority_unavailable`, `counting_down` and
`warning_sent`. Only the last two include `remaining_seconds`, rounded up: idle
time remaining before the warning, or warning time remaining before sleep.
`not_ready` includes a stopped, starting, degraded or unhealthy game. `hold`
identifies a held start/maintenance gate; other unfinished operations, backups and
file effects report `operation_in_progress`. Unknown/failed player or console
evidence reports `player_count_unknown` and never means zero players. The warning
operation itself reports `operation_in_progress` until acknowledged. Revoked
execution authority reports `authority_unavailable`.

This is the coordinator's latest observation (normally updated every 250 ms while
enabled), not a sleep admission guarantee. A policy revision change invalidates
the old diagnostic immediately. Status/configuration reads do not issue commands.
The launcher client/IPC uses `sleepBlocker` and `remainingSeconds`, preserving the
snake_case reason values. Missing diagnostics from older agents map to null.
Process `state`, canonical `lifecycle`, Sleep/Stop behavior and UI pages are
unchanged; this field is available for a future explanatory UI.

`game_port_known` means the configured address parsed, not that bind succeeded.
Canonical Java-properties decoding rejects duplicate/escaped-key ambiguity. A
valid explicit `server-port` is required. Port changes require agent restart.

## Status and wake

`GET /api/v3/status` exposes coarse public lifecycle/MOTD, without player names,
generation IDs, principal/device IDs, paths or administrative errors. Authenticated
server status preserves process `state` and adds canonical `lifecycle`: offline,
sleeping, waking, starting, online, stopping, crash loop and recovery required.
Maintenance templates exist; the maintenance admission overlay is not implemented,
and stopped-backup holds are not mislabeled maintenance.

The actual game owner (agent or supervisor) owns the temporary responder. It closes
and joins it before every launch, including manual/queued starts, restart
continuations and supervisor crash restart. Uncertain release refuses launch. A
running Minecraft server owns its port and live status. During startup handoff the
game port can be unavailable; HTTPS status remains available. No always-on proxy
or login transfer is implemented.

Pings never wake or consume the join-wake budget. A complete compatible login-start
packet enqueues a hint; the coordinator admits one durable start under fresh
authority and persisted cooldown. The disconnect reports a **request**, not
guaranteed admission. Reconnect after readiness. The temporary responder cannot
authenticate a Minecraft UUID; Minecraft enforces admission after startup. A
suspended crash breaker refuses start until the existing explicit reset clears it.

Bounds: eight clients, a 750 ms total read/write deadline, 4 KiB input frames,
16 wake hints, 256 tracked sources and one wake hint per source per ten seconds.
Same-address presentation changes retain the listener, clients and source budget.
Offline online-player count is always zero with no sample names. Transitional
Vesta status echoes the client's handshake protocol; real game compatibility is
negotiated after reconnect, not certified by the temporary reply.

Supported login forms: username alone or username plus UUID. Signed-key,
intermediate optional-UUID/modded extensions, proxy-forwarded headers and tunnel
negotiation are not handled. CGNAT, TCP forwarding services, multiple game ports
and bots need separate compatibility testing. Polling remains non-waking.

The reserved `sleep-runtime` row is transient operational state. Recovery export
excludes it, and import rejects restoring it. A normal agent restart may reconcile
a durable wake intent; a portable recovery import cannot revive an old wake/stop.

## MOTD settings

`GET /api/v3/manage/motd/policy` requires `ConfigView`. `PUT` requires `ConfigEdit`,
`expected_revision` (null initially) and `document`; conflict returns 409
`motd_policy_conflict`. Configuration/audit commit atomically. The reserved typed
policy is bounded to 32 KiB; the generic settings editor keeps its 4 KiB limit.

Document fields: `states` (all nine state keys), `normal_icon_png`, `login_message`.
State keys: `online`, `offline`, `sleeping`, `waking`, `starting`, `stopping`,
`maintenance`, `crash_loop`, `recovery_required`. Each presentation has `template`,
`version_name`, `maximum_players`, `icon_png`. Images are base64 PNG, not paths/URLs.

`POST /api/v3/manage/motd/preview` accepts `{ "state": "offline", "document": ... }`
under `ConfigView`. It returns structured text, two plain lines, an icon and the
placeholder catalogue. Inputs are public configured metadata, not caller-supplied
diagnostics. Duplicate states, unknown fields/placeholders and bad syntax refuse.

```text
{server_name} {state} {online_players} {max_players}
{minecraft_version} {loader_version} {pack_name} {pack_version}
{maintenance_reason} {queued_operation} {elapsed_seconds}
{estimated_ready_seconds} {public_failure}
```

Failures/operation labels are a closed safe vocabulary. Readiness estimates remain
unknown until one exists. Elapsed time is monotonic time since this agent observed
the current state/generation; prior offline duration is not reconstructed.
Insertion is nonrecursive and cannot inject formatting. Paths, credentials,
exception text and administrative details have no placeholder.

Templates: at most two lines, 1024 source bytes, 512 rendered UTF-16 units, plus a
rendered-component byte limit. Standard `§0`–`§f`, `§k`–`§o`, `§r` colors/styles
produce structured text with no arbitrary click/hover actions or raw JSON.
Icons: noninterlaced 64×64 RGB/RGBA 8-bit PNG; CRC, decompressed size and filters
are checked and metadata stripped. Invalid/oversized assets use a generated
fallback, including normalized images over the 4 KiB public icon budget.
Maximum players/version labels are configurable; offline online players/names
are never fabricated.

The online/app template is a presentation preview: it does not alter Minecraft's
live response, even after restart. Edit the ordinary live MOTD through the existing
authorized `server.properties` route. That editor now preserves two-line colors
and Unicode through Java-properties escapes and reports `restart_needed`.
Existing file authority, optimistic hash, confirmation and authorized raw-file
access still apply. No broad plugin reload is issued. Dynamic live templates
remain a connector capability. Frontend color/placeholder controls are pending.

## Verification limits

Windows loopback and owned-process tests cover non-waking ping, malformed login,
source budget/update continuity, actual child port handoff, durable coalescing,
revocation, warning timing, joins/unknown evidence, holds, crash-loop refusal,
settings rollback, rendering/icons and recovery-intent exclusion. They do not
certify live Minecraft/loader/client compatibility, Linux socket/IPC behavior,
the maintenance overlay or new frontend screens.
