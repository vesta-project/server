# Datapack selection

Running Minecraft uses `POST /api/v3/manage/world/datapacks/toggle` with its current generation and the existing console operation path. Minecraft handles reload and pack compatibility.

Stopped Minecraft uses `POST /api/v3/manage/world/datapacks/offline`:

```json
{"pack_id":"file/example.zip","enabled":true,"expected_revision":"<world revision>"}
```

The request requires the management session, `config.edit`, `config.view`, the applicable file permissions and an idempotency key. Success returns `{"outcome":"succeeded"}`. Read `/world` for the new selection and revision. Reusing the same key and payload returns the saved receipt without applying the edit again.

Only ZIP and folder packs currently observed in the world's datapacks folder can be changed offline. Built-in packs remain visible as state labels. Vesta does not alter world feature flags or attempt to enable built-in experiments through this path. It does not delete, rename or move datapack files.

The offline edit acquires the existing stopped-game start exclusion, including a supervised hold when applicable. It verifies the world revision, pins server.properties while writing, and compares the level.dat hash immediately before the recoverable replacement. Starting, running, stopping and uncertain process ownership refuse the edit. The file journal preserves the previous level.dat and governs interruption recovery.

Only `Data.DataPacks.Enabled` and `Disabled` are changed. All other decompressed NBT bytes are preserved, including seeds, feature flags, world state and plugin metadata. Existing enabled-pack order is preserved; newly enabled packs are appended. Java modified UTF-8 handles non-ASCII pack names. Existing size, depth, list and parser-work bounds remain enforced. Missing or malformed selection lists refuse the edit.

Validation uses generated world fixtures, with byte-preservation, Unicode, no-op, replay, revision, authority and running-process refusal checks. It does not certify a real Minecraft installation's reload behavior or a particular pack's compatibility.
