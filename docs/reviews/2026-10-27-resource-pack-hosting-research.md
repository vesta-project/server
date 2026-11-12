# Minecraft Java resource-pack hosting research

Date: 2026-10-27
Scope: Vesta resource-pack hosting and the Java launcher; separate from the client modpack. No implementation performed.

Implementation update: after this research the user approved automatic hierarchy
resolution. The earlier recommendation to block every overlapping asset is
superseded by the [implemented builder contract](../operations/resource-pack-builder.md):
ordinary files use higher-pack priority, while verified language/sound/font/atlas
formats retain their stack semantics. Active pack filters and conditional overlays
remain explicitly unsupported. The historical conservative recommendations below
describe the research stage, not the current implementation policy.

## Findings

### Server configuration and compatibility

Paper documents the vanilla `server.properties` resource-pack settings: `resource-pack` is the ZIP URL; `resource-pack-sha1` is the pack file's SHA-1 in hex, recommended for checking the downloaded bytes; `require-resource-pack=true` kicks players who reject/fail the required pack; `resource-pack-prompt` supplies prompt text; and `resource-pack-id` is a UUID identifying the pack. The prompt field was added in snapshot 21w15a, required-pack in 20w49a, and ID in 1.20.3 pre-release 1. Keep generated configuration conditional on server/client protocol generation rather than emitting newer keys blindly to old versions. ([Paper server properties](https://docs.papermc.io/paper/reference/server-properties/), [Minecraft 1.20.3 pre-release notes](https://www.minecraft.net/en-us/article/minecraft-1-20-3-pre-release-1))

The vanilla server configuration describes one URL/pack. Java clients gained a protocol/API for a batch of multiple packs, stack-versus-replace behavior, required status and per-pack UUIDs in 1.20.3; Paper's Adventure guide documents sending/removing these packs and callbacks. This is a Paper/plugin/API path, not a multi-URL list in `server.properties`; it is appropriate only when Vesta deliberately owns/installs a plugin or integrates the server API. Older versions need a single merged ZIP. ([Paper Adventure resource packs](https://docs.papermc.io/adventure/resource-pack/))

`pack.mcmeta` is version sensitive. Mojang's release notes document overlays from 1.20.2-era snapshots: selected overlays add/replace files for a supported pack format but cannot remove files; their `pack.mcmeta` and `pack.png` are ignored. Mojang's 1.21.9 notes update metadata to minor-version ranges (`min_format`/`max_format`); old `pack_format`/`supported_formats` remain required for compatibility with older pack formats. Generate/check metadata against an explicit Minecraft version target and do not flatten overlays without evaluating which overlay applies. ([Mojang 23w31a notes](https://www.minecraft.net/en-us/article/minecraft-snapshot-23w31a), [Mojang 1.21.9 notes](https://www.minecraft.net/en-us/article/minecraft-java-edition-1-21-9))

Mojang's 1.19 notes document `filter.block`: a later pack can filter matching files from packs loaded before it. It applies to preceding packs, not its own assets. During merge, silently dropping filters changes behavior; either preserve/translate their intent under a documented supported policy or report a conflict requiring review. ([Mojang 1.19 notes](https://www.minecraft.net/en-us/article/minecraft-java-edition-1-19))

### Merging packs

A merged server ZIP is one ordinary resource pack rooted at `pack.mcmeta` and its `assets/` tree (not a ZIP containing several nested ZIPs). A resource-pack stack is not universally equivalent to a last-file-wins ZIP overlay: official 1.21.4 client bytecode confirms semantic processing for languages, sounds, fonts, and atlases; exact rules are in the implementation appendix. Mojang pack filters and overlays also depend on pack boundaries/version selection.

For simple binary assets such as PNG/OGG, ordinary path precedence can apply, but replacing a file can still break model/definition references. Use the content-aware rules in the implementation appendix for structured assets; reject unsupported structures rather than treating all JSON as generic last-file-wins.

`pack.mcmeta` is global metadata, not an asset to take arbitrarily from the last input: select a target version and explicitly synthesize/validate it. `pack.png` is likewise singular. Preserve provenance and notices where permitted. V1 may combine supported content-aware files using the rules in the implementation appendix; fail on unsupported conflicting structures, filters/overlays, incompatible formats or unsafe entries. Do not claim semantic stack equivalence for paths without a version-tested merge rule.



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







## Implementation appendix: stack semantics and conservative merge rules

In addition to official notes/docs, the project downloaded the official Mojang 1.21.4 client JAR and official client mappings as data only (artifact identity checked against Mojang version metadata). `javap -c -p` was used to inspect bytecode; the game was not launched and the JAR was not executed. Mappings resolve the requested client classes as `SoundManager=hjw`, `Preparations=hjw$a`, `FontManager=frk`, `ClientLanguage=hfz`, `SpriteSourceList=hez`, `ResourceManager=aup`, `MultiPackResourceManager=aui`, and `NamespaceResourceManager=aug`. Raw `javap` output is saved under `V:\tmp\resource-pack-hierarchy-20261027\javap\`.

| Resource / metadata | Verified stack behavior | Merger rule / limitation |
| --- | --- | --- |
| `assets/<namespace>/lang/<locale>.json` | In official 1.21.4 bytecode, `ClientLanguage.loadFrom` iterates each locale/namespace resource stack and calls `appendFrom` for every resource; its parser callback is `Map.put`. The ordered stack is processed bottom-to-top, so later/higher duplicate translation keys win. This matches Forge docs saying language files are content-merged. ([Forge docs 1.21.x](https://docs.minecraftforge.net/en/1.21.x/concepts/resources/)) | Merge JSON objects per locale with low-to-high resource order and higher-pack key overwrite. Reject malformed/non-object JSON and duplicate JSON keys within a single file; preserve ordinary key/value types as strings.
| `assets/<namespace>/sounds.json` | Official 1.21.4 `SoundManager.prepare` loads every `sounds.json` resource in stack order and calls `SoundManager.Preparations.handleRegistration` per event. If no prior event exists, or incoming registration has `replace=true`, it creates/replaces `WeighedSoundEvents` using the incoming subtitle; this discards prior sound entries and resets subtitle (including to null if omitted). Otherwise it keeps the existing event/subtitle and appends each incoming sound entry. Thus non-replacing higher registrations preserve the lowest earlier subtitle; a later replacing registration resets both sounds and subtitle, after which later non-replacing registrations append sounds without changing that subtitle. ([Forge sounds schema](https://docs.minecraftforge.net/en/1.21.x/gameeffects/sounds/), [Forge replace field docs](https://docs.minecraftforge.net/en/1.20.1/datagen/client/sounds/)) | Merge namespace event maps low-to-high. For each event: absent = create; `replace=true` = replace prior record/sounds/subtitle; otherwise append sound definitions and retain existing subtitle. Preserve sound list order. Reject duplicate keys within one JSON, invalid types, and unknown schema fields unless preserved losslessly. Confirm same behavior on every supported Minecraft range.
| `assets/<namespace>/font/<font>.json` | Official 1.21.4 `FontManager.loadResourceStack` parses every resource definition for a font ID and registers provider entries keyed by font ID, source pack and array index; same-path definitions aggregate, rather than whole-file winning. Font provider results are built through the stack, then the manager reverses the provider list before `FontSet` loads/selects it. With resource-stack resources ordered low-to-high, effective glyph-provider precedence is high-to-low; original provider-array order within each pack is retained after the reversals. ([Mojang 23w17a notes](https://www.minecraft.net/en-us/article/minecraft-snapshot-23w17a)) | Merge provider arrays in low-to-high resource-pack order, preserving each array order. This yields the client's high-priority-first glyph lookup while retaining lower providers as fallback. Keep versioned conditions/schema intact; reject malformed/unknown structure unless losslessly preserved, and add precedence fixtures for supported versions.
| `assets/<namespace>/atlases/<atlas>.json` | Mojang 1.19.3 notes explicitly say multiple packs' atlas files merge by pack order. Official 1.21.4 `SpriteSourceList.load` obtains all resources for an atlas ID, parses each `sources` list, and appends sources in resource-stack order. Each source runs sequentially; filter sources affect sprites already added. Mojang 1.21.11 warns about same-name sprites across atlases. ([Mojang 1.19.3 notes](https://www.minecraft.net/en-us/article/minecraft-java-edition-1-19-3), [Mojang 1.21.11 notes](https://www.minecraft.net/en-us/article/minecraft-java-edition-1-21-11)) | Concatenate `sources` arrays low-to-high without sorting/deduplicating. Preserve list order and filters; warn on duplicate sprite IDs. Apply only to supported schema versions.
| `pack.mcmeta` `filter` | Mojang 1.19 notes: filter block patterns are applied to files in packs loaded before the pack containing the filter; the filter does not apply to its own pack. ([Mojang 1.19 notes](https://www.minecraft.net/en-us/article/minecraft-java-edition-1-19)) | Flattening inputs into one pack removes the original earlier/later-pack boundary, so retaining filters in output can change or lose their effect. Reject any input with a `filter` section until Vesta implements and tests an order-aware transformation. |
| `pack.mcmeta` `overlays` | Mojang 23w31a says overlays are format-selected subpacks, can add/replace but not remove files, and are applied in reverse declaration lookup order (last overlay checked first, then earlier overlays, then base). 1.21.9 changed format metadata fields for newer pack formats while retaining old fields for older formats. ([Mojang 23w31a notes](https://www.minecraft.net/en-us/article/minecraft-snapshot-23w31a), [Mojang 1.21.9 notes](https://www.minecraft.net/en-us/article/minecraft-java-edition-1-21-9)) | Do not discard or blindly union input overlays. They are client-version selected and layered against each input base; flattening multiple packs requires preserving order, version ranges and winner behavior. Reject inputs with overlays for initial combiner unless a target-versioned overlay transformer is implemented and validated. |

Practical support: one-pack passthrough first; then implement tested object merge for language files, `sounds.json` event semantics (including subtitle retention/reset), font provider accumulation, and atlas source concatenation. Reject malformed/unknown JSON structures that cannot be losslessly preserved, and reject all input pack-level filters/overlays until there is an order-aware transformation. Keep these version-scoped: test merged output against the same inputs loaded as a pack stack for every supported client range.


