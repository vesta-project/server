# Host events design

`GET /v1/host/realtime` currently returns `501 realtime_multiplexer_deferred`
after bearer authentication. This design describes the future `host-events`
capability. The launcher can poll machine storage and the two server lists
while the capability is absent.

## Wire contract

Native clients upgrade an HTTP/1.1 GET without a body, query, Cookie or
Origin. Current `primary_owner` and `owner` sessions may subscribe. After
upgrade the client sends one message:

```json
{"op":"subscribe","channel":"host","after":null}
```

`after` is a host-scoped decimal string or null. Null begins live delivery.
For an accepted subscription, send
`{"op":"subscribed","channel":"host","cursor":"123"}` with the upper bound
observed during admission. Replay valid resume cursors strictly after
`after` in increasing order, then continue live delivery. A client resuming
keeps its last applied event cursor while applying replay. A client
starting live can fetch its first snapshots after the acknowledgment.

Each event is `{"channel":"host","cursor":"124","type":"server.removed","data":{...}}`.

| Event | Data | Publication point |
| --- | --- | --- |
| `server.removed` | `{serverId,removedAt,restoreUntil}` | Recoverable removal transaction |
| `server.restored` | `{serverId,name,gamePort,restoredAt}` | Registry reactivation transaction |
| `server.purging` | `{serverId,purgeStartedAt}` | Direct permanent delete, manual purge, retention expiry or cleanup queue transaction |
| `server.purged` | `{serverId,purgedAt}` | Purge completion transaction after filesystem work |
| `storage.measured` | `{measuredAt}` | After the completed cache snapshot is available |

On an invalid, expired or unretained resume cursor, send
`{"op":"reset","channel":"host","cursor":"123"}` using the current high-water
cursor, then deliver events after that cursor. This message replaces the
subscription acknowledgment on gap. Refetch the active list, removed list
and storage snapshot after reset. If an invalidation arrives during a
refetch, mark that snapshot dirty and refresh it again. Reconnect with a
current session and the last applied cursor. Event data stays limited to
invalidation facts in the table.

## Repository and worker boundaries

Use a dedicated host SQLite table with an increasing integer sequence and
a persisted high-water value. Use the existing host writer to append
lifecycle events in the same transactions as their registry or purge
state updates. Prune to 1,000 entries and 24 hours. Replay must enforce
the age cutoff even when the host has been idle. Retain the high-water
value when the last event ages out. Read the replay floor, high-water and
page in one transaction. Copy results out before network I/O. Retries
that return a stored lifecycle receipt must leave the log untouched.

Publish storage events after a measurement is accepted into the cache.
Pair publication with the actual accepted measurement, since a newer
storage refresh can cause an in-flight walk to be discarded. Schedule a
fresh measurement on startup so a restart can republish a valid snapshot.

Use a host-specific socket budget, bounded inbound frame size and send
deadline. Admit database work through the bounded blocking pool. Poll
authority even during an idle connection and recheck before each
delivery, including replay and reset. Check the current device bearer
session and host owner role. Close on expiry, revocation, demotion,
shutdown or a stalled send. Join socket tasks during shutdown. Existing
per-server WebSockets continue using their worker-based authority.

## Verification before advertising

Cover live subscription, replay across restart, identical mutation
retries, both retention bounds, invalid and future cursors, the snapshot
fetch race, owner demotion, device revocation, idle expiry, and slow
clients. Verify that lifecycle rollback never publishes and storage
invalidation cannot precede snapshot visibility. Verify these behaviors
on Windows and Linux before adding `host-events` to HostInfo.
