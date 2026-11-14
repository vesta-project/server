# Host server creation v1

Owner-only HTTPS API under `/v1/host/`, using the same host bearer session and current owner/device/generation checks as import. Other roles receive 403 `forbidden`. Wizard order: New, Modpack, Restore, Import. Paths always refer to the Vesta Server machine. This is the implementation contract; availability of individual routes must be checked against the implementation status below.

## Catalogue and defaults

* GET `/v1/host/catalogue/minecraft` -> 200 `{versions:[{id,stable,experimental,recommended}],latestStable}`. Versions newest first; latest stable is the sole recommended version.
* GET `/v1/host/catalogue/software?minecraftVersion=<id>` -> 200 `{minecraftVersion,kinds:[{kind,label,stable,experimental,recommended,available,builds:[{id,build,loaderVersion,stable,experimental,recommended}]}]}`. `build` is integer|null, `loaderVersion` string|null; `id` is an opaque selection id, not a URL. Kinds: `vanilla`, `paper`, `folia`, `purpur`, `fabric`, `quilt`, `forge`, `neoforge`, `bukkit` (CraftBukkit), `spigot`. Include unavailable kinds with empty builds. Vanilla is the default recommended kind. One newest stable build per available kind is recommended; if none is stable no experimental build is silently selected.
* GET `/v1/host/creates/defaults` -> 200 `{name,minecraftVersion,software:{kind:"vanilla",build:null,loaderVersion:null},gamePort,memoryMiB,java:"automatic"}`. Name is the first unused `Minecraft Server`, `Minecraft Server 2`, etc. Port is the next unreserved/free TCP port from 25565. Memory is suggested from machine RAM minus OS reserve and all server reservations. These are suggestions, rechecked at apply. No server need exist for any of these calls.

## Plan, review, apply, poll, start

POST `/v1/host/creates/plan` accepts one of:

```json
{"kind":"new","name":"Survival","minecraftVersion":"1.21.1","software":{"kind":"vanilla","build":null,"loaderVersion":null},"gamePort":25565,"memoryMiB":4096,"eulaAccepted":false}
```

```json
{"kind":"modpack","name":"Adventure","source":{"kind":"provider","provider":"modrinth","projectId":"project","versionId":"version"},"gamePort":25566,"memoryMiB":4096,"eulaAccepted":false}
```

Modpack source may instead be `{kind:"upload",uploadId}`. Provider is `modrinth|curseforge`; immutable versionId is required. Minecraft/loader come from the reviewed pack metadata, never a launcher guess. Reuse server install planning from `packs/workspace/install*.rs`; expose all its review issues. Client-provided download URLs and executable paths are refused.

Restore request is `{kind:"restore",name,source:{kind:"snapshot",sourceId},gamePort,memoryMiB,eulaAccepted:false}` or source `{kind:"upload",uploadId}`. GET `/v1/host/creates/restore-sources` -> `{sources:[{sourceId,label,createdAt,minecraftVersion,software:{kind,version},bytes}]}` lists only recoverable snapshots/recovery exports not attached to a live server. Opaque ids are revalidated during apply. Uploaded recovery exports return 501 `not_supported_yet` until that adapter exists.

POST `/v1/host/creates/uploads` accepts raw `application/octet-stream` server pack archive (header `X-Upload-Kind: modpack|restore`) -> 201 `{uploadId,expiresAt}`. Uploads are host-private, owner-bound, expire in 24h and limited to 2 GiB compressed/16 GiB extracted/100000 files; refuse traversal, links and special files. Uploaded restore currently returns 501 `not_supported_yet`.

Plan returns 202 `{jobId,state:"planning"}`. Poll GET `/v1/host/creates/jobs/{jobId}`. The envelope fields and state semantics are shared with import: `{jobId,state,phase,error,progress:{filesDone,filesTotal,bytesDone,bytesTotal},review,serverId}`. States: `planning|review|applying|ready|starting|online|blocked|failed|cancelled|interrupted`. Create phases: `resolving|downloading_java|downloading_software|installing|registering|starting`. Byte counts refer to current phase, zero total means not yet known. Error is null or a fixed code. `online` requires observed Minecraft readiness.

Review is null while planning, otherwise `{planId,confirmation,expiresAt,summary,blockers:[],warnings:[]}`. `confirmation` null when blocked or outside review. `expiresAt` RFC3339 UTC, TTL 10 minutes. `summary` is `{kind,name,minecraftVersion,software:{kind,version,build,loaderVersion},resources:{gamePort,memoryMiB},runtime:{requiredMajor,downloadRequired,automatic:true},destination:{serverId,displayPath,freeBytes},pack:null|{name,provider,projectId,versionId,minecraftVersion,loader,issues:[{code,message,blocking}]},restore:null|{sourceId,label,createdAt}}`. Blockers/warnings are string codes; pack issues are also retained verbatim from server modpack planning. Changes require a fresh plan. EULA is not accepted by planning.

POST `/v1/host/creates/apply` with `Idempotency-Key` and `{planId,confirmation,acknowledgedWarnings:[code],eulaAccepted:true}` -> 202 `{jobId,serverId}`. Atomically revalidate owner, plan, resources, reviewed artifacts and reserve server id/name/port/memory. Repeated key+body returns the same reply; different body is 409 `idempotency_conflict`. Java is always selected automatically by compatibility and downloaded from verified managed sources when needed. Never use Java or scripts from a pack. Install under `<machine servers>/<serverId>/game`, record EULA, freeze trusted launch, register and create a worker. Explicit start is separate.

POST `/v1/host/creates/jobs/{jobId}/cancel` (empty body) -> 200 `{state}`. Cancellation is cooperative at safe artifact boundaries; keep source and committed server intact, release uncommitted reservations.
POST `/v1/host/creates/jobs/{jobId}/resume` (empty body) -> 202 envelope, only for interrupted jobs. Revalidate reviewed content and owner; changed/expired plan requires replanning. Durable jobs and checkpoints survive restart, with in-flight uncertain jobs marked interrupted. Never duplicate downloads/publication or spawn a second game process.
POST `/v1/host/creates/jobs/{jobId}/start` (empty body) -> 202 envelope. Trusted authorized operation start; poll until online. Failed start retains installed files and returns ready with `start_failed` or `game_port_in_use`. Workers for every registered server are recreated after host restart; owners see each server immediately after registration.

## Review codes and suggested text

| Code | Kind | Suggested text |
|---|---|---|
| name_in_use | blocker | Choose another name; a server on this Vesta Server already uses it. |
| game_port_in_use | blocker | This game port is busy. Choose another port. |
| port_unresolved | blocker | Vesta Server could not find a free game port. |
| port_state_unknown | blocker | Vesta Server could not check this game port. Try again. |
| memory_budget_exceeded | blocker | This needs more memory than this Vesta Server has available. |
| memory_unresolved | blocker | Vesta Server could not check available memory. Try again. |
| memory_invalid | blocker | Choose at least 512 MiB of memory. |
| insufficient_space | blocker | Vesta Server needs more free disk space for this server. |
| software_unresolved | blocker | Choose a supported Minecraft version and server software build. |
| java_unresolved | blocker | Vesta Server could not find a compatible Java download. Try again. |
| provider_unavailable | blocker | Vesta Server could not reach the download provider. Try again. |
| pack_invalid | blocker | This pack does not contain a supported Minecraft server. |
| pack_client_only | blocker | Choose a server pack; this pack only supports Minecraft clients. |
| pack_review_blocked | blocker | Resolve the issues shown in the pack review, then try again. |
| restore_source_unavailable | blocker | This recovery source is no longer available on Vesta Server. |
| unsafe_archive | blocker | This archive contains unsafe paths or unsupported files. |
| source_too_large | blocker | This archive exceeds the Vesta Server size limit. |
| experimental_software | warning | This version is experimental and may be unstable. |
| legacy_checksum | warning | This download uses an older checksum format. Review it before continuing. |
| pack_review_warning | warning | Review the pack notices before creating this server. |
| restore_version_unknown | warning | Vesta Server could not identify this backup's Minecraft version. |

Transport errors use import's `{error:{code,requestId}}`: 400 `invalid_request|idempotency_key_required|eula_required|warnings_unacknowledged`; 404 `create_job_not_found`; 409 `conflict|plan_blocked|plan_expired|confirmation_invalid|idempotency_conflict|name_in_use|game_port_in_use|memory_budget_exceeded`; 413 `request_too_large`; 501 `not_supported_yet`; 503 `creates_unavailable|create_jobs_saturated|worker_unavailable|unavailable`. Download failures surface fixed job errors `download_failed|integrity_conflict|install_failed|java_unresolved|start_failed`. Suggested text: "Vesta Server could not finish this step. Try again." Cancellation uses `cancelled`; interruption uses `interrupted`.

## Server cards and import corrections

GET `/v1/host/servers` entries gain `software:{kind,version}|null`, `minecraftVersion:string|null`, `iconEtag:string|null`. Null means unknown/no icon. Exact authenticated image URL: `/v1/servers/{serverId}/manage/server/icon` (PNG); ETag is the existing quoted SHA256 branding icon etag.

Import inspection and review gain `nearby:[{folderToken,path,looksLikeServer:true}]` for immediate readable child server folders, allowing "Did you mean survival?". An unreadable source yields only `source_unreadable`; a readable non-server yields only `not_minecraft_server`, plus found/missing/nearby. Do not derive Java/space/resource blockers without valid source facts. Never report insufficient_space for unknown required size or when free >= required. Re-read saved import roots per request so `vesta import-root add` takes effect without restarting.

## Implementation status

Contract committed ahead of implementation for launcher coordination. Uploaded-export restore is explicitly deferred with 501 as specified above.

Implemented now (kind `new` end to end): catalogue, defaults, `POST /v1/host/creates/plan` with `kind:"new"`, apply, job status, cancel, resume, start, and the server card fields and icon route below. `GET /v1/host` lists the capability `create-jobs` when the host can run them. Not built yet, answering 501 `not_supported_yet` so the launcher hides those modes: plan with `kind:"modpack"` or `kind:"restore"`, `GET /v1/host/creates/restore-sources`, `POST /v1/host/creates/uploads`.

## Changes since first draft

All additive; no field or code above changed meaning.

* Catalogue kinds gain `unavailableReason: null|"provider_unavailable"|"unsupported_version"`. `provider_unavailable` means the provider could not be reached or answered badly (try again); `unsupported_version` means it answered and has no server for this Minecraft version. `available` is false in both cases. Builds of every kind are listed newest first (the installer sorts each provider explicitly), so the first stable build is the recommended one.
* Plan request: `gamePort` and `memoryMiB` may be `null` to take the defaults; `eulaAccepted` may be omitted and planning never accepts the EULA. Unknown fields are refused with 400 `invalid_request`, as are a build or loader for Vanilla and unknown software kinds.
* Planning checks name, port and memory against every registered server and every unreleased reservation (imports and creates share one name/port/memory namespace), reporting `name_in_use`, `game_port_in_use`, `port_unresolved`, `port_state_unknown`, `memory_budget_exceeded`, `memory_invalid` or `memory_unresolved` as blockers. Apply re-checks them in the reserving transaction, so of two plans for the same port only the first apply succeeds; the other gets 409 `game_port_in_use` (likewise `name_in_use`, `memory_budget_exceeded`).
* A Minecraft version the catalogue does not offer, or a build/loader the provider does not resolve to exactly, blocks with `software_unresolved`; a provider outage blocks with `provider_unavailable`. Snapshots add the warning `experimental_software`.
* New warning code `unsupported_upstream`: suggested text "Its developers no longer support this version. Choose a newer version if you can." Vanilla from Mojang's official manifest does not carry `legacy_checksum`; Vesta Server accepts Mojang's SHA-1 automatically (only for artifacts served from Mojang's own objects host, and records that in the job and host audit). Any other provider or source that offers only legacy checksums still carries `legacy_checksum` for the owner to acknowledge.
* `summary.software.version` is the software's own version: the loader version for loaders, else the build number as a string, else (Vanilla) the Minecraft version; null while unresolved.
* The reviewed artifacts are bound to the plan by digest. Apply downloads Java (when needed) and installs from fresh provider evidence; if what the provider now offers differs from the review, the job fails with `integrity_conflict` and the owner plans again.
* Phases: `resolving` (also while planning), `downloading_java`, `downloading_software`, `installing`, then `registering` (a `ready` job stays in `registering`), and `starting` once started. Progress counts bytes of the current phase only and resets to zero at each phase change; `filesDone`/`filesTotal` stay zero for creates.
* Job failures while applying are state `failed` with error `download_failed|integrity_conflict|install_failed|java_unresolved|insufficient_space`; nothing is registered, the destination is removed and the reservation released, so the owner simply plans again. A job interrupted while planning fails with `replan_required` on resume.
* Cancel during `resolving|downloading_java|downloading_software|installing` returns 200 `{state:"applying"}` and the job settles `cancelled` at the next safe boundary (destination removed, reservation released). From `registering` on the server is committed and cancel answers 409 `conflict`.
* Resume of an interrupted install restarts it from a clean, never-registered destination (artifacts re-verified against the review); interrupted while `registering` it finishes registration; interrupted while `starting` it returns to `ready` without spawning.
* Status and actions for an unknown create job answer 404 `create_job_not_found`; an apply naming an unknown `planId` answers the same. Import job ids are unknown to the create routes and the other way round. A host without an installer answers 503 `creates_unavailable`.
* Server cards: for created servers `software:{kind,version}` uses the summary's kind and version; for imported servers `kind` is the detected loader (`vanilla|paper|purpur|fabric|forge|neoforge|quilt`) with `version:null`, or `software:null` when undetected. `minecraftVersion` is the reviewed version. `iconEtag` is the branding icon ETag, matching the `ETag` of `GET /v1/servers/{serverId}/manage/server/icon` (404 `resource_not_found` while there is no icon).
* Vanilla checksums are confirmed with Mojang itself. Vanilla metadata is read through a mirror, so before accepting Mojang's SHA-1 automatically the host reads Mojang's own `piston-meta.mojang.com/mc/game/version_manifest_v2.json` (cached briefly like the other catalogues), follows the selected version's Mojang version document (verified by the SHA-1 the manifest gives for it) and requires every SHA-1-only Mojang object it will install (`downloads.server`, and `downloads.server_mappings` where a loader uses it) to have exactly that `url` and `sha1`. The same check covers the Vanilla jar inside Fabric, Quilt and Forge style installs. If Mojang's data differs, or Mojang does not list the version, planning blocks with the new blocker `integrity_conflict` (suggested text: "The download does not match what Mojang publishes. Try again later."); if Mojang cannot be reached or answers badly it blocks with `provider_unavailable`. It is never accepted silently. The reviewed artifacts are bound to the plan by digest, so apply installs exactly the verified objects.
* CraftBukkit and Spigot are compiled on the machine by BuildTools, which needs `git` and a POSIX shell available to the Vesta Server service (Git for Windows provides both on Windows). When they are missing, the catalogue lists `bukkit` and `spigot` with `available:false` and the new `unavailableReason: "requires_git"`, and planning either kind blocks with the new blocker `requires_git` (suggested text: "Install Git on the Vesta Server machine to use CraftBukkit or Spigot, then try again."). It never fails late with `install_failed`. The Java Development Kit BuildTools also needs is managed by Vesta Server: these kinds plan a compatible installed JDK, or download Adoptium's checksum-verified JDK of the required major (`runtime.downloadRequired` is then true), never the plain runtime.
* Imported server cards now fill `software.version` from the facts recorded at import, with the same meaning as for created servers: Vanilla gives the Minecraft version, Paper/Purpur the build from the launch jar (`paper-<mc>-<build>.jar`), Fabric the loader from its launch jar, Forge/NeoForge the loader from the launch argfile's library folder or jar. It stays `null` only when those facts do not say (for example Quilt, or a jar whose name does not carry the version).
