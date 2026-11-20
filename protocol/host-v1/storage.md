# Machine storage

The `storage` capability enables GET `/v1/host/storage` and POST
`/v1/host/storage/measure` for primary owners and owners. Other device sessions
get `403 forbidden`. Host errors use the nested `error` envelope.

GET returns the last completed measurement. Its fields are `measuredAt`
(null before the initial measurement), `measuring`, `volumes`, `servers`,
`removed` (`count` and `bytes`), `backups` (`bytes` and `outsideServers`),
`caches` and `other`. Sizes are integer apparent bytes. Active servers are
ordered by descending size and include the registered name, volume ID and
a breakdown of worlds, backups, content, logs, software, trash and other.
Volumes report `volumeId`, `label`, `displayPath`, `freeBytes`, `totalBytes`,
`holds` and `lowAt`. Caches list kind, bytes and whether the data is removable.

The host walks managed local directories on a background thread. It queues
a measurement on startup, refreshes after observed host registry changes,
and measures every 15 minutes while idle. POST accepts an empty body or
`{}` and returns `202 { "measuring": true }`. Concurrent requests coalesce.
A request during the minimum interval since the last completed admission
gets `429 rate_limited` with `Retry-After` in seconds. While `measuring` is
true, poll GET. The host preserves the last completed snapshot if a walk fails.

Values can change while a walk is running. The current projection covers
managed server, removal, retained backup, state and known cache roots.
External repositories and nonstandard paths can be outside these totals.
Cache deletion and per-server storage endpoints are deferred. Host realtime
remains deferred and per-server streams keep their existing URLs.
