# Removed servers

All routes here require a current host session for a primary owner or owner.
Error bodies use `{ "error": { "code": string, "requestId": string } }`.
A non-owner gets `403 forbidden`. Read the role and the relevant capability
before exposing each action.

| Operation | Capability | Request | Success |
| --- | --- | --- | --- |
| Remove an active server | `server-remove` | DELETE `/v1/host/servers/{serverId}`, JSON `{confirmName,idempotencyKey}` | 200 `{serverId,removedAt,restoreUntil}` |
| Permanently remove an active server | `server-purge` | Same DELETE with JSON `{confirmName,idempotencyKey,permanently:true,keepBackups?:boolean}` | 202 `{serverId,purgeStartedAt,bytes}` |
| List removed servers | `removed-servers` | GET `/v1/host/servers/removed[?cursor=...]` | 200 `{servers,nextCursor}` |
| Restore a removed server | `removed-servers` | POST `/v1/host/servers/removed/{serverId}/restore` with `{name?:string,gamePort?:integer}` and `Idempotency-Key` | 200 `{serverId,name,gamePort,restoredAt}` |
| Purge a removed server | `server-purge` | DELETE `/v1/host/servers/removed/{serverId}` with `{confirmName,keepBackups?:boolean}` and `Idempotency-Key` | 202 `{serverId,purgeStartedAt,bytes}` |

Use a UUID for each key. Recoverable and permanent removal use the existing
body key. Restore and purge use the header. Completed lifecycle replays return
the original receipt and HTTP status. Reuse of a key with changed input on
the same operation returns `409 idempotency_conflict`. A header key is required
for new writes. Confirmation is case-sensitive and returns
`422 confirm_name_mismatch` on mismatch. Removal returns `409 server_busy`
if the server has an active job. New operations return `409 purging` while an
admitted purge runs, `410 gone` after it finishes and `404 not_found` for unknown
or inapplicable targets.

Rows are ordered by removal time descending and server ID descending. Follow
the opaque `nextCursor` when a page of 100 entries has older results. Each row
includes `serverId`, `name`, `removedAt`, `restoreUntil`, `removedBy`, `state`,
`bytes`, `minecraftVersion` and `software`. Unknown measurements and software
facts are null. The list reports `restorable` or `purging`. Permanent removal of
an active server skips the removed list. Both purge routes capture `bytes`
from the last measurement, or null when the server has yet to be measured.
This is a size estimate for the server tree and can include backups that will
be retained. Retries return the original receipt. Purge completion refreshes
the background storage snapshot.

Soft removal stops the game, revokes grants and invites, releases the name
and game port and isolates the server directory. Restore returns the same
server ID and a stopped server. Invite and grant access must be reissued.
Restore tries the original or requested name followed by ` (restored)`,
` (restored 2)` and so on. It tries the original port, then the host's standard
free-port range. A requested port that cannot be assigned returns
`409 game_port_in_use` or `port_state_unknown`. An exhausted automatic search
returns `409 port_unresolved`. The host retains the registry tombstone after
purge so the ID stays reserved.

The default `keepBackups` is true, including on expiry. The host retains
an internal local repository outside the removed tree under the server ID.
The existing restore-from-backup flow discovers these repositories. Pass
false to delete backups stored in the managed tree. External or remote
repositories stay separately managed.

The `removed-server-retention` capability adds owner-only
`removedServerRetentionDays` to GET and PATCH `/v1/host`. PATCH accepts an
integer from 1 to 365 and returns the effective days with the revision.
Invalid values return `400 invalid_request`. The default is seven days.
Changes apply to future removals and each existing `restoreUntil` is preserved.
A background worker processes expiry and queued purges, including after restart.

Poll the server and removed lists after changes or while deletion is pending.
Host realtime still returns `501 realtime_multiplexer_deferred`.
