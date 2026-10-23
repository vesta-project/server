# Whitelist desired membership during startup

Implemented 2026-10-23. The launcher shows the desired whitelist immediately after acceptance, without a pending badge. Minecraft's file is not rewritten while its process is starting.

## Lifecycle and cancellation

- With Minecraft fully stopped, the existing guarded offline editor remains the file-write path.
- With a running generation (including its initial startup), the launcher submits desired membership to `POST /api/v3/manage/players/whitelist/startup`. Ready-server whitelist edits use this same path so an inverse edit cannot accidentally bypass an outstanding startup change.
- Requests include `player`, `add`, `expected_generation` and the existing device-scoped idempotency header. Current `players.moderate` authority is required. The agent resolves an addition's UUID before acceptance using the existing bounded online/offline resolver. A live generation can briefly predate published readiness: recording a desire is safe then; dispatch still requires matching-generation readiness.
- Opposite unsubmitted changes coalesce. Adding an absent player and removing it before dispatch cancels the addition and leaves no command to send. Likewise removing an existing player then adding it back cancels the pending removal.
- Once the durable dispatch boundary has been crossed, later edits remain ordered compensating desires. They do not erase an accepted or potentially accepted operation. Older effects cannot be dispatched after a newer desire for the same player, including after a lost receipt or rotating observation window.
- Replaying a request returns its original receipt/status and never resurrects a superseded addition. Current authority is checked on retrieval and before dispatch. A stopped/replaced generation, authority loss, degraded startup or changed identity mode fails remaining applicable work; it is not replayed into a replacement process.

The ordinary whitelist read endpoint projects current-generation desires over fresh persisted membership, so the existing list and controls need no pending visual state. Malformed/ambiguous baseline identities refuse a new mutation. Failure removes the projection, records a terminal receipt and audit, and raises a warning. The launcher retires cached list reads and receives a coalesced list-change event; it then renders the file-backed state and displays a patient error notification for failed/unknown receipts.

## Persistence verification

Dispatch uses ordinary typed, generation-bound moderation operations once readiness is `Ready`. A durable dispatch marker precedes operation admission; a stable internal operation key recovers a lost admission receipt without a duplicate command.

`stdin_written` remains delivery evidence. After that operation succeeds, the coordinator rereads `whitelist.json` until the expected UUID membership is present/absent. Removal additionally checks that the target name is absent. File reads are shared within a coordinator tick. A mismatch or unreadable file is retried for at most twenty seconds after successful stdin delivery is first observed, then reported as unverified failure. The command is not automatically resent on a read-back mismatch.

This proves the persisted result, not arbitrary plugin admission behavior or that a player can join. The [real vanilla 26.3 probe](whitelist-startup-observations.md) supports the file-save behavior and demonstrates why startup writes are unsafe. It also found offline-mode command/profile casing differences. If the command persists a different UUID, verification fails rather than falsely claiming the expected identity was added. No broad `whitelist reload` or plugin reload is introduced.

## Bounds and recovery

The queue is a typed `whitelist-runtime` state row, at most 32 desires / 64 KiB, including in-flight reconciliation. Existing management file receipts provide device-scoped replay and normal bounded retention. The existing host maintenance loop processes up to four entries per tick, rotates fairly across entries, preserves per-player order, and does no SQLite/file work for this feature when the queue is empty. It adds no agent thread and reserves no lifecycle lock while waiting for readiness or file verification; Stop remains independent.

`GET` on the startup endpoint returns recent receipts for the current actor/device. `?ids=<up to 32 comma-separated UUIDs>` retrieves exact receipts, including older IDs outside a recent page. Unknown receipts stay unknown. The launcher uses one bounded observer per connection, batches up to 32 IDs, backs off unchanged observations, and preserves the original generation/intent on uncertain retries. It shares the existing 32-tracker native budget; saturation leaves accepted work durable on the agent and resumable rather than turning its receipt into a refusal.

Normal same-instance agent recovery can resume matching-generation work. Portable recovery exports exclude the runtime row and management operation receipts, and imports reject forged runtime rows: importing a backup cannot revive queued whitelist mutations. No schema migration or always-on game proxy was added.
