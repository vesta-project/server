# World management

Routes under `/api/v3/manage`, canonical session authorization, normal JSON envelopes:

- GET `/world`: `{name,seed:string|null,configured_seed:string,exists:boolean,revision:string}`. Config view and scoped file reads required. The actual current seed is read from modern/legacy `level.dat` NBT, never inferred from an empty generation setting. Signed 64-bit seeds are strings.
- POST `/world/new`: `{seed:string,expected_revision:string,confirm_world:string}` with the normal `Idempotency-Key` header. Owner, config.edit and protected property-file write authority required. Response `{world:<same status>,preserved_worlds:string[],outcome:"succeeded"}`. Current name confirmation and exact revision required; malformed/linked/unknown paths refuse.

Creating a new world selects a fresh unique `level-name` and the requested generation seed in one recoverable atomic `server.properties` replacement. Existing world/dimension directories remain at their original paths. It does not edit an existing world's seed, delete, rename or copy its chunks. Empty seed selects Minecraft's random generation. The new world is created by the next separately requested Start. Preserved world locations are returned to the caller.

The service holds the existing start gate and supervisor stopped-game maintenance hold across the file effect, including proof checks inside completion. Unknown, starting, stopping, draining, orphan or unresolved processes refuse. The ordinary file journal protects previous configuration and handles ambiguous receipts; retries never select a second world. No new background worker, scan, copy, listener, poller or credential is introduced. World metadata decompression and NBT work are bounded.

Scope is the world selected by ordinary `server.properties` under the configured game root. Custom launch arguments/plugins that override world selection require a separate integration; the panel must not claim their metadata is covered.
