# Client pack workspace

The client pack is independent of live server resources. Its protected SQLite
draft contains presentation, the server's Minecraft version, a client loader and
pinned entries. Paper, Purpur and vanilla servers can publish a Fabric client
pack; a vanilla resource-pack-only draft uses loader `none`. No workspace action
copies files into the game directory, restarts Minecraft or applies suggestions.

Workspace tables and uploaded bytes are excluded from credential-free recovery
exports, like hosted artifacts and release receipts. Preserve them through
database-aware protected-state backups. Server game-file backups alone do not
constitute a client-pack revision archive.

## Draft and authority

`GET /api/v3/manage/pack/draft` returns `draft:{revision,manifest}`,
`server_minecraft_version`, `minecraft_drift`, `published_revision`,
`minimum_revision`, `pending_publication` and `safety_snapshot_recommended`.
An untouched workspace has revision `"0"`. Revisions and byte counts are decimal
strings on HTTP/IPC. Draft revisions and published revisions are different clocks.

Read/edit/import/upload requires `pack.edit` (owner and administrator defaults).
Build preview also requires `pack.build`; publication also requires `pack.publish`;
raising minimum requires `pack.force_update`. Server inventory suggestions and
publication's mismatch report additionally use the existing inventory/world-read
file/config scopes. Every edit, receipt replay and publication rechecks authority.

The manifest has `name`, `summary`, nullable `icon` (an uploaded image SHA-256),
numeric/dotted `display_version`, `minecraft_version`, `loader`, nullable
`loader_version`, and `entries`. Loaders are `none|fabric|quilt|neoforge|forge`.
The managed server installation supplies Minecraft version, with the configured
pack version as fallback when that installation has no known version. Edits must
use that version. A subsequent server upgrade leaves the old draft unchanged and
sets drift; explicitly update the draft before publishing it.

Each entry has `id`, `kind`, portable game-relative `path`, `required`,
`environment:{client,server}`, and `source`. Kinds are `mod|resource_pack|shader|
datapack|config_override`; side values are `required|optional|unsupported`.
Sources are `{type:"provider",provider,project_id,version_id}`,
`{type:"upload",sha256}` or an imported `{type:"manual",reason}` blocker.
Provider requests contain no download URL or digest. Authoritative provider
metadata supplies compatibility, filename, download identity and dependencies.
An entry unsupported on the client is excluded from the artifact. `required`
controls required/optional client installation for included entries.

CurseForge resolution requires the agent's trusted `VESTA_CURSEFORGE_API_KEY`,
independently of the launcher's provider configuration. Missing metadata/download
authority is reported; a caller URL is never used as a fallback.

Resource paths live under `mods/`, `resourcepacks/`, `shaderpacks/` or `datapacks/`.
Overrides support `config/`, `defaultconfigs/`, `kubejs/`, `scripts/`, `resources/`,
and Minecraft's `options.txt`, `optionsof.txt`, `optionsshaders.txt`, `servers.dat`.
Absolute, parent, linked and nonportable paths and colliding file sets refuse.
The workspace holds at most 256 entries, 16 MiB per embedded file and 64 MiB of
retained embedded bytes. Icons are PNG/JPEG, at most 256 KiB;
`GET pack/draft/icon` returns nullable `{mime_type,content_base64}`.

`POST pack/draft {expected_revision,manifest}` creates/replaces the whole draft.
`POST pack/draft/add {expected_revision,entries}` atomically upserts entries by ID.
`POST pack/draft/delete {expected_revision}` clears entries and client loader,
retaining presentation and advancing the draft revision. Reusing an intent with
the exact original body returns its original result even after later edits;
different bodies conflict. Success audit, draft update and receipt share one
transaction. Refused edit transactions produce refusal audits.

Browse can instead post `pack/draft/add` with
`{expected_revision,provider,project_id,version_id,required,dependencies}`.
Dependencies are selected `{provider,project_id,version_id,required}` records.
The agent resolves these IDs to authoritative filenames/kinds/environments,
then commits one draft edit. It does not select additional dependencies here.

## Upload and import

All writes carry `Idempotency-Key`. For small files `POST pack/draft/upload
{content_base64}` returns their SHA-256. Normal clients use resumable chunks:

1. `POST pack/draft/uploads/begin {upload_id,total_size,sha256,archive}`.
2. `POST pack/draft/uploads/chunk {upload_id,offset,content_base64}`; decoded
   chunks are at most 32 KiB, fitting the default 64 KiB HTTP request limit.
3. `POST pack/draft/uploads/finish {upload_id}` verifies exact length/digest.
4. For a file, reference the resulting SHA-256 from an entry or `icon`. For an
   archive, call import with its upload ID. `POST pack/draft/uploads/cancel
   {upload_id}` discards that device's unused transfer.

Keep the same intent and upload ID across retries, including chunk offsets.
Uploads are actor/device-bound, at most eight retained transfers / 128 MiB of
reserved bytes, with archives bounded to 68 MiB. File finish moves bytes into
the hash store and retires its transfer; successful archive import consumes its
transfer. Cancel abandoned transfers. Receipts are bounded to 10,000 and are not
silently evicted; retained file blobs are not automatically garbage-collected.

`POST pack/draft/import` accepts:

- `{source:"uploaded_archive",expected_revision,upload_id}`;
- `{source:"archive",expected_revision,content_base64}` for a small archive;
- `{source:"published",expected_revision,expected_published_revision}`.

Import replaces the draft, never the active release. It accepts Modrinth
`.mrpack`, CurseForge pack ZIPs, and Vesta archives; current catalogue references
are resolved through their pinned provider identity. ZIP member count, individual
and expanded sizes, portable paths, symlinks and collisions are checked without
extracting files. Client overrides take precedence over shared overrides. Unknown
or unavailable Modrinth file identities become visible manual-download entries;
imported download URLs never become download authority. CurseForge file IDs stay
pinned, including files requiring a manual download. Unsupported override paths
refuse rather than silently drop content. Platform archive downloads retain the
shared provider downloader's 16 MiB verified-download cap.

## Suggestions, preview and publication

`GET pack/suggestions` is an explicit provider/inventory read and returns
`draft_revision`, `items`, `warnings`, `potentially_outdated`. Kinds are
`client_mod_missing`, `newer_compatible_version`, `server_mod_removed`,
`minecraft_version_drift`. Provider client-side **required** metadata establishes
a missing client requirement; a Fabric wildcard environment alone does not.
Unknown metadata is not an inferred requirement. Removal suggestions apply to
both-side draft mods; truncated inventory suppresses removals. Provider failures
are warnings. The response never changes the draft or its revision.

The launcher's `ResourceManager::resolve_dependency_preview` is the same traversal
used for instance installs. Native preview automatically includes required
dependencies, keeps optional choices explicit, reports resolver failures and
retains pinned conflicts. These additions/promotions are preview-only.

`POST pack/preview {draft_revision,resolved_entries}` revalidates the complete
proposed client file set against provider Minecraft/loader/kind/side metadata,
required and incompatible dependency edges and download availability. Shader
engine requirements agree with the instance resolver. Uploaded binaries have an
unverified-compatibility warning; manual/unavailable provider downloads block
publication. A lower display version produces `display_version_lower`; revision
is always the update authority. The response includes issues, complete manifest,
diff, server mismatch suggestions, `valid`, `review_sha256`, and snapshot advice.
The diff includes additions, removals, changed entries/version pins, metadata and
custom overrides. Symbolic links refuse admission and therefore never appear in
an accepted file set.

Publish extends the existing route with
`POST pack/publish {expected_revision,draft_revision,resolved_entries,
review_sha256,required_update,acknowledge_lower_version}`. Here
`expected_revision` names the current **published** revision. Use exactly the
preview's entries/digest. Native exposes this as `packBuildPublish`; the existing
configured-source publish and `pack/minimum` remain compatible.

Publication checks authority, expected release, current draft and server software
revision, validates again, downloads pinned provider bytes with allowlisted CDN
redirects and verified hashes, builds privately, then uses the existing artifact
store/release transaction. Release row, complete published workspace manifest,
audit and idempotency receipt activate together. A failed build, stale review,
concurrent draft edit, software switch or failed transaction keeps the previous
release active. The Vesta index preserves summary, embedded icon, Minecraft/client
loader and per-file environments. The new published revision is always monotonic.
Optional updates retain minimum; required updates advance it atomically to the new
revision. The first publication always requires its first revision.

Recommend a configured safety snapshot before destructive draft replacement or
publication (`files-and-content-008`); the response explicitly carries this advice.
No snapshot is taken implicitly and no live server file set is updated. Stage 3
signed consumption is documented below; linked-instance ownership, rollback and
executable consent are in the launcher's `features/servers/PACK-SYNC.md`.

## Errors and verification

## Stage 3 consumption transport

Paired clients use `GET /api/v3/manage/pack/sync?installed=N&nonce=HEX32`.
The response data contains `payload_base64`, `certificate_base64` and
`signature_base64`. The exact decoded payload is signed after the byte prefix
`vesta-pack-sync-v1\0` (the final byte is NUL), using the active TLS private key.
The included leaf DER must hash to the client's already approved TLS fingerprint.
The signed JSON binds server ID, fingerprint, installed revision, fresh nonce,
latest/minimum revisions, status, artifact format/size/SHA-256. All counters are
decimal strings. Clients verify the signature, all bindings and monotonicity
before trusting a statement; discovering a certificate is never trust approval.

`GET /api/v3/manage/pack/artifact?revision=N&sha256=HEX64` streams only the
currently published artifact and refuses a stale selection with 412. Both routes
recheck ordinary pack-download policy, including automatic roles and revocation,
and use the existing bounded HTTP/download admission and integrity-checked body.
Statements are generated from the committed release, so minimum-only policy
changes are signed too. A TLS rotation requires explicit client trust approval;
no independent download key is silently substituted. The nonce prevents replay
of an old signed minimum. Offline status cannot establish current join policy.

Revision/review conflicts retain 412 `precondition_failed`; intent reuse with a
different body returns 409 `idempotency_conflict`. MC drift uses 409
`pack_release_revision_mismatch`; lower-display-version refusal uses 409
`pack_release_version_not_increasing`; unresolved downloads use 409
`pack_unresolved_file`; invalid manifests use 400; caps use 413 `pack_too_large`.

Regression coverage is in `packs/workspace/{tests,import,publication,suggestions}`
and transport `tests/pack_workspace/mod.rs`. It covers durable revisions/receipts,
revocation, import confinement, dependency completion, optional/required releases,
artifact environments/presentation and authenticated TLS publication. No live
provider downloads or production publication are used by these tests.

### Validation on 2026-10-26

- Latest workspace regression selection: 12 passed, including verified provider
  bytes, concurrent draft edits, image/summary preservation and force-update
  authority on replay. Authenticated TLS workspace lifecycle/capability tests pass.
- Full serial control unit run: 1,026 passed, two ignored and one old-v8 fixture
  failure. The fixture omitted removal of the new tables when simulating v8; it
  was corrected and its targeted rerun passed. Control integration suites: 28
  passed. No production migration was weakened to accommodate fixture downgrades.
- State suite: 114 passed, including migration/export classifications. Protocol:
  85 passed. Transport: 47 unit plus 120 HTTPS tests passed.
- All-target clippy with `-D warnings` passed for `vesta-protocol`, `vesta-state`,
  `vesta-control`, `vesta-transport` and the launcher native-client crate.
- Launcher native-client suites: 112 passed, seven environment/live tests ignored.
  `cargo check -p vesta-launcher` and desktop typecheck passed; launcher check
  retains existing unrelated warnings.
- The expanded Bun server/localization selection ran 331 tests: 330 passed and
  the pre-existing console `HOT_SCOPE_TIME` 8 ms budget assertion failed, including
  an idle rerun. That console test passes in isolation. All six new pack checks
  and localization checks pass. No diagnostic threshold was changed or suppressed.
