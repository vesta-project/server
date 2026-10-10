# Remote management API contract — DRAFT 0.1

**Not implemented. Not an available API.** This is a proposed, versioned contract
for frontend mocks and owner review, not permission to enable remote management.
Every endpoint below is **NOT IMPLEMENTED** as a remote management endpoint.
Existing pack APIs and basic OS-local CLI/IPC do not implement this boundary.
Numbers and wire shapes below are draft proposals unless identified as binding.

Binding sources:
- [Implementation plan](../SERVER_SOFTWARE_IMPLEMENTATION_PLAN.md), sections 5,
  6 and 21; phase dependencies use its P2–P5 definitions.
- [Requirements](../SERVER_SOFTWARE_REQUIREMENTS.md): Management authentication;
  Pack access versus management access; Trust boundaries and powerful operational
  permissions; Durable operations and concurrency.
- [Local operator design](local-operator-design.md).
- Existing route conventions/refusal: `crates/vesta-transport/src/routes.rs`.

## 1. Versioning and compatibility

Proposed HTTPS base path: **`/api/v3/manage`**. Paths in tables are relative to it.
Draft document version `0.1` is not a released API version. A future OpenAPI
specification and event schemas must freeze the reviewed wire contract first.

`/api/v2/manage` and descendants stay refused with HTTP **403** and fixed code
`management_unavailable`. V2 authenticates pack access, not canonical management
principals/devices/sessions; neither an alias nor a redirect may upgrade it.
V3 is not currently routed; this document does not claim a deployed V3 refusal.

A future authenticated `/capabilities` response negotiates API/schema version,
stable server identity, supported adapters/features and effective request limits.
Feature absence must be explicit, never inferred from public pack discovery.
Stable Vesta identity is distinct from renewable transport certificates.

Compatibility proposals:
- Breaking semantics, authority changes or required fields require a new major
  version. Released versions never silently reinterpret a desired action.
- Additive response fields are permitted; clients ignore unknown response fields.
  Unknown request fields, query keys, action tags and enum inputs are rejected.
- Closed error codes and operation states are frozen per major version. New
  optional events/features require negotiated support; unknown events trigger
  resynchronization rather than an inferred state transition.
- Use decimal strings for generations, revisions, sequences and byte counts;
  timestamps are UTC RFC 3339. IDs/cursors are opaque, bounded strings.

## 2. Authentication — unavailable until P2

Management is a **canonical device/session boundary that does not yet exist**.
Transport reachability, player identity and pack access are not substitutes.

| Placeholder state/flow | Required future behavior | Status |
| --- | --- | --- |
| Owner uninitialized → bootstrapped | Reviewed OS-identity verification, one-time canonical owner recording and protected policy; no inference from existing files | NOT IMPLEMENTED; P2 |
| Device unenrolled → enrolled | Explicit server-scoped, role-scoped invitation and proof of possession; owner approval under current policy | NOT IMPLEMENTED; P2 |
| Enrolled device → authenticated session | Verified mTLS client certificate bound to canonical principal/device and server identity | NOT IMPLEMENTED; P2 |
| Session → expired/revoked | Immediately invalidate requests, subscriptions and authority for future queued steps | NOT IMPLEMENTED; P2 |
| Identity/key rotation or recovery | Authenticated continuity or explicit OS-authorized recovery; unexpected identity change blocks management | NOT IMPLEMENTED; P2 |

These states do not define usable bootstrap/enrolment endpoints. Public discovery
and pairing must remain reachable without a client certificate, but every
management route requires verified device authentication. Public-CA transport
certificates do not replace per-device mTLS. A proxy must provide verifiable
client authentication; arbitrary forwarded certificate headers are not trusted.

For every request: verify device/session, resolve current canonical policy,
authorize capability plus resource scope, validate desired input, audit admission,
and persist intent before effects. Repeat current-policy checks before queued
steps; expiry/revocation cannot leave durable authority behind. Optional step-up
(TOTP/local security PIN) is disabled by default and never replaces this boundary.

**Not accepted as management credentials or grants:** pack passwords, pack/public
sessions or download/realtime tickets, Microsoft/Minecraft proofs or receipts,
Minecraft UUID/name/whitelist membership, OP/operator level, connector credentials,
loopback/LAN location, reverse-proxy proximity, process ownership, installation
account identity, config/file possession, arbitrary passwords, or bearer possession.
A future explicitly enrolled identity may link Minecraft identity; proof alone
never creates a role. OS-local CLI authorization remains local only.

## 3. Common protocol

### Envelopes and fixed errors

JSON success: `{"api_version":"3","request_id":"opaque","data":{...}}`.
Collections use `data.items`, `data.next_cursor` (null at end), and a stable snapshot
reference. Accepted durable mutations return **202** with `data.operation_id`
and `data.state`; admission is not execution, readiness or success.

All HTTP failures use the same proposed envelope:
`{"api_version":"3","request_id":"opaque","error":{"code":"invalid_request"}}`.
Only documented safe fields (`operation_id`, `retry_after_seconds`) may be added
to `error`; no exception text, rejected values, internal paths or raw diagnostics.
TLS handshake failures occur before HTTP and cannot promise a JSON envelope.

| HTTP | Fixed code(s), closed list for this draft | Meaning |
| --- | --- | --- |
| 400 | `invalid_request`, `confirmation_required` | Invalid shape/value, or powerful action missing confirmation |
| 401 | `management_authentication_required`, `management_session_invalid` | Missing or invalid canonical device/session |
| 403 | `management_unavailable`, `management_forbidden` | Boundary disabled/unavailable, or current policy denies action |
| 404 | `endpoint_not_found`, `resource_not_found` | Unknown route, or absent/not-visible resource |
| 409 | `operation_conflict`, `idempotency_conflict`, `confirmation_invalid`, `recovery_required`, `cursor_gap` | Conflict, changed/reused/expired confirmation, unsafe recovery or unavailable history |
| 412 | `precondition_failed` | Stale generation, configuration version or file validator |
| 413 | `request_too_large` | Byte/item limit exceeded |
| 428 | `precondition_required` | Required version/generation/If-Match absent |
| 429 | `rate_limited` | Admission budget exceeded; bounded `Retry-After` |
| 503 | `feature_unavailable`, `temporarily_unavailable` | Negotiated feature unavailable, or temporary capacity/dependency failure |
| 500 | `internal_error` | Redacted unexpected failure; never implies safe retry |

Authorization precedes resource disclosure. Not-visible resources return
`resource_not_found`; fixed codes must not reveal paths, secret existence or roles.
Operation failures reuse this list with separately typed, redacted result states.

### Idempotency, operations and confirmation

Every POST/PATCH/PUT/DELETE requires `Idempotency-Key` (16–128 ASCII characters).
Scope: stable server identity + canonical principal/device + method + route.
Persist its association with canonical desired inputs and preconditions before
any side effect. Same key/same intent returns the original result/operation;
changed intent returns `idempotency_conflict`. Recheck current authorization on
replay; a saved result is not an authority grant. Retain associations at least
as long as their operation summaries (default 90 days). After advertised expiry,
a lost-outcome mutation requires reconciliation, not blind submission with a new key.

Long-running/material mutations have durable operation IDs and states:
`queued`, `validating`, `awaiting_confirmation`, `running`, `cancelling`,
`succeeded`, `failed`, `cancelled`, `recovery_required`. Summaries expose typed
progress/result, timestamps, safe-to-cancel status/reason code and audit correlation,
not raw submitted payloads, logs or credentials. Serialize conflicting concrete
resources; normally queue in request order. Safe reads require stable snapshots.
A disconnect does not cancel work. Unknown non-idempotent console outcomes enter
`recovery_required`; historical commands are never replayed automatically.

Powerful actions first obtain confirmation from `POST /confirmations` with an
allowlisted action tag, exact target ID, expected generation and complete desired
parameters/source versions. The server resolves and returns a bounded redacted
preview plus an opaque, single-use token (proposed lifetime: 120 seconds).
Bind token to actor, device/session, stable server, target, generation, action,
canonical parameters, source revision/validators and expiry. Reject reuse, changed
parameters/targets, stale state or revoked authority. A stopped server retains a
monotonic control generation; every process incarnation has a distinct generation.
Precise generation advancement rules require schema review before implementation.

The final mutation supplies `confirmation_token`; recording its consumption and
idempotency association must be atomic so a retry retrieves the original operation
rather than executing again. Confirmation is intent approval, not authorization.
Typed target acknowledgement is required for kill, deletion and raw console;
the server validates it against that exact preview. Never accept a generic “yes”
as transferable approval. Current policy may require other actions to confirm too.
Stop/kill priority never bypasses consistency locks: cancelling competing work
requires confirmation of that exact operation and auditable safe cleanup first.

### Pagination, limits, rate limits and redaction

Proposed defaults/hard ceilings (advertised limits may be stricter):

| Surface | Bound |
| --- | --- |
| All collections | `limit` default 50, maximum 100; opaque cursor ≤512 bytes; no unbounded `all` option |
| Ordinary request / any JSON response | 64 KiB request; 1 MiB response; lists truncated via next cursor, not silent data loss |
| File content request/read | ≤256 KiB decoded content, ≤1 MiB JSON; larger/resumable transfer out of this draft |
| IDs / names / action strings | IDs ≤128 bytes; names ≤255 UTF-8 bytes; console command ≤1,024 UTF-8 bytes |
| Console and audit tail | Default 50, maximum 200 entries and 64 KiB per response; resumable sequence cursor |
| Typed action/config change batch | Maximum 32 fields/arguments; nested schemas and string lengths explicitly bounded |
| Realtime | ≤64 KiB frame; ≤100 events/frame; bounded per-device subscriptions and server queues |

Pagination cursors bind server, scope, filters, snapshot and expiry. Expired list
snapshots or missing retained events return `cursor_gap`; clients reload the
snapshot, not invent missing state. Default operation-summary retention is 90 days;
audit retention is independent. Console history follows the bounded supervisor
ring/existing-log cursor design, not a new persistent console store.

Proposed admission budgets: per device 120 reads/minute, 20 mutations/minute,
5 confirmation requests/minute and 2 realtime connections; also bounded global,
per-principal and pre-auth peer budgets, worker queues and operation concurrency.
No IP-only authority or rate-limit exemption. Return 429 with a bounded retry hint,
not a reservation. Remote throttling must not disable protected local recovery.

Responses expose opaque scoped resource references and safe typed projections.
Never return secrets, credential material, absolute/relative filesystem paths,
argv, executable locations, raw audit payloads or unfiltered config. File browsing
uses IDs and sanitized single-component labels, not path strings. Client validators
such as file ETags are server-issued preconditions, **not client-selected executable
hashes**. Clients request desired changes; they never supply executable paths,
download URLs, artifact hashes, launch arguments or arbitrary scripts.

Requirements explicitly forbid relying on heuristic console/log redaction. This
strict redacted draft exposes only adapter-produced safe console projections;
unstructured output/content that cannot meet the secret/path rule is refused.
Raw-console output and arbitrary file-content exposure need owner resolution
before expanding this draft; explicit capability checks alone do not sanitize them.

## 4. Resources and endpoints

**Every row: NOT IMPLEMENTED.** Phase labels are prerequisites, not implementation
claims or delivery promises. All resource routes also depend on P2 authorization
and P1 durable operations/audit. `S` below means `/servers/{server_id}`.
Only authorized resource IDs are visible. No endpoint grants roles implicitly.

| Method and path | Desired input / safe result | Status / dependency |
| --- | --- | --- |
| GET `/capabilities` | Negotiated identity, schemas, feature flags and limits | NOT IMPLEMENTED; P2 |
| POST `/confirmations` | Bound preview/token for exact typed action and preconditions | NOT IMPLEMENTED; P2 + action phase |
| GET `/servers` | Paginated scoped IDs, safe labels and coarse state | NOT IMPLEMENTED; P2, P3 for live state |
| GET `S` | Safe identity, adapter/features, desired/effective versions; no launch details | NOT IMPLEMENTED; P2/P3 |
| GET `S/status` | Generation, process state, readiness, freshness/source, active operation references | NOT IMPLEMENTED; P3 |
| POST `S/lifecycle/start` | Expected generation; launch only locally approved configuration | NOT IMPLEMENTED; P3 |
| POST `S/lifecycle/stop` | Expected generation; graceful stop only | NOT IMPLEMENTED; P3 |
| POST `S/lifecycle/restart` | Expected generation + confirmation; explicit stop-then-start intent | NOT IMPLEMENTED; P3 |
| POST `S/lifecycle/kill` | Expected generation + exact typed acknowledgement + confirmation; separate force action | NOT IMPLEMENTED; P3 |
| GET `S/config` | Allowlisted structured settings, optimistic `version`, desired/effective values, restart-required flags | NOT IMPLEMENTED; P4 |
| PATCH `S/config` | `expected_version`, bounded typed desired field changes; privileged changes confirmed | NOT IMPLEMENTED; P4 |
| GET `S/files` | Parent directory ID + paginated scoped entries; labels, kind, size, file validator | NOT IMPLEMENTED; P4 |
| GET `S/files/{file_id}` | Bounded safe content projection plus strong ETag; no raw arbitrary files | NOT IMPLEMENTED; P4 |
| PUT `S/files/{file_id}` | Bounded content, strong `If-Match` hash ETag; overwrite approval where powerful | NOT IMPLEMENTED; P4 |
| POST `S/files` | Parent ID, validated single-component name/content; create only, no replacement | NOT IMPLEMENTED; P4 |
| DELETE `S/files/{file_id}` | `If-Match` hash ETag + generation + confirmed typed acknowledgement; move to trash | NOT IMPLEMENTED; P4 |
| GET `S/files/trash` | Paginated recoverable entry IDs, safe labels, size and retention metadata | NOT IMPLEMENTED; P4 |
| GET `S/pack/status` | Workspace state, published/current/minimum revision and active build reference | NOT IMPLEMENTED; P5 |
| POST `S/pack/publish` | Expected workspace version/current revision, chosen minimum revision + confirmation | NOT IMPLEMENTED; P5 |
| GET `S/pack/revisions` | Paginated immutable revision summaries | NOT IMPLEMENTED; P5 |
| GET `S/pack/revisions/{revision}` | Safe manifest summary, compatibility/signature state; no source paths/URLs | NOT IMPLEMENTED; P5 |
| GET `S/console/tail` | Cursor/limit; bounded safe adapter projection, source generation and explicit gaps | NOT IMPLEMENTED; P3; redaction gate |
| POST `S/console/commands` | Typed command union, generation + exact preview confirmation; operation ID | NOT IMPLEMENTED; P3 |
| GET `/operations` | Paginated authorized operations; bounded state/server filters | NOT IMPLEMENTED; P2 + producing phase |
| GET `/operations/{operation_id}` | Safe progress/outcome, cancellation eligibility and audit correlation | NOT IMPLEMENTED; P2 + producing phase |
| POST `/operations/{operation_id}/cancel` | Exact operation/version + confirmation if powerful; safe-point cancellation request | NOT IMPLEMENTED; P2 + producing phase |
| GET `S/audit/tail` | Bounded cursor-based safe event projection; opaque correlation, no raw principals/device IDs/targets | NOT IMPLEMENTED; P2 |
| GET `S/players` | Paginated authorized current-player projection; stable player references and freshness | NOT IMPLEMENTED; P3 adapter |
| GET `S/players/{player_id}` | Authorized safe identity/admission/online projection; unknown is explicit | NOT IMPLEMENTED; P3 adapter |

Lifecycle invariants: Start ownership is not readiness. Stop timeout leaves the
same owned generation stopped-or-running/uncertain as observed; it never silently
force-kills. Restart waits for actual exit and I/O completion before launching a
new generation; failed stop means **no new start**. Kill targets only the exact
owned generation/process tree after confirmation and competing-operation cleanup.
No automatic restart, world restore, binary replacement or EULA acceptance.

Config writes distinguish desired from effective state; they do not auto-restart.
Protected agent state, owner/device policy, vault, update files, supervisor control,
approved scripts and launch definitions are not editable through these routes.
File APIs share confinement, scopes, ignore rules, budgets and audit; IDs do not
bypass traversal/link/junction/race defenses. Missing/stale If-Match returns 428/412;
creation never upserts. Deletion/overwrite preserves recoverable bytes under trash
policy. Trash recovery/purge and recursive deletion are not defined in this draft.
Executable mod/plugin changes are trusted-operator code execution, require a
separately reviewed capability/approval flow and cannot be smuggled through text
editing. Runtime/executable content management is outside these generic endpoints.

Pack publish builds from the approved canonical workspace, preserves immutable
revisions and advances publication atomically only on success. It cannot accept a
client artifact URL/hash/path or implicitly restart the game. Existing public V2
pack delivery does not imply this P5 management surface exists.

Command input is a closed tagged union: adapter-validated typed actions with
bounded player references/arguments; or `raw_console` with one bounded line and
an explicit raw-console capability. Reject newlines/control characters, unknown
actions and interpolation. Raw console is powerful trusted-operator access, never
shell/argv execution; Moderator has no raw-console entry by default. Confirmation
binds the exact command and generation; unsafe replay is forbidden. No
`execute_as_player`. Exact typed action catalogue remains an owner/schema question.

## 5. Events and realtime

Reuse the existing **purpose-specific ticket → realtime upgrade → bounded notices**
pattern, not V2 pack credentials or authority. Proposed `POST /realtime/ticket` and
`GET /realtime` are **NOT IMPLEMENTED; P2 plus producing phases**. Management tickets
must be short-lived, single-use, purpose/scoped-device-bound and insufficient
without verified management authentication. Browser/proxy mTLS support is a gate.

Define event types only here: `server.status_changed`, `server.generation_changed`,
`config.changed`, `files.changed`, `pack.status_changed`, `pack.revision_published`,
`console.available`, `players.changed`, `operation.changed`, `audit.available`,
`management.authorization_changed`, `stream.gap`.

Future event schemas carry version, opaque event ID/cursor, timestamp, scoped
resource reference and generation/revision where relevant. Use safe change notices,
not raw console/file/audit content; re-fetch authorized snapshots. Cursors have
explicit retention gaps. Bounded queues disconnect slow consumers with resync
required. Revalidate subscriptions under current policy; revoke/expiry terminates
access immediately. Streams are observations, not authorization or command replay.

## 6. Explicit non-goals

Backups/restoration, SFTP, Discord integration, scheduling, sleep/MOTD automation,
and metrics are future work per owner; no routes or promises are defined here.
Also excluded: web UI hosting/proxying, shell/script execution, arbitrary downloads,
remote bootstrap via bearer/config, automated recovery/restart, server creation/
adoption/deletion, signed binary updates, file archives/resumable transfer, trash
restore/purge, role-management routes and an alternative raw TCP management path.
Local CLI capabilities do not imply remotely available counterparts.

## 7. Open questions for the owner

- Approve the V3 namespace, draft limits/rate budgets, version-negotiation shape,
  capability names, safe labels and release/deprecation policy?
- Which owner-bootstrap/recovery OS adapter and enrollment/invitation UX satisfy
  P2, and how do browser/frontends prove mTLS through supported proxies?
- What is the canonical device/session binding and rotation protocol, and which
  actions require optional step-up beyond exact-intent confirmation?
- What advances stopped/control generations and configuration/workspace versions,
  and which precise preconditions must invalidate a preview?
- How should strict secret/path-free responses coexist with requirements for
  non-heuristically-redacted console/log access and arbitrary text editing?
  Which content must be withheld versus separately designed privileged access?
- Which adapter-typed moderation commands and player identity fields belong in
  the initial contract, and what freshness/unknown-state behavior is acceptable?
- What trash quota/retention and restore policy, event retention, audit projection
  and idempotency-expiry reconciliation UX should be frozen?
- Which conflicts queue versus return an existing operation, and how is exact-task
  cancellation presented without overriding consistency or local recovery?

## Implementation gate

Do not enable this surface until **P2** proves canonical bootstrap/device/session
policy, mTLS/proxy verification, revoke/expiry, replay and cross-server isolation.
Lifecycle/status/console/player endpoints additionally require the **P3** supervisor,
protected IPC, generation/readiness and Windows/Linux failure-drill gates. P4/P5
routes need their own phase gates. Review OpenAPI/event schemas, durable admission,
confirmations, redaction and bounded mock fixtures before a frontend treats this
**draft, not implemented contract** as a production API.
