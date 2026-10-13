# Metrics and player-activity contract (F35)

Metrics are local operational observations, not a diagnostic export or a promise
that every platform exposes every measurement. The sampler is passive and runs
approximately every 10 seconds. Configuration is optional:

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
| `five_minute_retention_days` | `30` | 1–90 days |
| `daily_retention_days` | `365` | 1–730 days |
| `max_storage_bytes` | `268435456` | 1048576–1073741824 bytes |

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
  and send are monotonic total counters from `/proc/net/dev` where available; Windows
  host network counters are unknown. There is no per-process network measurement.
- `players_online` and `players_max` are populated only from a successful readiness/status
  ping no older than 30 seconds. A failed ping clears the player count; stale values are
  not retained. The collector does not list players and does not write to the game console.
- Vanilla TPS/MSPT and per-process receive/send metrics are always unknown; no plugin,
  command, or console adapter is used to infer them.
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

## Read API

All routes require a current canonical management bearer and the exact capability.
The metrics endpoint requires `MetricsView`; activity requires `PlayersView`.

- `GET /api/v3/manage/metrics/current` returns the latest observation/status. The
  latest observation is separately stored and byte-accounted in status, so it remains
  available even if its raw history row is later pruned.
- `GET /api/v3/manage/metrics?series=<closed>&from=<epoch-ms>&to=<epoch-ms>&resolution=auto|raw|5m|daily`
  returns points whose sample/bucket-start timestamps are in the inclusive range
  (`from <= at <= to`). The range span cannot exceed 730 days. At most 2,000 points
  are returned; requests needing more are refused rather than truncated. `auto` selects
  raw through 2 hours, 5-minute through 6 days, and daily above 6 days; explicit
  resolutions select the named raw, 5-minute, or daily tier.
- `GET /api/v3/manage/players/activity?after=<numeric-id>&limit=<1..100>` returns
  bounded ordered event pages. `next_after` is the numeric event ID cursor (or null
  when there is no following page). This PlayersView-only event route may include
  observed join/leave names; daily identity sets are not returned.

Metrics responses never contain daily player identities. TPS/MSPT and per-process
network series return unknown values, not fabricated measurements. Data is local
state and is excluded from recovery exports.
