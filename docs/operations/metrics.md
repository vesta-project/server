# Metrics and player-activity contract (F35)

Metrics are local operational observations, not a diagnostic export or a promise
that every platform exposes every measurement. Collection is passive by default,
except for explicitly opted-in tick command polling, and runs approximately every
10 seconds. Configuration is optional:

```toml
[metrics]
enabled = true
sample_seconds = 10
raw_retention_hours = 24
five_minute_retention_days = 30
daily_retention_days = 365
max_storage_bytes = 268435456
```

| Key | Default | Inclusive bound |
| --- | ---: | ---: |
| `enabled` | `true` | Boolean |
| `sample_seconds` | `10` | 5–60 seconds |
| `raw_retention_hours` | `24` | 1–168 hours |
| `five_minute_retention_days` | `30` | 1–365 days |
| `daily_retention_days` | `365` | 1–1825 days |
| `max_storage_bytes` | `268435456` | 1048576–8589934592 bytes |

The host `[metrics]` table is the fallback. Schema v24 adds `metrics_policy`; its row
is intentionally absent until an owner stores a runtime policy. The management
policy view reports `source: "host_config"` and a null revision while using host
configuration, or `source: "stored"` with its positive revision. Policy writes use
an optimistic expected revision and are audited; see the policy route below. The
stored document contains a nested `settings` object with the keys above and an
optional `command_polling` object:

```json
{
  "expected_revision": null,
  "settings": {
    "enabled": true,
    "sample_seconds": 10,
    "raw_retention_hours": 24,
    "five_minute_retention_days": 30,
    "daily_retention_days": 365,
    "max_storage_bytes": 268435456
  },
  "command_polling": { "enabled": false, "interval_seconds": 60 }
}
```

`command_polling.interval_seconds` is bounded to 30–3600 seconds; it defaults to
60 and polling defaults disabled. Enabling it is an explicit console-write opt-in,
not a zero-log observation. The collector implements receipt-gated, version-adapted
command polling when this setting is explicitly enabled; no command is sent while it
is disabled. The source is the exact protected launch receipt, not the untrusted
version string from a status ping. Missing, malformed, oversized, symlinked, or
launch-mismatched receipts leave tick values unknown and include the reason in the
sampled record. This remains an opt-in console write, not a connector or zero-log
observation.

The configured byte ceiling is a logical row-storage budget, not a promise about
SQLite physical file size or immediate disk-space reclamation. Cleanup is bounded per
tick: reducing retention or the ceiling can leave excess rows temporarily. Retention
uses the configured `five_minute_retention_days` for activity as well as 5-minute
aggregates. A storage ceiling prunes oldest raw rows first, then 5-minute rows, daily
rows, and activity rows; if the work bound is reached, later ticks continue cleanup.
`storage_bytes`, pruned row/byte counts, and activity-pruned status expose this state.
No samples or buckets are invented to fill missing intervals; missed/empty buckets
stay absent. Aggregates update incrementally and atomically with raw inserts. A
bucket's primary key makes retry/restart processing idempotent. Backwards wall-clock
steps of at most 10 minutes suppress samples until time catches up; a larger step
records a `clock_step` activity/status marker and sampling resumes at the new time.
The current observation is the last accepted sample, not the greatest timestamp, so
future-dated rows remain present if the system clock is corrected backward. Daily
buckets use UTC days.

## Collection and honesty rules

- Game CPU percent and RSS/working-set are sampled only for the current protected
  game-process identity, bound to generation and launch identity. Direct start captures
  identity while the child is owned; supervisor mode checks its protected record plus
  generation/launch data. Creation time is checked before sampling to avoid PID reuse.
  The agent metrics observe the agent's own process using its native process handle and
  creation identity; they do not use the game's protected generation record. CPU percent
  is unknown on the first sample. Process CPU uses a one-core-is-100% convention and may
  exceed 100% for multithreaded processes; host CPU is normalized to a 0–100% total-host
  scale.
- Host CPU and used/total memory, game-volume free space and local-backup-volume free
  space are sampled where supported. S3 free space is unknown. Host network receive
  and send rates use interface byte-counter deltas over elapsed monotonic time. Linux
  reads `/proc/net/dev` and verifies interface metadata under `/sys/class/net`; it
  includes only up, device-backed Ethernet/Wi-Fi interfaces. Windows reads native
  interface counters with the existing `windows-sys` package; it includes only up
  hardware Ethernet/Wi-Fi (`HardwareInterface=1`, `FilterInterface=0`,
  `ConnectorPresent=1`, `TunnelType == 0`). NDIS lightweight-filter layer rows are
  excluded so one adapter is not counted repeatedly; duplicate interface GUIDs make
  the observation unknown. Virtual bridges/VPN Ethernet are excluded to avoid duplicate traffic counting.
  The legacy series names `host_network_received_bytes` and
  `host_network_sent_bytes` hold bytes/second, not cumulative byte totals. Down,
  loopback, tunnel, virtual point-to-point and unknown interface types are excluded.
  These are whole-host interface rates, not a Minecraft-port estimate. A failed
  read, membership change, counter reset/wrap, or nonpositive interval makes that
  interval unknown rather than combining incompatible totals. There is no honest
  per-process network measurement: Windows would require a costly/invasive source
  such as ETW, while Linux namespace counters are not process-specific.
- `players_online` and `players_max` are populated only from a successful readiness/status
  ping no older than 30 seconds. A failed ping clears the player count; stale values are
  not retained. This status-ping collection does not list players or write to the game
  console; the separate explicit tick-polling opt-in below may write tick commands.
- TPS/MSPT and per-process receive/send metrics may be unknown. TPS/MSPT command
  polling is implemented only for a locally installed launch with an accepted
  `control/receipt.json` (schema 1 or 2, at most 64 KiB) whose literal Java program,
  working directory and argument suffix match the running launch specification;
  the receipt is not trusted based on a status-ping software/version string. External
  servers, mismatched launch shapes, unsupported versions and Folia remain unknown.
  Installation-receipt detection refuses symlinks and ambiguity. Separately, the
  native command-write receipt is retained until acknowledgement even after cancellation. Supported receipt identities are Paper,
  Purpur, Pufferfish and Leaf; Spigot/Bukkit; Vanilla/Fabric/Quilt from 1.20.3; Forge;
  and NeoForge from 1.20.2, for versions 1.16 through 1.21.x. Later or unknown
  versions (including 1.22 and year-based 26.x releases) remain unknown with
  `version not in the tested adapter matrix`. This is a narrow fixture-tested matrix,
  not general compatibility certification or a claim of real-server smoke testing.
- Polling runs only when the stored `command_polling.enabled=true` opt-in is present,
  the game is ready and the same launch generation remains active. Anchored INFO output
  must correlate after the write within three seconds and no more than 128 lines;
  gaps, truncation, unrecognized output, timeout, changed generation or revoked
  consent fail closed. No arbitrary line is treated as a response, and timeouts back
  off for four configured polling intervals. Paper-family alternates `mspt` and `tps`
  commands: each metric is sampled every two configured intervals (120 seconds each
  at the 60-second default); MSPT is the one-minute average and TPS is the one-minute
  value. Spigot reports one-minute TPS. Vanilla/Fabric/Quilt report measured average
  MSPT only; target TPS is not reported as achieved TPS. Forge/NeoForge report overall
  mean tick time and mean TPS only, not per-dimension values. Polling writes to the
  server console and is not a connector or zero-log source. No JMX/RMI remote
  connection is added; per-process network remains unknown. Console capture retains
  raw UTF-8 and does not strip ANSI escapes. The tick parser strips recognized section
  color codes but rejects ANSI/control escapes; whether a real Paper server emits ANSI
  to this piped console is unverified. Such output remains unknown rather than being
  speculatively normalized. INFO correlation is best effort, not authenticated proof
  that a matching plugin-produced line was caused by this command.
- Player activity is derived only from server INFO join/leave lines, using the same anchored prefixes as readiness: vanilla `[HH:MM:SS] [Server thread/INFO]: ` (optionally with a logger tag) and the Paper/Purpur/Folia console form `[HH:MM:SS INFO]: `. Chat, rejects, and
  other unrecognized lines are ignored; they do not alone create a gap. Truncated lines,
  lost cursors, generation/restart boundaries, and persistence loss produce gap markers;
  clock discontinuities produce `clock_step` markers. Activity history is incomplete
  when those observation gaps occur, including the initial startup gap; absence of events
  is not represented as proof of zero activity. Daily activity records the unique observed
  joining-player count and peak online count from status pings. The unique-player set
  is capped at 10,000; attempting to add a new name beyond the cap makes the count unknown/incomplete
  rather than silently undercounted. Daily identity sets are never included in metrics
  responses; the separately authorized activity-event page may include observed join/leave
  names.

Unknown values remain explicit null/unknown observations rather than zero. Metric
series are closed to the names below:

`game_cpu_percent`, `game_memory_bytes`, `agent_cpu_percent`,
`agent_memory_bytes`, `host_cpu_percent`, `host_memory_used_bytes`,
`host_memory_total_bytes`, `game_disk_free_bytes`, `backup_disk_free_bytes`,
`players_online`, `players_max`, `host_network_received_bytes`,
`host_network_sent_bytes`, `tps`, `mspt`, `game_network_received_bytes`,
`game_network_sent_bytes`.

## Read, policy, range and live API

All routes require a current canonical management bearer and recheck the exact
capability. Metrics reads/ranges/streaming require `MetricsView`; policy GET/PUT
require `ConfigView`/`ConfigEdit`; player activity requires `PlayersView`.

- `GET /api/v3/manage/metrics/current` returns `{sample,columns,status}`. The latest
  sample is separately stored and byte-accounted in status, so it remains available
  if its raw history row is later pruned. The response ETag is revision-based;
  one exact matching strong `If-None-Match` may return 304 after fresh authorization.
  Wildcard, list, weak and malformed validators return 400.
- `GET /api/v3/manage/metrics?series=a,b&from=<epoch-ms>&to=<epoch-ms>&resolution=auto|raw|5m|daily&max_points=2000&after=<revision>` accepts 1–8 unique names from the
  closed series list. `from` and `to` are inclusive, nonnegative epoch milliseconds;
  the span is at most five years, or seven days for explicit raw resolution. The
  optional `max_points` is 1–2,000 (default 2,000). Response columns share
  `timestamps` and include `series[name]` arrays for `avg`, `min`, `max`, `known_count`
  and `unknown_count`, plus aligned `gaps` and nullable `tick_source` arrays. The
  response includes the selected `resolution`, `bucket_millis`, and global monotonic
  database `cursor`. `after` is a revision cursor, not a timestamp: it returns buckets
  whose source rows changed after that revision, including mutable aggregate buckets;
  clients merge/replace by timestamp. `auto` chooses a tier that fits the requested
  point bound and available data; explicit resolutions select raw, 5-minute or daily.
  Once pruning or a stored policy change has occurred, an incremental response for a stale cursor sets
  `retention_gap`, requiring a full-window refresh because no tombstone log is kept.
  Range ETags bind the query and global cursor. A single matching strong
  `If-None-Match` validator may return 304 after fresh authorization; stale validators
  return the fetched 200 response. Wildcards, lists and weak/malformed validators are
  rejected with 400. `after=<current cursor>` without a conditional header returns
  200 with an empty shared timestamp axis and empty requested-series columns.
- `GET /api/v3/manage/metrics/policy` returns the effective source (`host_config` or
  `stored`), nullable/positive revision, nested `settings`, `command_polling`, current
  `storage_bytes`, and explicit `command_polling_writes_console: true`. This route
  does not accept conditional headers.
- `PUT /api/v3/manage/metrics/policy` accepts `expected_revision` (null for no
  stored row, positive current revision otherwise), required nested `settings`, and
  optional `command_polling`. The write compares revisions, validates the closed
  bounds, increments the revision and appends its audit record transactionally.
  Stale expected revision returns 409 `metrics_policy_conflict`. Policy mutation is
  a configuration action, not a MetricsView grant.
- `GET /api/v3/manage/metrics/stream` is a native-client WebSocket; it accepts no
  query/body/cookie/Origin. It starts with `metrics.snapshot` carrying the same
  `columns` shape as `current` plus `status`, then sends `metrics.sample` with the
  latest columns when a sample changes. It has no accumulating queue: skipped
  revisions or activity overflow send `metrics.gap` with `mode: "latest_only"` and
  the latest cursor. Up to 32 latest player activity events may follow, each freshly
  authorized with `PlayersView`; missing that capability suppresses identity events.
  `MetricsView` is checked on every push and heartbeat. It shares the WebSocket
  limits: 32 global and two per session, 2-second send timeout, 10-second heartbeat,
  and 30-second idle closure. No SSE endpoint is implemented.
- `GET /api/v3/manage/players/activity?after=<numeric-id>&limit=<1..100>` returns
  bounded ordered event pages. `next_after` is the numeric event ID cursor (or null
  when there is no following page). This PlayersView-only event route may include
  observed join/leave names; daily identity sets are not returned.

Metrics responses never contain daily player identities. TPS/MSPT can be populated
only by the receipt-gated, explicitly opted-in adapters described above; otherwise
they remain unknown. Per-process network series remain unknown. Data is local state
and excluded from recovery exports.
