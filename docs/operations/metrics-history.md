# Launcher metrics history (D9)

The launcher now consumes the existing server-owned `GET /api/v3/manage/metrics`
range API through `server_metrics_history` and TS `metricsHistory`. Requests carry
series (tps, game_cpu_percent, game_memory_bytes, players_online), from/to Unix ms,
resolution (raw / 5m / daily / auto), maxPoints (1–2000), and optional decimal-string
after cursor. Raw reads span at most seven days; other windows at most five years.
`metrics.view` is required. No client console polling or synthetic history is used.

MetricsHistory returns aligned timestamps, per-series avg/min/max/knownCount/
unknownCount arrays, gaps, nullable tickSource per row, resolution, bucketMillis,
decimal-string cursor and retentionGap. Series map keys stay snake_case; DTO field
names are camelCase. Null means unknown and must never be rendered as zero. Empty
history and a wholly-null TPS series are valid; render separate graphs only when
that series has known samples. Gaps break lines; unknown aggregate counts explain
partial five-minute buckets. A retentionGap requires a full-window reread.

Fixtures provide realistic CPU/memory/player/TPS variation, two-sample gaps,
five-minute aggregates and fixture-creative with all TPS values unknown and no
TPS source. UI follow-up: seed overview plots from a range read, then merge live
samples or incremental updates; replace mutable five-minute buckets by timestamp.
Keep the established disrupted-only freshness indicator rule in the UI engineer's
worktree.
