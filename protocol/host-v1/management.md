# Host management extensions

The server paths below are relative to `/v1/servers/{serverId}/manage`. Successful management responses wrap the given payload in `{"api_version":3,"data":...}`. Role writes and startup settings use decimal-string Host revisions. A stale `expected_version` returns 412 `precondition_failed`.

## Host access

GET `/v1/host` adds `hostCapabilities` with the caller's effective Host ability IDs. Its `capabilities` feature list includes `host-access`, `role-presets`, `server-enablement` and `public-access`. Owners have every Host ability. Linked accounts with a Host grant, a server membership or an applicable default server role have `host.view`. An authenticated account without any of these receives 403 `forbidden` from GET `/v1/host`.

These paths start at `/v1/host/manage`. Responses use the management envelope and the same decimal-string Host version as server management.

| Method and path | Request and response | Authority |
| --- | --- | --- |
| GET `/capabilities` | Returns `{capabilities:[{id,label,description,group,layer,delegable}]}` | `host.view` |
| GET `/people` | Returns `{version,people:[{account_id,name,role,capabilities}]}`. Name and owner role are nullable. The role is `primary_owner` or `owner` for Host owners. Capabilities are explicitly stored Host grants. | `host.access.manage` or `host.memberships.manage` |
| PUT `/people/{accountId}/capabilities` | Accepts `{expected_version,capabilities:[id]}` and replaces all explicit Host grants. An empty array clears them. Returns `{version,person}` with the same fields as a people row. | `host.access.manage` |
| GET `/people/{accountId}/memberships` | Returns `{version,memberships:[{server_id,name,role_id,inherited}]}`. Owners inherit `owner` membership on each active server, including disabled servers. Delegated callers see memberships only on servers where they can read access. | `host.memberships.manage` |
| PUT `/people/{accountId}/memberships/{serverId}` | Accepts `{expected_version,role_id}`. A role ID replaces the grant, and an explicit null clears it. Returns `{version,server_id,role_id}`. | `host.memberships.manage` plus the same per-server assignment, revocation, rank and capability checks as the server users route |

Host grants carry no server membership. Delegated Host access managers can change another non-owner's grants only when they already hold all the target's current and proposed abilities. Server role definitions and account overrides accept only server-layer delegable capabilities.

- `host.servers.create` permits the creation catalogue, defaults, and the caller's own `new` create jobs. Starting a created server requires a membership with `lifecycle.start`. Import, restore and modpack setup retain their existing owner authority.
- `host.settings.edit` permits reading and changing Host settings on `/v1/host`.
- `host.servers.remove` permits reversible removal of a server where the caller holds an explicit membership. Gracefully stopping a running server for removal also uses the caller's `lifecycle.stop` authority. Permanent removal, removed-server browsing, restore and purge require an owner.
- `host.access.manage` also permits device listing. `devices.revoke` permits listing devices and revoking another non-owner's device subject to the Host grant authority check. Ownership and primary-owner protection still apply.
- `access-sources.manage` permits editing automatic whitelist rules and default access when the caller holds an explicit membership on that server.
- `host.presets.manage` permits Host presets. Ownership, owner recovery and agent installation keep their existing protections.

Unknown accounts and role IDs return 404 `not_found`. Unknown capability IDs and malformed fields return 400 `invalid_request`. Disallowed delegation or use of a capability from the wrong layer returns 403 `management_forbidden`.

## Roles

- GET `/roles` returns `{version,roles}`. Each role has `{id,name,rank,builtin,editable,modified,capabilities:[{capability,scope:"*"}]}`. The `modified` field identifies a server-specific edit to a built-in.
- GET `/roles/capabilities` returns the same catalog as Host management. Use entries with `layer:"server"` and `delegable:true` for editable server grants. `layer` is `host` or `server` and `delegable` applies within that layer independently of the caller.
- POST `/roles` creates a custom role with `{expected_version,name,rank,capabilities:[{capability,scope:"*"}]}`. PATCH `/roles/{roleId}` accepts the same fields with only `expected_version` and at least one change field required. Both return `{version,role}`. Role IDs survive renames. Editable ranks are 1 to 79.
- PATCH supports the stable built-in IDs `maintainer`, `moderator`, `viewer` and `pack-user`. The edit applies to all holders of that role on the selected server and revokes outstanding invitations for that role on that server.
- POST `/roles/{roleId}/reset` accepts `{expected_version}` and restores the shipped definition of an editable built-in, returning `{version,role}`. Owner and Administrator have fixed IDs, capabilities and ranks because they mark reserved authority tiers.
- DELETE `/roles/{roleId}` accepts `{expected_version}` and returns `{version,id}` for custom roles. A role in use returns 409 `role_in_use`. Built-in deletion, editing Owner or Administrator, and resetting either locked role return 403 `management_forbidden`.
- PUT `/users/{accountId}/roles` accepts `{expected_version,add:[roleId],remove:[roleId]}` and returns the updated user. A single added role replaces the current role and an empty add list clears it. Changing or clearing a role clears its overrides.
- PUT `/users/{accountId}/overrides` accepts `{expected_version,overrides:[{capability,effect:"allow"|"deny"}]}` and replaces the override set. User rows expose `overrides` as an array of permanent grants. Deny takes precedence over role grants.

Role editors need `roles.create` for creation and either `roles.create` or `roles.manage` to edit, reset or delete. Overrides require `roles.manage`. Delegation checks the current and proposed rank and capabilities, including the authority of existing assignees. Grants of Owner and Administrator remain reserved. Invalid fields return 400 `invalid_request`, unknown IDs return 404 `not_found`, and disallowed changes return 403 `management_forbidden`.

## Automatic whitelist rules

GET /access-sources returns the current version and settings. PATCH accepts
expected_version and auto_whitelist_roles. The array replaces the complete
selection. An empty array clears the selection. Role IDs come from GET /roles
and include built-in roles (including owner) and custom role UUIDs. Reads
require access-sources.manage. Writes also require players.moderate. As with
other worker management endpoints, api_version is the string "3".

```json
{"expected_version":"<current access-sources version>","auto_whitelist_roles":["owner","viewer"]}
```

On online-mode servers Vesta Server adds verified Minecraft profiles whose
effective role is selected, subject to Minecraft bans. Losing the role or
clearing the selection removes entries Vesta Server added. Existing manual entries
are retained. This setting does not toggle whitelist enforcement.

Use `["*"]` to select all roles. An enabled legacy `sync_whitelist` setting migrates to this selection on Host-managed and standalone servers. Only verified linked profiles are eligible.

The legacy boolean is still accepted for older launchers. True selects all roles. False clears the all-roles or Owner selection. Send either the array or the boolean in one PATCH.

## Image previews

GET /files/image-preview?path=<encoded game-relative path> requires files.read.
It returns the original bytes with a signature-sniffed image/png, image/jpeg,
image/gif or image/webp Content-Type and x-vesta-api-version: 3. Responses use
Cache-Control: no-store. Other formats return 415 unsupported_media_type.
Files over 8 MiB return 413. The usual Files scope and protected-path rules
apply.

## Default access and public visibility

When GET `/v1/host` advertises `public-access`, GET `/default-access` reads the settings and PUT `/default-access` replaces them together. Both require a Host owner or a caller with the Host ability `access-sources.manage` and an explicit membership on that server. Only Host owners can change visibility.

PUT requires all three fields. Use a role ID from `eligible_roles` or explicit null to clear the default.

```json
{"expected_version":"42","default_role_id":"pack-user","visibility":"public"}
```

GET and PUT return 200 with the following envelope. The two visibility values are `private` and `public`. Clearing a public server's default requires an owner to send `"visibility":"private"` in the same PUT.

```json
{
  "api_version": "3",
  "request_id": "request-1",
  "data": {
    "version": "43",
    "default_role_id": "pack-user",
    "visibility": "public",
    "can_manage_visibility": true,
    "eligible_roles": [{"id":"pack-user","name":"Pack User"}]
  }
}
```

Defaults can project only `public.info`, `pack.download`, `pack.realtime` and `pack.access`. Pack User is eligible and its bundled `status.view` applies only to explicit memberships. A selected role is in use. Deleting it returns 409 `role_in_use`, and editing it, resetting it or applying a preset that would introduce unsafe abilities returns 403 `management_forbidden`. PUT returns 403 for an unsafe role or insufficient authority, 404 for an unknown role or inaccessible server, 400 for malformed fields or public visibility with a null role, and 412 for a stale version.

A default applies to linked accounts on both private and public servers. Owners and explicit memberships take precedence. Inherited roles appear in the caller's server list and worker session abilities. Membership lists show explicit grants and Host ownership. Revoking a server membership through either access route records an exclusion that also blocks fallback and invite redemption for that account. Assigning an explicit role clears the exclusion. Minecraft account bans block Host pack access. Changing the default advances the Host authorization generation and synchronizes the worker.

Public visibility enables anonymous exact-port discovery with GET `/v1/here?port=25565`. Disabled, private, removed and unknown servers return 404. Pack status, tickets, artifacts and realtime require a linked Host device. New and restored servers start private with a null default.

## Host role presets

These routes start at `/v1/host/manage` and require `host.presets.manage`. A preset has `{id,name,rank,capabilities:[{capability,scope:"*"}],default_for_new_servers,applicable}`.

| Method and path | Request and response |
| --- | --- |
| GET `/role-presets` | Returns `{version,presets}` |
| POST `/role-presets` | Copies a server role selected by `{expected_version,server_id,role_id,default_for_new_servers?}`. Returns `{version,preset}`. |
| PATCH `/role-presets/{presetId}` | Accepts `{expected_version,name?,rank?,capabilities?,default_for_new_servers?}` with at least one change. Returns `{version,preset}`. |
| DELETE `/role-presets/{presetId}` | Accepts `{expected_version}` and returns `{version,id}`. Existing copies stay on their servers. |
| POST `/role-presets/{presetId}/apply` | Accepts `{expected_version,server_id}` and returns `{version,role}`. Creates a custom role with the preset's ID or updates the existing role with that ID on the destination server. |

Saving from a server requires access-list authority on that server. Applying requires the same destination role authority and delegation checks as regular server role writes. Newly registered servers receive the shipped built-ins and a copy of each preset marked `default_for_new_servers`. The flag defaults to false, and only owners can mark, change or delete a default preset. Presets with reserved rank or capabilities may be saved and renamed, with `applicable:false` until the rank and capabilities fit a custom role. Applying or defaulting an incompatible preset returns 403 `management_forbidden`.

## Startup and enablement

GET and PATCH `/startup` return `{version,on_agent_start,start_delay_seconds,last_startup,enabled}`. PATCH accepts `{expected_version,on_agent_start?,start_delay_seconds?}`. `on_agent_start` is `restore_previous`, `stay_stopped`, `sleep` or `start`. Delay is an integer from 0 to 3600 and applies to automatic launch, including a restored running state. Configure distinct delays to stagger launches. The report is null before evaluation or `{at_ms,applied_action,outcome,error_code}`, where action is `stay_stopped`, `sleep` or `start`, outcome is `pending`, `applied`, `already_running` or `failed`, and error_code is nullable. An applied start means the runtime accepted the start request. Read requires `status.view`. Write requires `config.edit` plus `lifecycle.start` for start or restore previous, or `lifecycle.stop` for stay stopped or sleep. Invalid values return 400 `invalid_request`.

Host settings expose `defaultOnAgentStart` with the same action values. The initial default is `restore_previous`. A newly registered server adopts the default in effect at registration. Setup, import and restore completion leave Minecraft stopped. Startup actions apply when an enabled worker opens at Host boot or after enablement.

GET `/enabled` returns `{version,enabled,can_manage}` to server members, including while disabled. `can_manage` is whether the caller can enable or disable this server. PATCH `/enabled` accepts `{expected_version,enabled}` and returns `{version,enabled}`. It requires the server-layer ability `lifecycle.enable`, which owners and the shipped Administrator hold and which can be delegated. GET `/v1/host/servers` includes disabled members' server cards with `state:"disabled"` and `enabled:false`. Disabled servers keep their files, backups, memberships and startup settings. The Host skips their workers at boot. Enablement reopens the worker and applies its startup action and delay.

Disabling closes admissions and asks the existing game runtime for a graceful shutdown. Authenticated server routes that require a disabled worker return 409 `server_disabled`. Public pack assets return 404 while that worker is offline. Host access, roles and startup settings remain available to authorized callers during disablement. The same Host revision precondition applies to enablement.

| Status and code | Meaning |
| --- | --- |
| 409 `backup_job_active` | An active backup or verification job prevents disablement |
| 409 `restore_job_active` | A restore requires confirmation, execution or recovery |
| 409 `server_busy` | Automation is applying an action, a software or removal job prevents admission, or the Host is stopping |
| 409 `server_disable_pending` | Disablement is saved and new worker requests are fenced. The previous worker is still shutting down. Refresh the Host revision before retrying. |
| 503 `worker_unavailable` | A worker could not be opened or shut down. An ordinary enable-open failure keeps the server disabled and records the startup error. |

Pending workflow runs are cancelled with outcome `server_disabled` and audited as skipped. When the worker next opens, each due schedule produces one skipped summary for the latest missed occurrence. It appears in notifications, the audit log, and available run history as `state:"cancelled"` with `outcome_code:"server_disabled"`. Their missed-run policy does not replay work from the disabled interval.

## File deletion

POST `/files/delete` accepts `{items:[{path,expected_sha256?,recursive?}]}` with 1–100 items and the existing `Idempotency-Key` header. Send `expected_sha256` for a file. Explicitly set `recursive:true` for a directory. The same request shape works for a single directory. Paths are game-root-relative. Requires `files.delete` and authority on every descendant of a directory.

Returns `{batch_id,results:[{path,trash_id,error_code}]}` with nullable trash_id and error_code, in request order. Successful items share the batch ID and can be restored independently through POST `/trash/{trashId}/restore`. Path failures are per item, including `file_path_invalid`, `file_path_protected`, `file_path_linked`, `file_not_found`, `file_not_regular`, `file_not_directory`, `file_conflict`, `file_too_large` and `file_trash_full`. GET `/trash` entries add `directory` and nullable `batch_id`. For a directory, `sha256` identifies the recorded layout and file sizes. Directory moves require the game and state directories to share a filesystem. An interrupted batch can return 409 `recovery_required` after an item was moved. Inspect trash before sending a fresh request. Invalid requests return 400 `invalid_request`, missing permission returns 403 `management_forbidden`, and a reused key with different inputs returns 409 `idempotency_conflict`.

## Backups

Backup policy and configuration profiles accept optional positive integers `max_snapshot_bytes` and `max_collection_bytes`. Missing or null fields mean unlimited. The former caps the logical bytes in a single snapshot. The latter measures the profile's referenced stored bytes, including chunks shared with its other snapshots once. After a backup, collection retention removes oldest eligible entries first, preserving the newest snapshot, retention-selected entries, and pinned or unexpired keep points. Failed jobs expose `backup_snapshot_size_limit_reached` or `backup_collection_limit_reached` in `error_code`.

GET `/backups/storage` includes `profiles:[{name,collection_bytes,max_snapshot_bytes,max_collection_bytes}]`. GET `/backups/policy` returns `{source,revision,profiles,limits}`. PUT `/backups/policy` accepts `{expected_revision,profiles,limits?}` and returns `{source:"stored",revision}`. Use null for `expected_revision` while host configuration applies. GET and PUT `/backups/config` also expose or accept the profile fields. As before, the policy and configuration writes require `backups.configure`, and a stale revision returns 409. Start a backup with POST `/backups/run` and `{profile}` plus `Idempotency-Key`. It returns HTTP 202 with `{job_id,kind:"backup",restore_mode:"inspection_only"}`. GET `/backups/jobs/{job_id}` reports the terminal result.

The workflow definition on POST `/automation/workflows` and PUT `/automation/workflows/{workflowId}` accepts `{"kind":"backup_failed","profile":null}` for all profiles or a profile name as the filter. GET `/automation/runs/{runId}` returns a run whose `trigger_event` contains `{kind:"backup_failed",profile,error_code}`. Use {{backup_error_code}} in a notification body to include the failure code. Backups initiated by a failure-triggered workflow do not recursively start more failure workflows.
