# Host folders and import v1

Only current primary owner/owner can call these HTTPS routes. Server Admin file access is confined to managed server storage and cannot reach them. No browser-local file picker: paths refer to the host, e.g. the Linux machine at 192.168.1.67. Host response paths are informational; opaque tokens identify admitted directories. No unrestricted file-read API is introduced.

## Browse and candidates

| Method/path | Request -> response |
|---|---|
| GET `/v1/host/folders/roots` | `{roots:[{rootId,label,displayPath,readable,kind:"filesystem"|"import_root"}],revision}`. Always includes the filesystem root (`/` or each drive root on Windows) plus saved import roots. Saved roots are labelled by their folder name (never "Import folder 1"); `displayPath` is the full path. |
| POST `/v1/host/folders/resolve` | `{rootId,relativePath}` -> `{folderToken,displayPath,expiresAt}`; relative path uses slash segments, no `..`, absolute path, NUL or traversal. |
| POST `/v1/host/folders/check` | `{path:"<absolute>"}` -> `{path,status:"ok"|"permission_needed"|"not_found"|"not_directory"|"protected",folderToken:null|string,remediationCommand:null|string}`. `ok` mints a token usable like a browsed folder. When a parent is unreadable, prefer `permission_needed` with `remediationCommand` for the typed path (`sudo vesta import-root add '<absolute path>'`). |
| POST `/v1/host/folders/list` | `{folderToken,cursor:null|string}` -> `{displayPath,entries:[{name,kind:"directory",folderToken,readable,looksLikeServer,remediationCommand}],nextCursor}`. Every visible sub-directory is listed with `readable:true|false`; unreadable entries cannot be entered and carry `remediationCommand`. `looksLikeServer` is a cheap name check (`server.properties` or a recognisable server jar/loader marker). Never listed or enterable: Vesta Server's own data/install directories and pseudo/system trees (`/proc`, `/sys`, `/dev`, `/run`, `/boot`, `/lost+found`, and the like). Directories only, 100/page; no file contents. |
| GET `/v1/host/imports/candidates` | `{candidates:[{candidateId,kind:"amp"|"minecraft",source,label,displayPath,folderToken,readable,remediationCommand,confidence,evidence}],warnings}`. Only folders that look like servers and are readable, or that exist but need permission (with `remediationCommand`). No warnings for absent or bare unreadable default locations (a bare unreadable `/home` is not a warning). Labels are the folder's own name; `kind`/`source` are machine codes, not for display. Max 100 entries. |
| POST `/v1/host/imports/plan` | Request below -> 202 `{jobId,state:"planning"}`. |
| GET `/v1/host/imports/jobs/{jobId}` | Durable job and review below. |
| POST `/v1/host/imports/apply` | `{planId,confirmation,acknowledgedWarnings:[code],eulaAccepted:true,sourceStoppedConfirmed:true}` -> 202 `{jobId,serverId}`; Idempotency-Key required. |
| POST `/v1/host/imports/jobs/{jobId}/cancel` | -> `{state}`; cancels at safe copy boundary, does not delete original or stop unrelated processes. |

Folder token random 32 bytes, expires 10min, bound to host/account/device/root and open directory identity; it is not a bearer authorization replacement. Revalidate current owner and root membership every use. Reject links/reparse points, special files, traversal and overlap in either direction with host private state, any managed server or destination. Ancestors of protected paths may be browsed but not imported. Use descriptor-relative/no-follow access through scan and copy; canonicalize-once/string-prefix checks are insufficient. Roots are also registered by privileged local `vesta import-root add|list|remove`; `remove` revokes the ACL that `add` granted. Remote owners cannot expose host secrets by adding roots. Access to `/home`, `/srv`, `/opt`, `/mnt` and `/var` is governed by ordinary file ACLs (the service unit must not hide those trees). No privileged HTTP filesystem helper. User-visible API and CLI text never names another server manager product; machine codes such as `amp` may remain internal.

Typical layout from another server manager: `/home/amp/.ampdata/instances/<instance>/Minecraft`. Bounded metadata in `instances.json` is read only when explicitly readable and inside a saved import root. [CubeCoders](https://discourse.cubecoders.com/t/diagnosing-amp-application-startup-issues/2289) documents that default datastore; custom roots must be supplied locally. Candidate heuristics: `server.properties`, world directories, jar/loader metadata; manager ancestry/config is evidence, not proof. Never read or return manager passwords/tokens, execute its CLI, source scripts, execute candidate jars or recursively scan the whole filesystem.

When the dedicated Vesta account cannot read a folder, inspection returns `source_unreadable` (and browse/`check` carry `remediationCommand`). A local administrator runs `sudo vesta import-root add <path>`, which registers the folder and grants narrowly scoped read/traverse access for the service account—or the owner stops the other server manager, exports a read-only copy into a readable location, and imports the copy. Do not run as the other manager's user/root, chown its files or open its whole datastore to Vesta. If using an exported copy, retain the original instance path/port as separately supplied inspection hints, and flag process correlation as unknown if unavailable. Existing `CopiedExport` protections remain mandatory.

Inspection of a non-server folder returns blocker `not_minecraft_server` with `missing:["server_jar"|"server_properties"|"world"]` and `found:[...]` so the launcher can explain what was recognised. Review carries the same `found`/`missing` fields.

## Plan and review

Request `{folderToken,name,gamePort:null|integer,memoryMiB:null|integer,launchChoice:null|{kind:"jar"|"java_argfile",relativePath},minecraftVersion:null|string}`. Nulls mean detect/default; version and safe launch choice overrides are needed only for unresolved detection. No raw shell/Java arguments, native executable paths or upload-and-run detection. Newest Java is not necessarily compatible with an old modpack.

Job response `{jobId,state,phase,error,progress,review}`. States `planning|review|applying|ready|starting|online|blocked|failed|cancelled|interrupted`. Phases `inspect|conflicts|java|snapshot|copy|configure|register|start`. Progress `{filesDone,filesTotal,bytesDone,bytesTotal}`; error null or fixed code. Review is null until computed, otherwise:

```json
{
  "planId":"<UUID>","confirmation":"<random token>","expiresAt":"2026-11-13T10:00:00Z",
  "source":{"displayPath":"/home/amp/.ampdata/instances/Survival/Minecraft","snapshot":"<sha256>","files":1200,"bytes":123456789},
  "detected":{"kind":"amp","instanceName":"Survival","minecraftVersion":"1.21.1","loader":"fabric","launch":{"kind":"jar","relativePath":"fabric-server-launch.jar"}},
  "runtime":{"requiredMajor":21,"selectedRuntimeId":"<id>","downloadRequired":true,"verified":true},
  "destination":{"serverId":"<UUID>","displayPath":"/var/lib/vesta/servers/<id>/game","freeBytes":9876543210},
  "resources":{"gamePort":25565,"memoryMiB":4096},
  "processes":[{"kind":"java","pid":123,"startIdentity":"<opaque>","relation":"source","state":"running"}],
  "ports":[{"port":25565,"protocol":"tcp","state":"occupied","owner":"source"}],
  "inspection":{"processes":"complete","ports":"complete"},
  "blockers":["source_running","game_port_in_use"],"warnings":["disable_amp_auto_restart"]
}
```

Example is blocked; apply must refuse until a fresh plan resolves blockers. `launch`, version and requiredMajor may be null when unknown, with `launch_unresolved`/`java_unresolved` blockers. Process state `running|stopped|unknown`; relation `source|amp_controller|unrelated|unknown`. Report only relevant PID/name/start identity, no environment or full command lines. Linux inspect `/proc` executable/cwd and sanitized argv for candidate jar/argfile paths, parent AMP process and start time; enumerate listening sockets for exact requested game addresses/ports and optional query/RCON ports detected in properties. PID without start identity is never reliable. AMP controller alone may stay running; source Java must be stopped, and AMP auto-restart for that instance disabled by the owner. Insufficient permission or inaccessible namespace -> inspection `unknown`, never “no conflict”. An owner acknowledgment cannot bypass observed source activity. Unknown process visibility is a warning requiring the explicit source-stopped confirmation at apply.

Choose existing trusted Java matching detected software/version or a checksum-verified managed runtime, using existing `java.rs` logic at host scope; never execute `java` found in imported files. Unknown compatibility asks for explicit version/launch selection and replans. Memory default comes from machine RAM minus OS reserve and existing reservations (one server initially); reject overbudget requests. Port defaults next unreserved/free from 25565, but source must still be stopped even when importing to a different port. Record reservations atomically at apply; no duplicate ports from racing plans.

Blockers include `source_unreadable`, `source_running`, `source_port_listening`, `source_recently_active`, `game_port_in_use`, `port_state_unknown`, `source_changed`, `unsafe_path`, `launch_unresolved`, `java_unresolved`, `insufficient_space`, `memory_budget_exceeded`, `not_minecraft_server`, `software_unresolved` (loader or Minecraft version not identified from files), `source_too_large` (bounded inventory exceeded), `port_unresolved` (no free game port in the bounded search), `memory_unresolved` (host RAM not observable), `memory_invalid` (below 512 MiB). Candidate warnings carry `{code,displayPath,remediation:null|{command,alternative}}` with codes `source_unreadable`, `outside_import_roots`, `scan_truncated`; remediation commands are for a local administrator and are never run by Vesta. A listed `folderToken` may be null where the platform cannot observe an unreadable directory's identity (Windows). Only warnings are acknowledgeable. Port change, owner stopping AMP, local ACL/export remediation, version/launch selection each require a new plan. Bind plan+confirmation to actor, source directory/file identities and content hashes, destination, launch/runtime version, resources, EULA and warning set. TTL 10min. Plans with blockers have no usable confirmation.

## Apply and start invariants

1. Recheck owner/device/generation, plan TTL, confirmation, complete warning acknowledgment and explicit EULA acceptance. Reserve registry server id, destination and ports in one transaction; repeated same key returns the same job, conflicting body is 409.
2. Require relevant source Java stopped and owner confirmation that AMP's instance auto-start/restart is disabled. Reinspect source/process start identities and sockets before snapshot/copy. Snapshot after stopping, not while live; discard earlier live inventory. If source changed from reviewed snapshot, return 409 `source_changed` and replan. Imported executable content remains untrusted even if source is owned by amp.
3. Copy read-only through existing safe snapshot/journal primitives into a new managed staging directory; no hardlinks/symlinks to source. Reject external links, device nodes, secret/private-state files and oversized/unbounded trees. Never write, delete or lock source files. Verify source stability during copy and destination hashes; live world changes abort and quarantine incomplete destination. Concurrent writers cannot be fully excluded by sampling; require a stopped source, with pre/post content validation, and document this limit.
4. Rewrite only destination config for chosen ports, loopback-only RCON if used, host-managed paths, memory and trusted Java. Record EULA acceptance; never infer it from source. Never execute AMP shell scripts. Freeze safe launch spec and destination file ownership before registering worker.
5. Recheck port availability at actual bind/start, source state and worker authority. Port inspection is advisory against races; actual bind failure returns `game_port_in_use`/start failure with copied files retained. Publish committed registry and start only through authorized operations runtime. `online` requires observed game readiness, not merely spawned PID; preserve logs and existing start failure classifications.
6. Durable checkpoints survive host restarts; uncertain copy/start becomes `interrupted`/`blocked`, no automatic recopy or duplicate game spawn. Reattach supervisor only with matching launch identity/lease. Replan before resume if source changed. Destination cleanup only via audited explicit owner action, never source cleanup.

Manual acceptance: stop the selected AMP instance on people-server, import its Minecraft directory from the Windows launcher, resolve any reported conflict, use automatic Java and observe online; compare original file hashes, test console/files/players/packs/backups through port 25500, reboot without login, reconnect using the same host identity and server ref. Verify actual default/custom AMP layouts, unreadable metadata, AMP restart races, active source on a different port and low-disk failure. Do not modify the real source just to run a test; use disposable fixture worlds for destructive fault cases.

## Implemented HTTP routes (H08 + H16/H17 wiring)

All routes below need `Authorization: Bearer <host session>` of a current primary owner/owner; anyone else gets 403 `forbidden` (inspection, folders and jobs alike). Long work never blocks the HTTP call: plan, apply and resume return 202 and run on a host background thread; the job row is durable and polled. Error bodies are `{error:{code,requestId}}`.

| Method/path | Body | Success |
|---|---|---|
| GET `/v1/host/folders/roots` | | 200 roots (filesystem + saved import roots) |
| POST `/v1/host/folders/resolve` | `{rootId,relativePath}` | 200 `{folderToken,displayPath,expiresAt}` |
| POST `/v1/host/folders/check` | `{path}` | 200 `{path,status,folderToken,remediationCommand}` |
| POST `/v1/host/folders/list` | `{folderToken,cursor}` | 200 listing (`looksLikeServer`, `remediationCommand`) |
| GET `/v1/host/imports/candidates` | | 200 candidates/warnings |
| POST `/v1/host/imports/inspect` | `{folderToken,gamePort}` | 200 inspection report (`found`/`missing`; no job) |
| POST `/v1/host/imports/resources` | `{gamePort,memoryMiB}` | 200 `{memory,port}` |
| POST `/v1/host/imports/plan` | `{folderToken,name,gamePort,memoryMiB,launchChoice,minecraftVersion}` | 202 `{jobId,state:"planning"}` |
| GET `/v1/host/imports/jobs/{jobId}` | | 200 job; while `starting` this call also observes readiness |
| POST `/v1/host/imports/apply` | `{planId,confirmation,acknowledgedWarnings,eulaAccepted,sourceStoppedConfirmed}` + `Idempotency-Key` header | 202 `{jobId,serverId}` (same key + body returns the same reply) |
| POST `/v1/host/imports/jobs/{jobId}/cancel` | empty or `{}` | 200 `{state}`: `cancelled`, or `applying` while a copy stops at its next safe boundary |
| POST `/v1/host/imports/jobs/{jobId}/resume` | empty or `{}` | 202 job; only for `interrupted` |
| POST `/v1/host/imports/jobs/{jobId}/start` | empty or `{}` | 202 job (`starting`, or `ready` with a resolvable `error`) |

Job: `{jobId,state,phase,error,progress:{filesDone,filesTotal,bytesDone,bytesTotal},review,serverId}`; `review.expiresAt` is RFC 3339 UTC; `review.confirmation` is present only in state `review`. Progress counts source files/bytes hashed while planning and verifying (phases `inspect`, `snapshot`) and copied files/bytes in phase `copy`, updated every 64 files.

Route errors: 400 `invalid_request`, `idempotency_key_required`, `eula_required`, `warnings_unacknowledged`, `source_stop_confirmation_required`; 404 `import_job_not_found`; 409 `conflict` (wrong state for the action), `idempotency_conflict`, `plan_blocked`, `plan_expired`, `confirmation_invalid`, `server_capacity_reached`, `game_port_in_use`, `memory_budget_exceeded`; 503 `imports_unavailable` (host built without import storage), `import_jobs_saturated`, `worker_unavailable`, `unavailable`.

Copy bounds: up to 2,000,000 entries, 16 GiB per file and 16 TiB total (`source_too_large` beyond), with the free-space preflight (source bytes + 512 MiB) and a running reserve during the copy as the practical bound. The snapshot inventory is held in host-private state, so very large trees cost proportional memory while planning.

Commit rewrites only the managed copy: `eula.txt` from the owner's acceptance, `server-port`, and `enable-rcon=false`. Vanilla and its common forks bind RCON to `server-ip` (the game address), so RCON cannot be confined to loopback separately; a source with RCON enabled adds the acknowledgeable warning `rcon_disabled`. `rcon.password` stays in the host-only file, which the files API refuses to serve and the properties API redacts.

Start creates the server's in-process worker on first use and routes it immediately, so `/v1/servers/{serverId}/manage/...` (console, files, players, lifecycle, status) works without restarting the host. A start that has not spawned within 30 s and is observed stopped returns the job to `ready` with `start_failed` (or `game_port_in_use` when the port is then busy); readiness `Degraded` by startup timeout or fatal output also ends in `start_failed`. The host copies each worker's observed state into the registry (on `GET /v1/host/servers` and every 2 s): `starting`, `online`, `stopped`, `failed`; this changes the revision but not the authorization generation. After a host restart every registered server with a managed `<servers>/<serverId>/host.toml` gets its worker back automatically; a job that was `planning`, `applying` or `starting` becomes `interrupted`.

Remaining deferred items: destination ownership freeze (H11), a separate revocation barrier for import jobs (H07), and process correlation on non-Linux hosts (reported `unknown`, with the acknowledgeable `source_process_unknown` warning).


Import stopped-state codes and suggested text:
- `source_process_unknown` is a warning requiring the existing explicit source-stopped confirmation and warning acknowledgement at apply: "Vesta cannot inspect a possible Java server process. Confirm that you stopped the source server."
- `source_running` remains a blocker for a process positively correlated with the source.
- `source_port_listening` blocks independently of the destination port: "Something is listening on the source game port. Stop the server, then check again."
- `source_recently_active` blocks: "The world is locked or world/log files changed within the last 60 seconds. Stop the server, wait a minute, then check again."
- `world_outside_source`: "Choose a folder containing the world and use a relative level-name inside that folder."
- `bind_address_cleared` warns: "The imported server will bind to all interfaces; Vesta clears server-ip in the copy."
- `launch_arguments_unsupported`: "The launch arguments contain unsupported options or paths. Use a self-contained server folder with supported Forge arguments, then check again."
