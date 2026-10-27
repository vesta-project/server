# Minecraft Java resource-pack hosting research

Date: 2026-10-27
Scope: Vesta resource-pack hosting and the Java launcher; separate from the client modpack. No implementation performed.

## Findings

### Server configuration and compatibility

Paper documents the vanilla `server.properties` resource-pack settings: `resource-pack` is the ZIP URL; `resource-pack-sha1` is the pack file's SHA-1 in hex, recommended for checking the downloaded bytes; `require-resource-pack=true` kicks players who reject/fail the required pack; `resource-pack-prompt` supplies prompt text; and `resource-pack-id` is a UUID identifying the pack. The prompt field was added in snapshot 21w15a, required-pack in 20w49a, and ID in 1.20.3 pre-release 1. Keep generated configuration conditional on server/client protocol generation rather than emitting newer keys blindly to old versions. ([Paper server properties](https://docs.papermc.io/paper/reference/server-properties/), [Minecraft 1.20.3 pre-release notes](https://www.minecraft.net/en-us/article/minecraft-1-20-3-pre-release-1))

The vanilla server configuration describes one URL/pack. Java clients gained a protocol/API for a batch of multiple packs, stack-versus-replace behavior, required status and per-pack UUIDs in 1.20.3; Paper's Adventure guide documents sending/removing these packs and callbacks. This is a Paper/plugin/API path, not a multi-URL list in `server.properties`; it is appropriate only when Vesta deliberately owns/installs a plugin or integrates the server API. Older versions need a single merged ZIP. ([Paper Adventure resource packs](https://docs.papermc.io/adventure/resource-pack/))

`pack.mcmeta` is version sensitive. Mojang's release notes document overlays from 1.20.2-era snapshots: selected overlays add/replace files for a supported pack format but cannot remove files; their `pack.mcmeta` and `pack.png` are ignored. Mojang's 1.21.9 notes update metadata to minor-version ranges (`min_format`/`max_format`); old `pack_format`/`supported_formats` remain required for compatibility with older pack formats. Generate/check metadata against an explicit Minecraft version target and do not flatten overlays without evaluating which overlay applies. ([Mojang 23w31a notes](https://www.minecraft.net/en-us/article/minecraft-snapshot-23w31a), [Mojang 1.21.9 notes](https://www.minecraft.net/en-us/article/minecraft-java-edition-1-21-9))

Mojang's 1.19 notes document `filter.block`: a later pack can filter matching files from packs loaded before it. It applies to preceding packs, not its own assets. During merge, silently dropping filters changes behavior; either preserve/translate their intent under a documented supported policy or report a conflict requiring review. ([Mojang 1.19 notes](https://www.minecraft.net/en-us/article/minecraft-java-edition-1-19))

### Merging packs

A merged server ZIP is one ordinary resource pack rooted at `pack.mcmeta` and its `assets/` tree (not a ZIP containing several nested ZIPs). A resource-pack stack is not universally equivalent to a last-file-wins ZIP overlay: Minecraft has loaders that process particular resource types semantically across packs. A Mojang-mapped client class reference identifies `SoundEventRegistration` as carrying a per-event `replace` flag, and mapped `SoundManager` has registration handling; this is evidence that `assets/<namespace>/sounds.json` has event-level semantics rather than being a safe opaque whole-file winner. The mapped/decompiled source documentation reports that non-replacing higher-pack event entries augment lower entries, while replacement changes that behavior. This is implementation evidence rather than a Mojang-published API contract, so V1 should be conservative. ([Mojang-mapped SoundEventRegistration reference](https://mc-packet-reference.netlify.app/26.x/source/net/minecraft/client/resources/sounds/soundeventregistration/), [SoundManager mappings](https://mappings.dev/1.21.11/net/minecraft/client/sounds/SoundManager.html), [mapped/decompiled sound loader explanation](https://minecraftdocs.dev/systems/client/what-makes-a-sound)) Forge's official developer documentation for 1.21.x explicitly notes that localization files are merged contentwise (and tags in the data resource system are merged); this is corroborating ecosystem documentation, not a vanilla/Mojang contract for every version. ([Forge resource documentation](https://docs.minecraftforge.net/en/1.21.x/concepts/resources/))

For simple file resources such as PNG/OGG assets, a ZIP can retain the chosen file at a colliding path, but that only reproduces path selection, not semantic relationships (for example a model can reference a texture or sound file that another pack replaces). Font provider and atlas source definitions are JSON documents with internal arrays; the examined documentation does not establish a cross-pack list-merge rule for those files, so do not claim they merge across packs. A full path replacement can discard definitions from the lower pack. Mojang's documented overlays are a distinct in-pack mechanism and do not remove files. ([Mojang 23w31a notes](https://www.minecraft.net/en-us/article/minecraft-snapshot-23w31a))

`pack.mcmeta` is global metadata, not an asset to take arbitrarily from the last input: select a target version, synthesize compatible metadata/description and retain compatible overlays/filter semantics where possible. `pack.png` is likewise singular. Non-asset files may include license/readme material; preserve provenance and notices where permitted. A conservative V1 should reject duplicate normalized asset paths (including structured JSON such as `sounds.json`, fonts, atlases, models, and language files) rather than claim semantic stack equivalence or silently apply last-file-wins. Permit straightforward combination only for inputs whose asset paths do not overlap; handle only root metadata/icon through explicit synthesis/selection. A future version may add format-aware merge rules behind target-versioned validators and fixture comparisons to actual pack-stack behavior. This is a deliberate conservative compatibility limit, not a Minecraft prohibition.



Inspect ZIP entries defensively before extraction/build: reject path traversal/absolute paths, symlinks or unsupported entry types, duplicate normalized paths, malformed metadata, excessive entry count, expanded-size bombs and oversized output. Enforce compressed and expanded byte limits, build to a staged file, validate it, calculate digests on final bytes, and atomically publish. This is an implementation security recommendation, not a quoted Minecraft requirement. Treat user-submitted packs as untrusted data; server-side ZIP parsing should not execute pack content, and clients still parse/render downloaded assets.

### Hosting and integrity

Vanilla expects a direct HTTP(S) ZIP download URL; use public, unauthenticated HTTPS with a correct content type, no interactive webpage/login, stable availability, and sufficient transfer capacity. Minecraft's hash is SHA-1 of the complete ZIP bytes (not extracted contents); it is a legacy protocol field, not a modern security signature. Compute it only after final ZIP creation and publish URL/hash/UUID as one release record. Prefer immutable content-addressed URLs and never replace bytes at a URL while keeping its hash/ID; publish new bytes at a new URL and update configuration together. A mismatched/missing hash can impair cache/integrity behavior. Keep SHA-256 internally for Vesta artifact identity/security even though Minecraft configuration requires SHA-1. ([Paper server properties](https://docs.papermc.io/paper/reference/server-properties/))

### Bukkit/Paper and worlds

The built-in server-wide URL applies regardless of the active world. Bukkit/Paper plugins can send player-level pack requests, and modern protocol supports stacking multiple server-provided packs; this permits plugin/world-aware policy but means ownership and ordering can conflict with Vesta's global pack. Paper docs warn pack responses come from clients and may be absent/incorrect; do not treat an apply callback as a security proof. Per-world behavior should be an explicit plugin integration with precedence/ownership rules, not implied by vanilla `server.properties`. Paper's documented world layout places `datapacks/` at the world root alongside `dimensions/`, whose subdirectories hold overworld, nether, and end data; this is one datapack-enabled world with multiple dimensions, not a requirement to copy the same datapack into each dimension. Before Paper 26.1, Paper used separate world folders for nether/end storage, but these are still dimensions of the configured world; follow the server version's supported world layout. ([Paper 26.1 world storage notes](https://papermc.io/news/26-1/), [Paper migration layout reference](https://docs.papermc.io/paper/migration/))

A custom `--universe` / `--world-container` (`--world-dir`) changes where world folders are stored, while `level-name` names the main world folder. Those are server adoption/path-resolution concerns, not changes to resource-pack delivery semantics. The Vesta transport path currently resolves the configured `level-name` main world; custom universe/world-container adoption remains unsupported and needs a separate explicit path-resolution feature before it is in scope. ([Paper CLI arguments](https://docs.papermc.io/paper/reference/cli-arguments/))

### Trust and licensing

A pack ZIP is untrusted client-facing content. Vesta should label the publisher/source, preview inputs/collisions, preserve licenses/attribution where source terms require it, and not assume that public download or client-side use grants redistribution rights. This is product guidance, not legal advice; require an operator to confirm rights before Vesta rehosts merged inputs. Avoid claiming signature/author authenticity from SHA-1 or UUID: they identify/check bytes, not publisher. A raw server resource-pack URL provides no Vesta-level access control or entitlement.

## Vesta codebase observations (narrow inspection)

`docs/SERVER_SOFTWARE_REQUIREMENTS.md` already models the client modpack as an independent workspace, with immutable provider references, an overrides workspace, explicit builds, and atomic artifact publication. It explicitly distinguishes those artifacts from installed server mods. Resource packs should be a separate domain/workspace and must not be synthesized from the client modpack or by scanning/copying the live server root.

The codebase has reusable concepts, not a ready-made public pack web host: artifact storage, SHA-256 verification, staged/atomic publication, bounded HTTPS download adapters, and file authorization are present. Narrow search found no resource-pack hosting route/service. Existing authenticated Vesta control/file APIs are not a substitute for Minecraft clients' unauthenticated direct ZIP fetch. Verify deployment topology before choosing a static origin; a dedicated static route/CDN/object store may be needed.

## Implementation proposal

1. Add one-pack passthrough and hosting first: explicitly select one supplied ZIP, validate/record it, publish it as an immutable artifact, and expose its URL/hash/UUID. Keep this workspace distinct from both `mods/` and the Vesta client modpack workspace.
2. Add a bounded combiner as a separate phase. Inputs are explicit uploads or authorized references; record source identity, attribution/license, target Minecraft version, order and collisions. Conservative V1 accepts only non-overlapping normalized asset paths; reject overlaps instead of claiming last-file-wins equals a pack stack. Handle root metadata/icon explicitly and reject unsupported filters/overlays, incompatible formats and unsafe entries. Preview inputs, collisions, size, target compatibility and metadata.
3. Use bounded, defensive ZIP parsing; persist build manifest and SHA-256; stage output, validate pack root/metadata/size, then atomically publish immutable artifact bytes. Never publish a partial archive.
4. Host a direct HTTPS URL accessible without Vesta login. This public origin is distinct from management authentication and any Vesta TLS pinning. Publish an immutable URL/content-addressed key and record Minecraft SHA-1, Vesta SHA-256, UUID, target version, size and revision together. Keep prior URLs valid during transition; update `server.properties` consistently. Configure `resource-pack`, `resource-pack-sha1`, `resource-pack-id`, and optional required/prompt settings, subject to server-version support.
5. Start with the single merged ZIP path for broad server compatibility. Add a Paper plugin/multi-pack path later only if there is a concrete need for runtime stacking, per-player/world behavior, or independent removal; define precedence against other plugins and version fallback. Do not make launcher-side modpack installation a prerequisite for the server pack.
6. Expose separate resource-pack management permissions, visibility, storage limits and lifecycle. Show users that the pack is downloadable by any joining client and that rejected required packs cause disconnects. For a first release, use owner/admin-generated releases and simple optional/required controls, with careful client-visible notice and an audited setting change.

## Sources

- PaperMC, [server.properties reference](https://docs.papermc.io/paper/reference/server-properties/)
- PaperMC, [Adventure resource-pack API guide](https://docs.papermc.io/adventure/resource-pack/)
- Mojang, [Java 1.20.3 pre-release 1](https://www.minecraft.net/en-us/article/minecraft-1-20-3-pre-release-1)
- Mojang, [Java 1.19 release notes: pack filters](https://www.minecraft.net/en-us/article/minecraft-java-edition-1-19)
- Mojang, [Java snapshot 23w31a: overlays](https://www.minecraft.net/en-us/article/minecraft-snapshot-23w31a)
- Mojang, [Java 1.21.9 release notes: pack metadata formats](https://www.minecraft.net/en-us/article/minecraft-java-edition-1-21-9)
- PaperMC, [configuration reference](https://docs.papermc.io/paper/reference/configuration/)






