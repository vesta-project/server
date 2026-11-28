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
Host realtime remains deferred. Per-server streams keep their existing URLs.

## One server

The `server-storage` capability enables `GET /v1/servers/{serverId}/manage/storage`
(the host-scoped form of `GET /api/v3/manage/storage`). It requires `storage.view`,
granted by default to owners and server administrators. It uses the API v3
management envelope with `data: {measuredAt,bytes,parts,volume,largest}`.
The volume contains `label`, `freeBytes` and `totalBytes`. Before that server's
first measurement, `measuredAt` and the three volume fields are null, sizes are
zero, and `largest` is empty.

`largest` contains up to ten `{path,bytes}` entries in descending byte order,
then path order for ties. Paths are relative to the Files tab's game root.
Folder sizes include their descendants and can overlap. Data comes from the
same background snapshot as machine storage. A standalone API v3 listener
uses its own startup and 15-minute measurements of the game root, managed
local backup repository, and private trash.

## Cleanup

The `storage-cleanup` capability enables owner-only POST
`/v1/host/storage/cleanup`. Send `{targets,dryRun}` with 1 to 100 targets:

- `{kind:"cache",cache:"java"|"software_downloads"|"catalogue"|"pack_staging"|"temp"}`
- `{kind:"logs",serverId,olderThanDays}` with an integer age in days, starting at zero
- `{kind:"trash",serverId}`
- `{kind:"removed",serverId,confirmName?}` with the exact name required for one server

`serverId` is a canonical UUID or `"all"`. Omit `confirmName` for `"all"`.
Targets of the same kind must cover distinct servers or caches. Preview with
`dryRun:true`, then reuse the targets with `dryRun:false` and a UUID
`Idempotency-Key` header. Dry runs ignore that header. Completed mutations
return the original receipt on retry. Changed input returns
`409 idempotency_conflict`. An incorrect name returns
`422 confirm_name_mismatch`.

The response is `200 {dryRun,items:[{target,bytes,skipped?}],bytes}`.
An `"all"` target expands to individual server rows when servers match.
Skipped rows report zero bytes and one of `in_use`, `busy` or `not_found`.
Removed servers are queued for purge with their backups retained. Their bytes
are estimates of the queued deletions. Log cleanup selects dated Minecraft
archives in stopped servers by modification time and leaves `latest.log`
alone. Trash cleanup retires completed copies that have no pending recovery
intent. Java, pack staging and temporary cache paths currently report
`in_use` when present because their ownership cannot be proven idle.
