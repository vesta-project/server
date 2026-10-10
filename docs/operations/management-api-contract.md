# Remote management API contract — DRAFT 0.1

**P2 authentication core is implemented as specified in section 2. Everything
else remains DRAFT and NOT IMPLEMENTED.** This is not an available operational
management API: authenticated management placeholders return 501. Sections 3
onward describe proposals, not deployed endpoint shapes or permissions. The
implemented P2 shapes in section 2 override conflicting draft authentication,
response-envelope and timestamp proposals elsewhere in this document.

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
V3 authentication and authenticated placeholder refusals are implemented as
described in section 2; operational management endpoints remain unavailable.

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

## 2. Authentication — implemented unified users, sessions and permissions

### Authority and protected local commands

By binding owner decision, the OS account running the local CLI is the owner for
these commands. They require a running `serve` and its protected native control
channel; there is no offline direct-state fallback. Windows uses current-token-SID
named-pipe DACLs and server-SID checking; Linux uses owner-only sockets and
bidirectional peer-UID checks. Same-account processes are trusted. This is not P3
process isolation, permanent SID/UID enrollment or supervisor recovery. A pack
password, Minecraft proof/receipt, OP status, loopback/LAN, process ownership,
configuration possession or mere bearer possession never appoints an administrator.

```text
vesta-agent pair <name> [--install-root <root>]
    [--role owner|admin|administrator|maintainer|moderator|viewer|pack-user]
    [--expires <minutes>]
vesta-agent devices list <name> [--install-root <root>]
vesta-agent devices revoke <device-id> <name> [--install-root <root>]
vesta-agent owner link <name> --minecraft-uuid <uuid>
    [--role <same built-in roles>] [--install-root <root>]
vesta-agent owner unlink <name> --minecraft-uuid <uuid>
    [--revoke-devices] [--install-root <root>]
vesta-agent owner list <name> [--install-root <root>]
vesta-agent access show <name> [--install-root <root>]
vesta-agent access set <name> [--install-root <root>]
    [--anonymous-role <role-id>] [--new-user-role <role-id>]
    [--whitelist-role <role-id>] [--whitelist-sync on|off]
    [--password-role <role-id>]
```

`<name>` selects an existing server registry entry, not a person. Root selection is
`--install-root`, then `VESTA_SERVER_ROOT`, then local `vesta-servers`. `admin` aliases
`administrator`; link/pair default to `owner`. Pair lifetime defaults to 10 minutes
and accepts integral 1–60 minutes. There are at most 32 outstanding codes and 32
owner links; canonical enrollment is bounded to 1000 registered device keys,
including revoked keys (no automatic deletion or eviction). Public registration
uses at most 968 slots, reserving 32 for explicit local pairing/owner links or
authenticated security managers, so unauthenticated registration cannot exhaust
local owner recovery admission. Expired codes are pruned on issuance; capacity refuses instead of
silently evicting a live code. Device lists are bounded to 1000 entries; larger
inventories refuse pending pagination. `access set` requires at least one setting,
rejects duplicates/unknown flags, and validates every automatic role before its
transactional audited update. Show/set return the canonical source settings as JSON.

Pair prints one JSON object with `code`, `server_id`, `role`, `expires_at_ms`. Only a
server/purpose-bound SHA-256 digest of the random 32-byte code is persisted. A code
survives restart until consumed/expired. Redemption consumes the code, creates or
attaches the device to its canonical user, assigns the locally selected role,
checks current identity and appends success audit in one immediate transaction.
Concurrent redemption has one committed winner. Audit/infrastructure failure rolls
back redemption; an uncertain network outcome must not be blindly retried. To
attach pairing to an existing user/key, supply that user's valid device session.
Knowledge of someone else's public key is not authentication and cannot merge users.

Owner links are server-scoped explicit role sources. Linking/replacing also updates
an already mapped canonical UUID user, not just future enrollments. Default unlink
removes the enrollment link but preserves the user's existing explicit role and
its devices. Because roles belong to the user, later devices authenticated as that
same user inherit preserved authority. `--revoke-devices` instead revokes **all**
devices of that UUID's canonical user, including subsequently attached/pairing
devices, and revokes its link-derived role assignment. It works after a previous
unlink and prevents fresh UUID enrollment from restoring that assignment. Other
explicit local grants are not silently removed. Independent device revoke remains
available. Local mutations and successful enrollment/session issuance are audited.

`owner link` returns `linked`, `minecraft_uuid`, `role`, `server_id`, and
`tls_sha256_fingerprint`: the live transport leaf-DER SHA-256 from the same reloadable
material as discovery's `x-vesta-tls-sha256-fingerprint`. It is null for local-only
serve. First-contact out-of-band comparison, TOFU/pinning and TLS verification are
launcher responsibilities. A renewable transport certificate is not server identity
or administrative authority.

### Permissions model

There is one canonical user/principal per verified Minecraft UUID per server and
one device-session store. Multiple device keys and authentication methods attach
to that user; a device signs in once, then acts as its user. Anonymous registration
cannot determine a person's identity. Existing-user attachment therefore requires
an authenticated session or a fresh proof for that already mapped UUID; conflicting
UUID/user/key bindings and silent account merges are refused.

Access is capability-based. `public.info` controls discovery/status;
`pack.download` controls archives/download tickets; `pack.realtime` controls realtime
tickets/subscriptions. Existing explicit `pack.access` is a compatibility alias
for download/realtime, not a second authentication domain. Owner/administrator/
pack-user canonical roles include the new public/pack capabilities. Management
still requires the exact management capability; a session identifies, not authorizes.

Canonical configurable role sources are:

| Source | Default | Lifetime and scope |
| --- | --- | --- |
| Anonymous | `public` | Live role selection; public info only by default |
| New user | `public` | Safe role captured on user creation; role capabilities resolved live |
| Whitelist sync | enabled, `player` | Fresh protected admission-file reads before every network decision; actual whitelist membership, online mode and no active ban; can disable/retarget |
| Correct password | `player` | Chosen safe role bound to that device and password revision |
| Local pairing/owner link | explicit selected built-in role | Canonical user role; only the protected local owner establishes it |

V12 seeds separate safe default roles even when existing custom names or IDs
collide: names/IDs receive unused suffixes, existing roles and assignments remain
unchanged, and canonical source settings reference the newly seeded roles. A
malformed or unreadable current admission document disables the whitelist source
for that decision; cached admission never prolongs it. Independent explicit,
password and public-role sources remain available.

`public` contains only `public.info`. `player` contains `public.info`,
`pack.download`, `pack.realtime`. All four automatic source roles are validated to
contain **only public/pack capabilities and all-resource scopes**. An owner,
management-capable custom role or even legacy `pack-user` (which has `status.view`)
is refused as an automatic role. Roles are checked again at use, so later unsafe
role edits fail closed. Explicit capability denies dominate automatic sources.
Whitelist sync uses the actual current list even when Minecraft's `white-list`
property is off; a successful proof for an unlisted player is identity, not access.
Password rotation/revocation invalidates derived grants and pending tickets/live
subscriptions, not the user's sign-in or independent explicit role. The current
password revision is rechecked in the canonical transaction. Password grants are
not shared with another device of the same user.

### Implemented HTTPS wire shapes

Requests are strict JSON: unknown fields, cookies, duplicate authorization headers
and query credentials are refused. Use `Content-Type: application/json` and one
`Authorization: Bearer <token>` field when authenticated. Public POST sign-in/
enrollment paths do not require an existing session; password attachment does.

| Endpoint | Request | HTTP 200 response |
| --- | --- | --- |
| `/api/v3/auth/device/enroll` | `{"device_name":"…","public_key":"…"}` | `{"device_id":"…"}` |
| `/api/v3/manage/pair` | `{"code":"…","device_name":"…","public_key":"…"}` | `{"server_id":"…","principal_id":"…","device_id":"…","role":"owner"}` |
| `/api/v3/auth/minecraft/challenge` | `{"uuid":"…","name":"…"}` | `{"challenge":"…","server_hash":"…","expires_in":60}` |
| `/api/v3/auth/minecraft/complete` | `{"challenge":"…","device_name":"…","public_key":"…"}` | `{"device_id":"…"}` |
| `/api/v3/auth/session/challenge` | `{"device_id":"…"}` | `{"server_id":"…","nonce":"…","expires_in_seconds":60,"purpose":"vesta.management.session.v1"}` |
| `/api/v3/auth/session` | `{"device_id":"…","nonce":"…","signature":"…"}` | `{"server_id":"…","token":"…","expires_at_ms":1791116100000}` |
| `/api/v3/auth/password` | `{"password":"…"}` plus device bearer | `{"granted":true}` |

The corresponding `/api/v3/manage/{device/enroll,minecraft/challenge,minecraft/complete,
session/challenge,session,password}` paths are compatibility aliases for exactly
the same handlers/store. Enrollment may supply an authenticated bearer to attach
another device or previously unmapped UUID to that user. Both linked and unlinked
Minecraft completions return only `device_id`; responses do not disclose owner-link
existence. There is no HTTP owner-link or role-source mutation endpoint.

Keys are exactly 32 Ed25519 bytes, signatures 64 bytes, in canonical unpadded
base64url. Codes/nonces/tokens are 32 random bytes (43 base64url characters).
Device labels use the existing 64-scalar bound. Expiration is signed epoch
milliseconds, not the later draft's RFC3339 convention. Server-authenticated HTTPS
and Ed25519 proof of possession do not require client certificates.

Minecraft enrollment reuses the bounded join verifier: proof checks UUID, name,
server, purpose, freshness and single use. V2 pack-proof and V3 enrollment challenge
hash domains differ; cross-purpose completion consumes/refuses before provider I/O.
The shared verifier limits are 1024 pending/active challenges, two active
verifications, 60-second TTL and bounded issue/verification rates. Tests use the
existing fake provider, not real Minecraft/Mojang. UUID proof finds/creates the
canonical user and enrolls its key; only a current explicit local owner link gives
management authority. Unlinked users receive their safe defaults/live pack sources.

Sign exactly these UTF-8 bytes, with two NUL separators and no trailing NUL/newline:

```text
vesta.management.session.v1\0<canonical server_id>\0<canonical base64url nonce>
```

The purpose string is retained as a compatibility/domain-separation identifier; it
is the **single** user/device session for both management and pack use. Wrong key,
server, purpose, signature, unknown/expired/replayed nonce or revoked identity
refuse. A recognized attempt consumes the nonce even when signature/device/capacity/
audit admission fails. Issuance rechecks identity and audits before returning.
Challenges are bounded to 1024, sessions to 4096, session lifetime to 15 minutes.
Both wall and monotonic deadlines apply. Only server/purpose-bound credential hashes
are retained in process-local memory; restart ends sessions/challenges.

Downloads and realtime now resolve this same session against **current** role
sources. There is no independent pack-session store. Bounded, hash-only,
single-use download/realtime action tickets are purpose-bound and no longer lived
than their session. Redemption and realtime push/heartbeat recheck live identity,
roles, whitelist/bans and password revision. A ticket is never a device bearer or
management credential. Configuring anonymous `player` deliberately opens pack
access; the default anonymous `public` does not.

Deprecated V2 thin shims remain: `/api/v2/auth/password` verifies the password and
requires a canonical device bearer, then attaches the device role and echoes that
**same** session (`session`, `expires_in:900`, `granted:true`), never mints a pack
session. `expires_in` is the maximum session lifetime, not a renewal. Correct
password without a device session receives 401 `device_session_required`.
V2 Minecraft completion requires `device_name`/`public_key` with a valid proof,
returns `device_id`, and uses the canonical UUID user; clients then use signed
session issuance. V2 status/artifact/ticket/realtime endpoints consume canonical
sessions or explicitly configured anonymous capabilities.

### Capability-checked placeholders and fixed errors

Other paths at/below `/api/v3/manage` require a valid device session and live
`status.view` as the current placeholder admission gate. They perform no operational
effect: denied capability returns 403 `management_forbidden`, admitted placeholders
return fixed **501** `not_implemented`. Future handlers must use their exact
capability/resource in their own mutation transaction and before durable queued
steps. Wrong methods on public-auth paths are not authentication exceptions.
V2 management remains 403 `management_unavailable`; V1 remains 410
`legacy_endpoint_removed`, including rate exhaustion.

Implemented failures retain small fixed `{"error":"fixed_code"}` responses, not
section 3's draft envelope. Pair refusal is 401 `management_pair_invalid`; invalid
session/nonces/identity are 401 `management_session_invalid`; denied management is
403 `management_forbidden`; malformed input is 400 `invalid_request`; capacity/state
failure is fixed 503 `management_saturated`/`management_unavailable`. Pack denial
retains 401 `pack_session_invalid`; password refusal is 403 `pack_access_denied`.
Verifier errors retain fixed `minecraft_*` responses. All handlers reuse bounded
bodies, timeouts, blocking admission and the peer-IP limiter (60 requests/minute,
1024 peers), with 429 `peer_rate_limited`. Secrets, SQL, rejected values and provider
diagnostics are never reflected in errors.

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
