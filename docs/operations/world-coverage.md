# Dated world and server-version coverage

2026-11-10 Australia/Sydney. Provider release metadata, Vesta filesystem behavior
and actual Minecraft world loading are three separate kinds of evidence.

The bounded catalog run retained 243 public metadata responses (6,512,861 bytes)
and 1,030 rows: 103 released Mojang game IDs across ten software identities.
All 103 downloaded Mojang version documents matched their manifest-advertised
metadata SHA-1. Server artifact metadata was present for 97; six historical
documents had no server download entry. No server binary was downloaded by this
inventory. Published artifact SHA-1/MD5 values remain legacy provider evidence,
not new Vesta integrity guarantees or downloaded-artifact verification.

Forge/NeoForge catalogs yielded 71/14 stable loader identifiers, without fetched
artifacts. Paper/Folia/Purpur exact build metadata resolved 20/4/24 game versions;
the remaining rows distinguish not listed, no stable candidate, unqueried, and
request-budget exhaustion. Bukkit/Spigot generated outputs remain unqueried.
Fabric 26.3's selected loader server metadata matched the required profile shape,
but its client profile was not queried. Quilt's selected server metadata matched
the main class/libraries while companion info omitted the decoder's required
`hashed` member; this compatibility gap needs a provider-contract investigation.
Do not remove hashed/artifact binding merely to accept a different response.

These are dated metadata results, not permanent latest-version constants or
universal supported/unsupported verdicts. The retained audit inventory includes
exact URLs, retrieval times, metadata hashes, provider channels, artifact IDs,
per-row evidence and explicit unknowns. No more requests are scheduled by it.

## World filesystem boundaries

| Layout | Recognized examples | What was exercised |
|---|---|---|
| Legacy/current Anvil vanilla | world root, `DIM-1`, `DIM1`, playerdata/stats/advancements | Saved metadata read and fresh-world selection preserves original witnesses |
| Bukkit split dimensions | `<name>_nether/DIM-1`, `<name>_the_end/DIM1` | Preserved roots returned; chunks/entities/POI/datapacks remain byte-identical |
| Vanilla 26.1 layout | `dimensions/minecraft/*`, `players/*`, split worldgen metadata | Overworld seed selection plus region/player/custom-dimension/config witnesses preserved |

The new fixture uses a non-default world name containing a space, extreme signed
seed, custom mod/config data, and same-intent replay. Only `server.properties`
selects a fresh world; existing files are neither migrated nor renamed. The
fixture ran on Windows x86_64 and local WSL Linux x86_64. Synthetic witness bytes
are deliberately not real chunk/load certification.

Vesta's declared host targets are Windows x86_64, Linux x86_64 and Linux ARM64.
Linux ARM64 execution was not available here. Java bytecode portability does not
certify Java/runtime/native-profiler or provider-specific host support. macOS and
Windows ARM64 are not silently added to the product support matrix.

Pre-Anvil conversion, automatic world downgrade, cross-loader migration,
Paper/vanilla conversion, custom universe roots and arbitrary plugin-selected
worlds are not implemented by these checks. Real sanitized world fixtures and
explicit game boot/migration acceptance remain future work. See
[world management](world-management.md) for the actual API contract.
