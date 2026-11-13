# Host folders and import v1

Only current primary owner/owner can call these HTTPS routes. Server Admin file access is confined to managed server storage and cannot reach them. No browser-local file picker: paths refer to the host, e.g. the Linux machine at 192.168.1.67. Host response paths are informational; opaque tokens identify admitted directories. No unrestricted file-read API is introduced.

## Browse and AMP candidates

| Method/path | Request -> response |
|---|---|
| GET `/v1/host/folders/roots` | `{roots:[{rootId,label,displayPath,readable}],revision}`; configured import roots only, plus owner-visible managed destination capacity. |
| POST `/v1/host/folders/resolve` | `{rootId,relativePath}` -> `{folderToken,displayPath,expiresAt}`; relative path uses slash segments, no `..`, absolute path, NUL or traversal. Enables typed host path by selecting its permitted root. |
| POST `/v1/host/folders/list` | `{folderToken,cursor:null|string}` -> `{displayPath,entries:[{name,kind:"directory",folderToken,readable}],nextCursor}`. Directories only, 100/page; no file contents. |
| GET `/v1/host/imports/candidates` | `{candidates:[{candidateId,kind:"amp"|"minecraft",label,displayPath,folderToken,confidence,evidence}],warnings}`; bounded scan of configured roots, max 100 entries. |
| POST `/v1/host/imports/plan` | Request below -> 202 `{jobId,state:"planning"}`. |
| GET `/v1/host/imports/jobs/{jobId}` | Durable job and review below. |
| POST `/v1/host/imports/apply` | `{planId,confirmation,acknowledgedWarnings:[code],eulaAccepted:true,sourceStoppedConfirmed:true}` -> 202 `{jobId,serverId}`; Idempotency-Key required. |
| POST `/v1/host/imports/jobs/{jobId}/cancel` | -> `{state}`; cancels at safe copy boundary, does not delete original or stop unrelated processes. |

Folder token random 32 bytes, expires 10min, bound to host/account/device/root and open directory identity; it is not a bearer authorization replacement. Revalidate current owner and root membership every use. Reject links/reparse points, special files, traversal, mounts outside allowed roots and overlap in either direction with host private state, any managed server or destination. Use descriptor-relative/no-follow access through scan and copy; canonicalize-once/string-prefix checks are insufficient. Roots configured by privileged local `vesta import-root add <path>`; remote owners cannot expose `/`, host secrets or arbitrary executables by adding roots. Installer registers readable default AMP candidates only within narrowly configured import roots; unreadable candidates appear as warnings, not invented contents. No privileged HTTP filesystem helper.

Typical AMP candidate `/home/amp/.ampdata/instances/<instance>/Minecraft`; inspect bounded metadata in `/home/amp/.ampdata/instances.json` only when explicitly readable and allowed. [CubeCoders](https://discourse.cubecoders.com/t/diagnosing-amp-application-startup-issues/2289) documents the default instance datastore; custom roots must be supplied locally. Candidate heuristics: `server.properties`, world directories, jar/loader metadata; AMP ancestry/config is evidence, not proof. Never read or return AMP passwords/tokens, execute AMP CLI, source scripts, execute candidate jars or recursively scan the whole filesystem.

Dedicated Vesta account may not traverse AMP home. Return `source_unreadable` with owner-visible instructions to stop AMP, then export a read-only copy into the registered staging root, or have a local administrator grant narrowly scoped read/traverse ACLs on this instance. Do not run as amp/root, chown AMP files or open its whole datastore to Vesta. If using an exported copy, retain the original instance path/port as separately supplied inspection hints, and flag process correlation as unknown if unavailable. Existing `CopiedExport` protections remain mandatory.

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

Example is blocked; apply must refuse until a fresh plan resolves blockers. `launch`, version and requiredMajor may be null when unknown, with `launch_unresolved`/`java_unresolved` blockers. Process state `running|stopped|unknown`; relation `source|amp_controller|unrelated|unknown`. Report only relevant PID/name/start identity, no environment or full command lines. Linux inspect `/proc` executable/cwd and sanitized argv for candidate jar/argfile paths, parent AMP process and start time; enumerate listening sockets for exact requested game addresses/ports and optional query/RCON ports detected in properties. PID without start identity is never reliable. AMP controller alone may stay running; source Java must be stopped, and AMP auto-restart for that instance disabled by the owner. Insufficient permission or inaccessible namespace -> inspection `unknown`, never “no conflict”. An owner acknowledgment cannot bypass an actual/unknown live-source blocker; a known stopped read-only export with independently verified port availability is the fallback.

Choose existing trusted Java matching detected software/version or a checksum-verified managed runtime, using existing `java.rs` logic at host scope; never execute `java` found in imported files. Unknown compatibility asks for explicit version/launch selection and replans. Memory default comes from machine RAM minus OS reserve and existing reservations (one server initially); reject overbudget requests. Port defaults next unreserved/free from 25565, but source must still be stopped even when importing to a different port. Record reservations atomically at apply; no duplicate ports from racing plans.

Blockers include `source_unreadable`, `source_running`, `source_state_unknown`, `game_port_in_use`, `port_state_unknown`, `source_changed`, `unsafe_path`, `launch_unresolved`, `java_unresolved`, `insufficient_space`, `memory_budget_exceeded`, `not_minecraft_server`, `software_unresolved` (loader or Minecraft version not identified from files), `source_too_large` (bounded inventory exceeded), `port_unresolved` (no free game port in the bounded search), `memory_unresolved` (host RAM not observable), `memory_invalid` (below 512 MiB). Candidate warnings carry `{code,displayPath,remediation:null|{command,alternative}}` with codes `source_unreadable`, `outside_import_roots`, `scan_truncated`; remediation commands are for a local administrator and are never run by Vesta. A listed `folderToken` may be null where the platform cannot observe an unreadable directory's identity (Windows). Only warnings are acknowledgeable. Port change, owner stopping AMP, local ACL/export remediation, version/launch selection each require a new plan. Bind plan+confirmation to actor, source directory/file identities and content hashes, destination, launch/runtime version, resources, EULA and warning set. TTL 10min. Plans with blockers have no usable confirmation.

## Apply and start invariants

1. Recheck owner/device/generation, plan TTL, confirmation, complete warning acknowledgment and explicit EULA acceptance. Reserve registry server id, destination and ports in one transaction; repeated same key returns the same job, conflicting body is 409.
2. Require relevant source Java stopped and owner confirmation that AMP's instance auto-start/restart is disabled. Reinspect source/process start identities and sockets before snapshot/copy. Snapshot after stopping, not while live; discard earlier live inventory. If source changed from reviewed snapshot, return 409 `source_changed` and replan. Imported executable content remains untrusted even if source is owned by amp.
3. Copy read-only through existing safe snapshot/journal primitives into a new managed staging directory; no hardlinks/symlinks to source. Reject external links, device nodes, secret/private-state files and oversized/unbounded trees. Never write, delete or lock source files. Verify source stability during copy and destination hashes; live world changes abort and quarantine incomplete destination. Concurrent writers cannot be fully excluded by sampling; require a stopped source, with pre/post content validation, and document this limit.
4. Rewrite only destination config for chosen ports, loopback-only RCON if used, host-managed paths, memory and trusted Java. Record EULA acceptance; never infer it from source. Never execute AMP shell scripts. Freeze safe launch spec and destination file ownership before registering worker.
5. Recheck port availability at actual bind/start, source state and worker authority. Port inspection is advisory against races; actual bind failure returns `game_port_in_use`/start failure with copied files retained. Publish committed registry and start only through authorized operations runtime. `online` requires observed game readiness, not merely spawned PID; preserve logs and existing start failure classifications.
6. Durable checkpoints survive host restarts; uncertain copy/start becomes `interrupted`/`blocked`, no automatic recopy or duplicate game spawn. Reattach supervisor only with matching launch identity/lease. Replan before resume if source changed. Destination cleanup only via audited explicit owner action, never source cleanup.

Manual acceptance: stop the selected AMP instance on people-server, import its Minecraft directory from the Windows launcher, resolve any reported conflict, use automatic Java and observe online; compare original file hashes, test console/files/players/packs/backups through port 25500, reboot without login, reconnect using the same host identity and server ref. Verify actual default/custom AMP layouts, unreadable metadata, AMP restart races, active source on a different port and low-disk failure. Do not modify the real source just to run a test; use disposable fixture worlds for destructive fault cases.
