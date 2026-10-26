# Server content operations

Server resources are the files Minecraft loads, independently of the published
client pack. Changing resources never edits or publishes a client pack. Dependency
selection belongs to the launcher's shared resolver; the agent accepts pinned
provider/project/version identifiers, never client download URLs or hashes.

## Inventory

`GET /api/v3/manage/content` lists mods, Bukkit plugins and the active world's
datapacks, with `kind`, game-relative `path`, `filename`, decimal-byte `size`, nullable
`enabled`, `sha256`, `provider`, `declared`, and `pending`, `pending_id`,
`pending_error`. Provider references contain `provider`, `project_id`, `version_id`.
Identity is attached only when the current bytes match recorded installation
provenance. Unrecognized jars remain visible. Missing/complex/corrupt JAR metadata
does not hide the file. Fabric, Quilt, Forge/NeoForge and Bukkit/Paper declarations
are inspected without loading classes or executing the JAR.

Datapacks reuse world details and their saved `level.dat` enabled/disabled selection.
Folders get a bounded tree digest and byte count; ZIPs get a file digest. Built-in
and missing datapacks remain available through the existing world API, rather than
being reported as installed files. File/tree hashing is bounded; a null validator
means that the observation did not hash the complete content.

Inventory has no provider network IO. `POST content/identify {path}` optionally
identifies one existing mod/plugin JAR (up to 16 MiB) through Modrinth's
[`version_files` hash API](https://docs.modrinth.com/api/operations/versionsfromhashes/).
The request has a five-second timeout and a 1 MiB metadata cap. Positive and negative
SHA-512 results are cached in protected state (1,024 records). Network failures are
not cached as negative identity. Existing CurseForge provenance is retained;
unidentified files are not sent to CurseForge fingerprint search.

## Changes and restart behavior

All POSTs require the management session and `Idempotency-Key`.

| Route | Body | Effect |
|---|---|---|
| `content/install` | `{kind,provider,project_id,version_id}` | Provider-verified install |
| `content/update` | Same plus `{path,expected_sha256}` | Replace the given installed filename, preserving disabled suffix |
| `content/mutate` | `{action:remove\|enable\|disable,kind,path,expected_sha256}` | Trash or toggle a JAR; remove a datapack ZIP/folder |
| `content/cancel` | `{id}` | Cancel an unstarted pending change |
| `content/identify` | `{path}` | Optional cached provider identification |

`kind` is `mod`, `plugin` or `datapack`. Mod installs require Fabric, Quilt, Forge
or NeoForge; plugin installs require Paper, Spigot, Bukkit, Purpur or Folia.
Incompatible kinds return `content_mods_unsupported` or
`content_plugins_unsupported` (409), before provider work. Unidentified/custom
software has no inferred family; the client pack loader is never used as evidence.
The managed launch version supplies the target Minecraft version when available.
Destination directories must exist; create them through the regular file API if
necessary. Provider downloads retain the existing 16 MiB write cap, allowlisted
CDNs, redirect restrictions, deadline and authoritative digest verification.

Stopped games apply immediately under the shared stopped-process hold. Running,
draining or uncertain games retain the desired operation and verified bytes in
protected SQLite state. Inventory shows `pending: install|remove|update|enable|disable`.
Nothing is restarted automatically. Before the next agent-driven start/restart,
the sole process owner applies pending changes in order. A protected durable marker
prevents the separate supervisor from launching autonomously while changes remain.
Its scheduled crash restart stays pending until explicit start/restart reconciles
the queue; agent disconnection does not bypass the barrier.

The queue is limited to 128 pending entries / 128 MiB of staged bytes, and 10,000
retained idempotency records. Application rechecks the original principal/device's
current authority and original file validator. Failed/conflicting/revoked changes
stay visible with a fixed `pending_error` and block start until resolved or cancelled.
An interrupted successful file mutation reuses its existing file-journal receipt.
Cancellation never reverses a started or uncertain filesystem effect. The original
device or a current owner may cancel; this lets an owner retire a revoked actor's
staged work. Stage, application and cancellation are audited.

JAR enable/disable uses `.jar` / `.jar.disabled`. Datapacks must instead use the
existing `world/datapacks/offline` (stopped saved selection edit) or
`world/datapacks/toggle` (generation-bound Minecraft command) routes. Attempting a
datapack filename toggle returns `content_datapack_toggle_requires_world`.
Inventory exposes separate offline/running datapack toggle capabilities. A running
toggle's saved state can lag until Minecraft saves `level.dat`.

Regular file removal/update uses existing recoverable file trash. A folder datapack
removal uses the existing journaled, fully scoped directory rename into
`content-trash/<opaque-id>`; `trash_path` identifies the retained tree. It never
recursively deletes files. Its snapshot is bounded to 4,000 entries / 64 MiB.
An authorized SFTP directory rename can restore that retained folder to its original
world path. No automatic expiry/purge applies to these retained directory trees.

## Capabilities

Listing requires file list/read scopes and the config-view scopes used by world
details. Mutations reuse `content.install` plus create/overwrite/delete/rename
authority on exact canonical paths. Directory trash additionally requires mkdir
for `content-trash` and rename authority on every source/destination descendant.
The returned capability projection is advisory; each mutation reauthorizes.
Datapack selection reuses config edit, console write for running commands, and
the existing stopped world-edit file scopes. No new role grants are introduced.

## Custom server JAR

An owner prepares `POST software/custom-jar/plan {path,expected_revision}`. The
path must name a regular, nonlinked `.jar` inside the game root; JAR size is bounded
to 256 MiB, central-directory entries to 4,096, and manifest bytes to 64 KiB.
The manifest must declare a valid `Main-Class`. The plan reports that class, risks,
expiry, revision, plan ID and confirmation token. Preparation does not activate it.

`POST software/custom-jar/apply {plan_id,confirmation_token,idempotency_key}`
confirms the exact actor/device, bytes and revision, and admits an audited software
job. The game must be stopped for activation. Existing launch journaling, recovery
blocking and selected Java/JVM flags apply. Status includes
`current.custom_jar: {path}` and no managed family is claimed.

`POST software/restore-managed/plan {expected_revision}` followed by the same apply
route restores the protected managed launch saved before the first custom
selection. Repeated custom selections do not lose that saved launch. Restoration
retains the current Java selection. Standard managed software installation remains
available through the existing software plan/apply flow.
