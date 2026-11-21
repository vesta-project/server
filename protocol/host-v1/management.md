# Host management extensions

The server paths below are relative to `/v1/servers/{serverId}/manage`. Successful management responses wrap the given payload in `{"api_version":3,"data":...}`. Role writes and startup settings use decimal-string Host revisions. A stale `expected_version` returns 412 `precondition_failed`.

## Roles

- GET `/roles` returns `{version,roles}`. Each role has `{id,name,rank,builtin,capabilities:[{capability,scope:"*"}]}`. Custom role IDs are generated UUIDs and survive renames. Built-ins are immutable presets.
- GET `/roles/capabilities` returns `{capabilities:[{id,label,description,group,delegable}]}`. It includes every stable capability ID. `delegable` reports whether the capability is eligible for a server grant, independent of the caller's own authority.
- POST `/roles` accepts `{expected_version,name,rank,capabilities:[{capability,scope:"*"}]}`. PATCH `/roles/{roleId}` accepts the same fields with only `expected_version` required. Both return `{version,role}`. To duplicate a preset, submit its desired capabilities and a lower rank in a new role.
- DELETE `/roles/{roleId}` accepts `{expected_version}` and returns `{version,id}`. A role in use returns 409 `role_in_use`.
- PUT `/users/{accountId}/roles` accepts `{expected_version,add:[roleId],remove:[roleId]}` and returns the updated user. As before, a single added role replaces the current role and an empty add list clears it. Clearing a role clears overrides.
- PUT `/users/{accountId}/overrides` accepts `{expected_version,overrides:[{capability,effect:"allow"|"deny"}]}` and replaces the override set. It returns the updated user. User rows expose `overrides` as an array. Deny takes precedence over role grants.

Role editors must have `roles.create` for creation and either `roles.create` or `roles.manage` to edit or delete a custom role. Overrides require `roles.manage`. Host owners can use all of these actions. Delegation checks the current and proposed rank and capabilities, including those of existing assignees. Administrator and Owner are reserved for local authority when issuing new grants. Invalid fields return 400 `invalid_request`, unknown IDs return 404 `not_found`, and disallowed changes return 403 `management_forbidden`.

## Startup

GET and PATCH `/startup` return `{version,on_agent_start,start_delay_seconds,last_startup}`. PATCH accepts `{expected_version,on_agent_start?,start_delay_seconds?}`. `on_agent_start` is `restore_previous`, `stay_stopped`, `sleep` or `start`. Delay is an integer from 0 to 3600 and applies to automatic launch, including a restored running state. Configure distinct delays to stagger launches. The report is null before evaluation or `{at_ms,applied_action,outcome,error_code}`, where action is `stay_stopped`, `sleep` or `start`, outcome is `pending`, `applied`, `already_running` or `failed`, and error_code is nullable. An applied start means the runtime accepted the start request. Read requires `status.view`. Write requires `config.edit` plus `lifecycle.start` for start or restore previous, or `lifecycle.stop` for stay stopped or sleep. Invalid values return 400 `invalid_request`.

The owner-only Host settings response and PATCH `/v1/host` accept `defaultOnAgentStart` with the same action values. The initial default is `restore_previous`. A newly registered server adopts the default in effect at registration. Setup, import and restore completion leave Minecraft stopped. The policy is applied on a subsequent Agent start.

## File deletion

POST `/files/delete` accepts `{items:[{path,expected_sha256?,recursive?}]}` with 1–100 items and the existing `Idempotency-Key` header. Send `expected_sha256` for a file. Explicitly set `recursive:true` for a directory. The same request shape works for a single directory. Paths are game-root-relative. Requires `files.delete` and authority on every descendant of a directory.

Returns `{batch_id,results:[{path,trash_id,error_code}]}` with nullable trash_id and error_code, in request order. Successful items share the batch ID and can be restored independently through POST `/trash/{trashId}/restore`. Path failures are per item, including `file_path_invalid`, `file_path_protected`, `file_path_linked`, `file_not_found`, `file_not_regular`, `file_not_directory`, `file_conflict`, `file_too_large` and `file_trash_full`. GET `/trash` entries add `directory` and nullable `batch_id`. Directory moves require the game and state directories to share a filesystem. An interrupted batch can return 409 `recovery_required` after an item was moved. Inspect trash before sending a fresh request. Invalid requests return 400 `invalid_request`, missing permission returns 403 `management_forbidden`, and a reused key with different inputs returns 409 `idempotency_conflict`.

## Backups

Backup policy and configuration profiles accept optional positive integers `max_snapshot_bytes` and `max_collection_bytes`. Missing or null fields mean unlimited. The former caps the logical bytes in a single snapshot. The latter measures the profile's referenced stored bytes, including chunks shared with its other snapshots once. After a backup, collection retention removes oldest eligible entries first, preserving the newest snapshot, retention-selected entries, and pinned or unexpired keep points. Failed jobs expose `backup_snapshot_size_limit_reached` or `backup_collection_limit_reached` in `error_code`.

GET `/backups/storage` includes `profiles:[{name,collection_bytes,max_snapshot_bytes,max_collection_bytes}]`. Policy response documents and PUT `/backups/policy` accept the two limits within each existing profile object.

Workflow triggers accept `{"kind":"backup_failed","profile":null}` for all profiles or a profile name as the filter. The run's `trigger_event` contains `{kind:"backup_failed",profile,error_code}`. Use {{backup_error_code}} in a notification body to include the failure code. Backups initiated by a failure-triggered workflow do not recursively start more failure workflows.
