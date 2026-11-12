# Ordered resource-pack builder

Status: backend builder implemented in `vesta_control::resource_packs`. Public
hosting, management routes, persisted drafts/jobs and Minecraft configuration
activation are separate integration work. No launcher UI or live game files are
changed by this module.

## Priority and special resources

`build` takes pinned open files ordered **lowest priority first, highest last**.
Reordering inputs changes precedence. There is no implicit sorting by provider,
filename or resource name. A UI showing highest priority at the top must reverse
its displayed order before calling this API.

Ordinary path collisions select the highest pack. They do not block a build or
require manual conflict resolution. `BuildReport.resolved` records the path,
ordered contributing IDs and resolution type; the last contributing ID wins an
ordinary collision. `pack.png` follows this same priority.

Minecraft loads some resources as a stack. The builder preserves these rules:

| Resource | Rule |
| --- | --- |
| Locale JSON | Merge keys; the higher pack wins repeated keys. |
| `sounds.json` | Merge event maps; append sound lists unless an incoming event sets `replace=true`. Appending retains the existing subtitle. Replacement resets sounds and subtitle, including clearing an omitted subtitle. Preserve any replacement flag needed to override Minecraft's base pack. |
| Atlas JSON | Concatenate sources from low to high. Retain source order and duplicates, including filters that act on earlier sources. |
| Font JSON | Put higher-pack provider arrays first, retaining each array's order and lower-pack fallback providers. |

These semantics were checked against Mojang's official Java 1.21.4 client bytecode
and official mappings using the installed JDK's `javap` as a data inspector. The
client JAR was not executed. This is a reference implementation check, not a live
Minecraft client test or certification for every past/future version. See the
[source research](../reviews/2026-10-27-resource-pack-hosting-research.md).

Root metadata is generated for the exact supplied pack format, including modern
minor-format bounds. Inputs must declare compatible ranges. Root language
declarations merge by key. Non-asset files retain provenance under
`vesta/attribution/<input-id>/...`; `vesta/stack.json` records source digests, order
and resolutions. This retains supplied notices without asserting redistribution
rights.

Active pack-level `filter` and conditional `overlays` metadata are explicitly
unsupported in this first builder. Flattening those features requires preserving
pack boundaries or evaluating version-selected layers. Ordinary overlapping assets
are supported; these metadata features are not silently stripped. Legacy `.lang`
resources and other unclassified formats currently use whole-file priority, so
callers must validate their supported Minecraft range separately.

## API and authority

1. Resolve/download/upload inputs through trusted server adapters. `Input::new`
   accepts an already-open regular file, a stable safe ID and the expected SHA-256.
   No caller URL, extraction directory or arbitrary output path is accepted.
2. Call blocking `build(state, inputs, target_format, description, limits)` on an
   owned background worker. It verifies input digests before/after the build and
   returns an immutable `BuiltPack`. Dropping it removes its private stage.
3. After checking publication authority and expected draft revision, `publish`
   verifies the retained stage again and publishes a hash-addressed ZIP using the
   existing protected `ArtifactDirectory` no-replace operations. Identical bytes
   are idempotent; differing existing bytes refuse and remain untouched.
4. A `PublishedPack` supplies name, size, SHA-256, Minecraft's SHA-1 and report.
   `open_verified` validates bytes before handing an owned file to a serving adapter.
   Publication here creates an immutable private artifact only. It does not expose
   a URL, change `server.properties` or switch an active release.

The trusted caller still owns principal/device authorization, durable idempotency,
revision checks, active-release transactions, origin configuration and retention.
Do not call this builder from an urgent-control executor or infer authority from a
successful build. Cache verified file handles appropriately in a future download
service instead of periodically hashing artifacts in the background.

## Budgets and archive admission

Default service budgets can be lowered but not increased through `BuildLimits`:

- 16 inputs, 8,192 total input entries and at most 8,192 output entries.
- 256 MiB total compressed input, 512 MiB expanded input, 64 MiB per member.
- 2 MiB per parsed/generated structured JSON and 64 KiB per input root metadata.
- 128 MiB output and 120 seconds of cooperative work between local IO operations.
- One active build per process; there is no automatic retry, queue or idle worker.

These are Vesta budgets, not Minecraft download-size guarantees. A hosting adapter
must enforce the chosen client's size/format compatibility and cross-agent host
budgets before exposing this feature. Local filesystem IO cannot be preempted by
the cooperative deadline.

Media streams through a 32 KiB copy buffer. Only bounded per-path structured JSON
is materialized. Entry tables, expanded budgets and final output remain bounded
even for lower-priority files hidden by an override. ZIP central-directory counts
and raw duplicate names are checked before the ZIP library indexes members.

The builder rejects unsafe/nonportable paths using the shared catalogue validator,
duplicate members, encryption, symlinks/special files, corrupt CRCs, unsupported ZIP
compression, multi-volume/ZIP64 directories, incompatible metadata and exhausted
budgets. Nothing is extracted into the game root. ZIP64 is unnecessary within this
builder's output budget and is deliberately excluded.

Output members are sorted, with fixed timestamps, Unix modes and compression
settings. Publication reuses the existing artifact naming and ownership checks.
Failed builds retire only their own stage; existing artifacts remain unchanged.
Crash stages count toward the shared artifact-directory limit; cleanup/retention
must never delete a referenced release.

## Next integration

Persist the ordered draft and actor-bound build/review jobs. Expose exact revision
and input-digest binding to Opus's UI. Add a direct unauthenticated HTTPS artifact
route behind an explicitly configured trusted public origin, then atomically bind
its URL/hash/UUID to Minecraft's settings using existing guarded writers. The
management connection's private certificate pin is not ordinary client TLS trust.
Keep this independent from the Vesta client modpack and its minimum revisions.
