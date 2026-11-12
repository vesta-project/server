# One world: details and datapacks (D1)

`GET /api/v3/manage/world` retains name, seed (decimal string), configured_seed,
exists and revision, and adds:

- size_bytes (decimal bytes), size_complete, size_observed_at_ms: a 30 s cache per
  server/world/caller, bounded to 20,000 entries with a 250 ms budget checked between directory reads. An incomplete
  scan is explicitly partial; never present it as the total. Read failures also
  mark the size incomplete. Private/ignored/linked paths are excluded.
- last_modified_at_ms: latest observed file/folder modification within that scan;
  last_played_at_ms: saved `Data.LastPlayed` from level.dat, nullable.
- dimensions: overworld/nether/end with present, layout and game-relative folders.
  Vanilla nested DIM-1/DIM1 and Paper-style world_nether/DIM-1 and
  world_the_end/DIM1 are parts of the same world; split folders count toward size.
- datapacks: id/name/kind/path/enabled. Folder and zip entries come from disk;
  builtin and missing entries come from saved DataPacks.Enabled/Disabled lists.
  A new pack absent from both lists has enabled=null, not an invented false.
  datapack_state_source is `saved_level_dat` and can lag a running game.

`POST /world/datapacks/toggle {pack_id,enabled,expected_generation}` requires
config.edit and console.write and a real running generation. It admits the exact,
JSON-quoted Minecraft `/datapack enable|disable` command through the durable
console-operation path with the normal Idempotency-Key. A stopped or stale game
refuses (`datapack_requires_running_game`); start it before toggling. Minecraft
owns validation/reload and writing level.dat. Operation success proves command
submission, not that a particular pack loaded; check console output and refresh
after Minecraft saves. The agent never rewrites compressed NBT offline.

The agent's `/world/new` remains for other tools. The launcher client method,
Tauri command, TS/fixture method and obsolete creation action are removed.
`server_world_datapack_toggle` and TS `worldDatapackToggle` replace that scope;
ServerWorld/WorldDimension/WorldDatapack expose details and folder layouts.

UI follow-up: present one world, hide/reveal-copy the seed, display nullable
metadata and partial-size state, group dimension folders by dimension, and show
folder/zip datapacks with explicit unknown enabled state. Require a running
server to toggle and follow the returned operation/console result.
