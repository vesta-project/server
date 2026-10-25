# Dimension-local world metadata

The development world's level.dat has a split seed. Its world-generation file
is stored under `world/dimensions/minecraft/overworld/data/minecraft/` rather than
the previously assumed `world/data/minecraft/`. Absence at the old path made the
entire world metadata request fail despite a valid saved world.

The reader now checks the known overworld locations, preferring dimension-local
metadata when present. Existing path authority, size bounds and NBT validation
apply to both. Missing files permit fallback; corruption or denied access does
not. It never borrows seed metadata from another dimension. The selected relative
path and bytes are included in the world revision.

Dimension discovery also recognises `dimensions/minecraft/overworld`,
`the_nether`, and `the_end`, alongside existing DIM-1/DIM1 and sibling
world_nether/world_the_end layouts. This is filesystem-layout detection; no
software identity or plugin-specific branch is added. Custom external world
containers and arbitrary plugin-managed worlds remain outside the managed root.

Six world integration fixtures pass, including precision/revision changes,
dimension discovery, malformed preferred-file refusal and no cross-dimension
seed fallback. An explicitly selected read-only test also validates the live
world's saved NBT without displaying private seed data or modifying any files.
