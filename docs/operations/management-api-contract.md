# Remote management API contract — DRAFT 0.1

**P2 authentication and operational management in sections 2.1–2.2 are implemented.**
This includes real deterministically redacted console text/streaming, durable
console command delivery, authorized pack publish/minimum mutations and bounded
game-root files/provider installs/server.properties editing. Sections
3 onward remain draft proposals except where sections 2.1–2.2 freeze an implemented
shape. This is not completion of every P3 supervisor/release gate. Implemented
shapes override conflicting draft envelopes, timestamps, generations, redaction
and endpoint proposals.

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
described in section 2; the implemented operational subset is specified in sections 2.1–2.2.

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

Pair prints one JSON object with `code`, `server_id`, `role`, `expires_at_ms`,
`tls_sha256_fingerprint` and `pairing_link`. The fingerprint uses the same live
reloadable leaf-DER SHA-256 source as `owner link`; it is null for local-only serve.
Optional `[listener] public-endpoint = "https://host.example:8443/"` advertises the
HTTPS base URL (default absent; max 2048 bytes; no userinfo, query or fragment).
It is never inferred from a bind address or request header. When both endpoint
and TLS fingerprint are available, `pairing_link` is
`vesta://pair?endpoint=<percent-encoded HTTPS URL>&code=<code>&fp=<lowercase SHA-256 hex>&sid=<server_id>`;
otherwise it is null. Query values are percent-encoded exactly once. This link
contains a pairing secret; handle it like the code, never as proof of authority.
Only a
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

### Authenticated self projection

`GET /api/v3/manage/session` accepts no body or query and requires only a valid
canonical device session, including pack-only users. The POST compatibility alias
for session issuance is unchanged. Success uses the standard V3 envelope:

```json
{"api_version":"3","request_id":"request-1","data":{"server_id":"server-uuid","principal_id":"user-uuid","device_id":"device-uuid","roles":[{"id":"public","name":"Public","rank":1}],"capabilities":["public.info"],"session_expires_at_ms":1791116100000}}
```

Roles include current explicit and applicable safe automatic sources, deduplicated
by ID. Capabilities use the live authorization policy, including explicit denies,
expired grants, current whitelist/bans and password revision. A scoped capability
is listed only when at least one resource passes the same live evaluator; covering
denies remove it. Presence never implies access to every path: the flat list
does not describe scopes, and each operation still checks its exact resource. This is
an informational UI hint, never authority for an effect. No credentials, keys or
audit entries are emitted by the read. Invalid/revoked/expired sessions return the
V3 401 `management_session_invalid` envelope.

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
bodies, timeouts and blocking admission. Anonymous/invalid bearer traffic uses a
peer-IP token bucket (1024 tracked IPs); valid device sessions use a separate
per-session bucket (4096 tracked sessions) shared across peer addresses and routes.
Only a syntactically valid, known, unexpired bearer that passes live identity
validation selects that bucket. Revoked/unknown/duplicate/malformed bearers cannot
bypass the anonymous budget. Route authority checks remain independent. The preliminary in-memory lookup is
nonblocking: store contention applies anonymous accounting and returns a capacity
refusal (503, or 429 if that anonymous bucket is exhausted), never admission to
the larger session budget.

TLS `[listener]` accepts optional integer settings (each bounded to 1-60000):
`peer-requests-per-minute` defaults to **60**, `peer-connections-per-minute` to
**120**, and `session-requests-per-minute` to **300**. Limits are token buckets
with an initial burst equal to the configured rate and continuous refill over 60
seconds. Connection admission remains per socket peer IP, before TLS, including
valid sessions; exhausted connection admission drops the socket. Reuse connections.
Active budgets are never evicted to admit new keys; full maps return 503.

Every HTTP 429 includes `Retry-After` as positive integer delta-seconds rounded
up to the next token. Operational routes (including GET session) use the V3
`rate_limited` envelope; compatibility auth/pack routes retain `peer_rate_limited`
(or their existing auth-specific code). No authority is conveyed by rate admission. Secrets, SQL, rejected values and provider
diagnostics are never reflected in errors.

## 2.1 Operational management — implemented wire contract

All paths below are relative to `/api/v3/manage`. Every endpoint requires exactly
one valid canonical device bearer and its listed current capability. Pack-only
and anonymous users cannot operate this surface. No owner-link, role-source,
role-management or pairing-code creation HTTP route has been added.

Success is `{"api_version":"3","request_id":"request-N","data":...}`;
errors are `{"api_version":"3","request_id":"request-N","error":{"code":"..."}}`.
Request IDs are bounded process-local correlation labels, not credentials or
persistent globally unique IDs. Operational errors are fixed: 400 `invalid_request`
or `confirmation_required`; 401 `management_session_invalid`; 403
`management_forbidden`; 404 `resource_not_found`; 409 `idempotency_conflict`,
`confirmation_invalid`, `operation_conflict`, `cursor_gap`; 412
`precondition_failed`; 413 `request_too_large`; 429 `rate_limited`; 503
`temporarily_unavailable`. Authentication aliases and future placeholders retain
section 2's legacy errors. Cookies, query credentials, unknown fields, duplicate
bearer/key headers, non-object mutation JSON and bodies on GET are refused.
No client executable, argv or URL is accepted as authority. Section 2.2 accepts
validated game-root-relative resources and optimistic SHA-256 validators; neither
grants authority or selects an agent/native executable.

### Conditional status and push updates

The authenticated server info projection additionally reports `backups_configured`
from the active backup service, `join_address` from trusted `[pack.join]` (IPv6
bracketed), and `game_port` from readable, valid game properties. Null join
information never means the HTTPS agent port is a Minecraft port. Clients may
show an explicitly inferred endpoint-host/game-port fallback when no configured
join address exists. Backup setup remains a trusted local operation; remote
backup capabilities alone do not mean a repository has been configured.

`GET /players` seeds one generation-scoped roster with the bounded `list` driver,
then reconciles complete captured join/leave events on subsequent reads and status
observations. A generation change or capture gap requires a new seed. Truncated,
incomplete or hidden-name lists stay unknown. Metrics collection consent is not
required for this current management view. Status adds nullable decimal-string
`player_roster_revision`, changing on roster activity (including count-neutral
join/leave pairs). Clients can read names once on attachment and again when that
revision changes; ordinary reads do not send more console commands. Modern vanilla
`System chat: There are ...` feedback is recognized only inside the anchored server
INFO log envelope. Sleep uses the same current roster plus an independent live
status ping and its existing exact-console-cursor stop guard; failed observations
retain retry pacing.

`GET /api/v3/manage/server/status` returns a strong `ETag` of the status data.
`If-None-Match` accepts one strong quoted validator, as `/metrics/current` does;
weak validators, wildcard, lists, duplicate fields and malformed values return
400. A match returns 304, the same ETag, `Cache-Control: no-store`, and no body
or Content-Length. Authentication and `status.view` are checked before a match.
The 200 envelope's bounded `request_id` is `status-<SHA-256 hex>`, stable for an
unchanged representation so the validator identifies byte-identical JSON.
Changes to any field, including uptime seconds, change the validator.

The existing `/api/v3/manage/metrics/stream` adds this message immediately after
`metrics.snapshot` and whenever its latest observed status projection changes:

```json
{"type":"status","api_version":"3","etag":"\"status-<64 lowercase hex>\"","data":{"state":"stopped","lifecycle":"offline","generation":null,"player_count":null,"player_count_known":false,"player_count_observed_at_ms":null,"uptime_seconds":null,"supervision":"in_process","supervisor":null,"readiness":"stopped","readiness_reason":null,"readiness_evidence":"log_only"}}
```

`data` is exactly the HTTP status data; `etag` is the HTTP validator including
quotes. Status observation runs at most every 250 ms independently of metrics
sampling, even when telemetry is disabled. Delivery is latest-only: intervening
changes may coalesce, slow writers have the existing bounded send deadline, and
no event queue grows. Each status message is capped at 4096 UTF-8 bytes. The
existing socket budget is unchanged. The stream still requires `metrics.view`;
status messages additionally require current `status.view` on connect and before
each push. A metrics-only subscriber receives no status; revoking `status.view`
withholds future status frames while retaining authorized metrics delivery.
Clients can ignore the additive `status` type. No extra public information is added.

### Exact endpoints and data shapes

`Operation` is the safe projection below. Input/result/log text is not exposed:

```json
{"operation_id":"opaque-id","state":"queued","progress_percent":0,
 "cancel_requested":false,"created_at_ms":1791115200000,
 "updated_at_ms":1791115200000,"finished_at_ms":null}
```

States: `queued`, `validating`, `awaiting_confirmation`, `running`, `cancelling`,
`succeeded`, `failed`, `cancelled`, `recovery_required`. Terminal state represents
outcome; detailed engine result/error payloads are not projected. Timestamps are
integer epoch milliseconds. Generations are opaque canonical nonzero 32-character
lowercase hexadecimal identities, not PIDs or decimal counters. Stopped generation
is null; persistent monotonic stopped-control generations are not introduced.

| Method / path | Exact request | Exact `data` / status | Capability |
| --- | --- | --- | --- |
| GET `/server` | no body/query | 200 `{server_id,name,agent_version,minecraft_version,pack_version,configured_mode:"same_account",backups_configured:boolean,join_address:string-or-null,game_port:integer-or-null}`; other fields strings | `status.view` |
| GET `/server/status` | no body/query | 200 `{state:"stopped\|running\|stopping\|draining",generation:hex-or-null,player_count:integer-or-null,player_count_known:boolean,player_count_observed_at_ms:integer-or-null,uptime_seconds:decimal-string-or-null,readiness:"stopped\|starting\|ready\|degraded",readiness_reason:"startup_timeout\|probe_failed\|fatal_output\|console_gap"-or-null,readiness_evidence:"log_and_probe\|log_only",supervision:"in_process\|separate",supervisor:null-or-{orphan_running,launch_unresolved,observation_failed,crash_restart_enabled,crash_loop,restart_state_unsaved:boolean,recent_crashes:integer,restart_pending_ms:decimal-string-or-null}}`. Readiness and crash-restart semantics: operations/supervisor.md | `status.view` |
| POST `/lifecycle/start` | `{}` + `Idempotency-Key` | 202 `Operation` | `lifecycle.start` |
| POST `/lifecycle/stop` | `{"expected_generation":"<hex>"}` + key | 202 `Operation` | `lifecycle.stop` |
| POST `/lifecycle/restart` | same generation request + key | 202 `Operation` | `lifecycle.restart` |
| POST `/lifecycle/kill` | `{"expected_generation":"<hex>","acknowledgement":"kill game:generation:<same hex>"}` + key; final submission adds `confirmation_token` string | preparation 202 `{operation_id,state:"awaiting_confirmation",confirmation_token,expires_at_ms}`; final 202 `Operation` | `lifecycle.force_kill` |
| GET `/operations/{id}` | no body/query | 200 `Operation`, 404 if absent | `tasks.view` |
| GET `/operations` | optional `limit`, `cursor` | 200 `{items:[Operation],next_cursor:string-or-null}` | `tasks.view` |
| GET `/console` | optional `limit`, `cursor` | 200 `{items:[{cursor,text:string,truncated:boolean}],next_cursor:string,latest_cursor:string}` | `console.read` |
| GET `/console/stream` | native WebSocket upgrade, bearer, no body/query/cookie/Origin | 101; availability, line and gap messages below | `console.read` |
| POST `/console/command` | `{"expected_generation":"<hex>","line":"say hello"}` + key | 202 `Operation` | existing `console.write` |
| POST `/players/moderate` | closed action-tagged request below + `Idempotency-Key` | 202 `Operation`; OP prepares an exact confirmation token | `players.moderate`, or `players.operator` for OP/de-OP |
| GET `/players/moderation` | optional scoped cursor, limit 1–100 | 200 `{items:[ModerationHistory],next_cursor}` | `players.view` |
| GET `/players/bans` | no body/query | 200 `{items:[{name,uuid,reason,created,expires,source}]}` | `players.view` |
| GET `/players/ops` | no body/query | 200 `{items:[{name,uuid,level,bypassesPlayerLimit}]}` | `players.view` |
| GET `/players/whitelist` | no body/query | 200 `{items:[{name,uuid}]}` | `players.view` |
| GET `/players` | no body/query | 200 unknown `{known:false,player_count:null,items:null}`; known `{known:true,generation:hex,player_count:integer,items:[{name:string}]}` | `players.view` |
| GET `/metrics/current` | no body/query; optional single strong `If-None-Match` | 200 latest `{sample,columns,status}`; exact matching revision ETag returns authorized 304; wildcard/list/weak/malformed validators return 400 | `MetricsView` |
| GET `/metrics` | `series` (1–8 unique closed names), `from`, `to`; optional `resolution=auto\|raw\|5m\|daily`, `max_points=1..2000`, `after` revision and single strong `If-None-Match` | 200 changed columnar buckets with shared timestamps/cursor; `after` equal to current cursor returns empty columns with 200; exact matching ETag may return authorized 304, stale ETag returns 200; invalid validator forms return 400. Inclusive span ≤5 years (raw ≤7 days); `retention_gap` signals full refresh | `MetricsView` |
| GET `/metrics/policy` | no body/query | 200 effective host/stored policy, revision and explicit console-write warning | `ConfigView` |
| PUT `/metrics/policy` | `{expected_revision,settings,command_polling?}` | 200 stored revision; optimistic conflict 409 `metrics_policy_conflict`; audited | `ConfigEdit` |
| GET `/metrics/stream` | Native WebSocket upgrade, bearer, no body/query/cookie/Origin | 101; bounded latest-only metrics snapshot/sample/gap and separately authorized player activity and status notices | `MetricsView`; status also requires `status.view` |
| GET `/players/activity` | optional numeric `after` event ID; optional `limit` 1–100 | 200 bounded ordered activity events and numeric `next_after` cursor (or null); event page may include observed join/leave names | `PlayersView` |
| GET `/pack` | no body/query | 200 `{hosted_revision:decimal-string-or-null,minimum_revision:decimal-string-or-null,status:"published\|unpublished"}` | `status.view` |
| POST `/pack/publish` | `{"expected_revision":"0"}` + key; zero means unpublished | 200 `{outcome:"succeeded",hosted_revision:decimal-string,minimum_revision:decimal-string}` | `pack.publish` |
| POST `/pack/minimum` | `{"expected_revision":"2","minimum_revision":"2"}` + key | same synchronous 200 outcome shape | `pack.force_update` only |
| GET `/audit` | optional `limit`, `cursor` | 200 `{items:[{cursor,at_ms:integer,action:string,outcome:string}],next_cursor:string-or-null}` | `audit.view` |

Metrics configuration, closed series, collection boundaries, retention, storage-budget
semantics, unknown values, activity parsing and exact query rules are specified in the
[F35 metrics contract](metrics.md). Metrics HTTP read routes never issue a game command
or list players to derive online counts. Separately, the background sampler may issue
receipt-gated tick commands only under explicit stored `command_polling.enabled=true`
consent; that source writes to the game console and remains unknown for unsupported or
external launches. Metrics routes never expose daily player identities.

### Mutation ownership and idempotency

Mutations require 16–128 visible-ASCII keys, scoped to canonical
server/principal/device/POST/path. Lifecycle and console commands use the durable
operation engine. Current policy, canonical intent, idempotency association and
success audit commit together; authority is checked again immediately before the
native effect. Exact retries return the same operation, including after a failure
or generation exit. Changed intent returns `idempotency_conflict`.

Kill preparation cannot kill. The server derives the exact target and issues the
existing 120-second generation/actor/device-bound receipt. Repeat preparation
supersedes the prior token without changing operation ID. Final submission uses
the same endpoint/key/target. Receipt consumption, queue transition and success
audit/event commit together. Stop never force-kills. Restart retains one claim and
resource lock through graceful write, actual exit and all I/O joins, and rechecks
policy before replacement launch. Failed stop never launches a replacement.
No automatic restart or world restore is added.

Console command lines are nonempty UTF-8, at most 1,024 bytes, and contain no
Unicode control characters (including CR, LF and NUL). They are never shell or OS
commands. Fresh admission requires the exact running incarnation; a stopped or
stale target is 412. The existing bounded, exclusively owned stdin writer runs
the fresh operation guard immediately before writing and retains its receipt and
claim until acknowledgement. `succeeded` means the line was sent to stdin, not
that Minecraft accepted/executed it. Partial/lost native write proof is
`recovery_required`, not a retry permit. A dedicated `console.command` audit
records the fixed terminal outcome, never command text. Canonical command text is
retained privately as operation input under existing engine retention/purge rules,
so commands should not be used to transmit secrets unnecessarily.

Pack publish prepares only the configured local scanned/archive/platform source,
using the existing authorized prepared-publish path. It accepts no source choice,
path, URL, hash, provider resolution, or lower-version override. Scanned sources
use an explicit empty provider registry and retain the configured unresolved-file
policy: unresolved files refuse unless the operator configured a permitted
alternative. A stale revision is 412. Publication, current policy, its existing
success audit and the exact-key outcome record commit in the same writer. Minimum
updates monotonically raise the server compatibility policy within the hosted
revision; they do not rewrite immutable release artifacts/history and do not
require `pack.publish`. Policy, minimum mutation, durable outcome and success audit
commit together. Both pack endpoints return synchronous durable outcomes, not
background task IDs; retries do not republish or append another success audit.
Revision strings are canonical decimal values in 0..9007199254740991 (minimum must
be positive and cannot decrease). Retention remains existing operation/input and
pack ledger retention, with no new 90-day promise.

### Console content, deterministic redaction and streaming

Authorized readers receive actual captured text. Complete credentials and paths
use deterministic known-material matching. Exact known configured absolute
base/state/config/launch/protected paths, in forward- and backslash forms, are
replaced longest-first by `[redacted]`. Matching is literal and case-sensitive.
Currently retained agent-issued management session tokens, challenge nonces and unexpired local pairing codes
are recognized by their server-bound hashes and replaced by `[redacted]`, without
retaining additional plaintext credentials. Password plaintext is deliberately
not retained by the agent, so arbitrary user-entered passwords, third-party
secrets, unknown/expired credentials and differently rendered paths cannot be
reliably identified by this rule. `console.read` is consequently a powerful grant
for real server logs, not a promise that arbitrary game output is secret-free.
For live in-process session tokens and challenge nonces, issuance also registers
process-keyed HMAC fingerprints of their 1–42 byte prefixes and suffixes. A line
edge is masked only when it matches an actual registered fragment; ordinary
Minecraft words, versions and ports remain readable. Registration ends when the
credential is consumed, removed or pruned. The reference-counted index is bounded
by the live challenge/session limits, and each line uses at most two indexed
lookups, without retaining additional plaintext tokens. Both sides of physical
newline splits and truncated line suffixes are covered independently of paging
and streaming. Durable pairing codes retain full-token matching only: their
hash-only records cannot identify arbitrary incomplete fragments after restart.
Interior content and the original ring's truncation flag are preserved. HTTP and
WebSocket use the same projection, and JSON escapes the resulting text.

The existing capture limits remain 16,384 bytes per line, 10,000 retained lines,
and 2 MiB of retained text. HTTP pages max 200 lines. Console continuation eviction
returns `cursor_gap`. The stream retains one cursor and at most one 16-line sampled
page, not an accumulating subscriber queue. It reuses owned workers/tasks and the
32-global/two-per-device-session socket budget shared with pack realtime. Inbound
frame/message ceilings remain 1,024 bytes; the outbound write buffer is bounded to
128 KiB to permit one escaped retained line. Each send has a two-second deadline;
slow sends disconnect. Current session/identity/console-read authority is checked
at every 250-ms sample. Initial delivery includes the most recent 16 lines;
subsequent pages follow the last delivered cursor. Eviction emits a gap notice
before the available retained lines. Exact messages:

```json
{"type":"console.available","api_version":"3","cursor":"<scoped-cursor>"}
{"type":"console.line","api_version":"3","cursor":"<line-cursor>","text":"actual redacted text","truncated":false}
{"type":"console.gap","api_version":"3","cursor":"<sample-continuation>"}
```

Native clients must service Ping/Pong and reconnect after idle/authorization
closure. No management ticket-minting route is introduced.

### Collections, observations and audit

Collection defaults are 50; operations max 100, console/audit max 200. Cursors are
bounded server/surface-scoped continuation references, maximum 512 bytes, treated
as opaque by clients. Operations use live pages, not retained expiring snapshots.
Console/audit start at a bounded retained tail. Audit exposes only cursor,
timestamp and allowlisted action/outcome labels, with `other` for unrecognized
labels; raw actors, source, targets, metadata, hashes and payloads are withheld.

Players seeds through the bounded live `list` driver with `players.view` checks at
admission, write and response, then maintains the generation-scoped roster from
complete captured activity. Stopped/unavailable/timed-out queries are unknown,
never fabricated zero. Names outside the Minecraft ASCII-name projection are
withheld. Status reconciles current retained activity and reports count/time and
`player_roster_revision`; a gap makes the observation unknown until reseeded. No
roster carries into a new/stopped incarnation. Status itself never issues a player
query. Uptime measures in-process monotonic incarnation observation, not readiness.

Remaining gaps: detailed operation result/error projection, retained expiring
pagination snapshots, persistent stopped-control generations and paginated player
collections. Section 2.2 implements a bounded files/content/server-properties
subset. Other unimplemented management paths remain authenticated 501
placeholders. F34 separately appends players.operator for owner/administrator;
no new authority is admitted into automatic role sources. TLS inert-child tests cover start → running → graceful stop → stopped,
confirmed kill, command delivery, real redacted HTTP/stream content, known players,
and durable pack publish/minimum outcomes.

## 2.2 Implemented game-root files, provider installs and server.properties

Only the exact routes below are added to the implemented subset; unrelated
management placeholders stay authenticated 501. All requests require a canonical
**device session** and current existing capabilities. Pack passwords, pack-only
roles and anonymous access grant no administrative file authority. No new
capabilities or automatic-role grants are introduced.

JSON uses section 2.1's success/error envelope. File-content GET is the explicit
binary-success exception. All mutations require exactly one `Idempotency-Key`
(16–128 visible ASCII bytes). JSON must be objects with no unknown fields and
`Content-Type: application/json`. POST/document PATCH bodies are at most 65,536
bytes, also subject to the configured listener body ceiling. Unknown/duplicate
query keys, cookies, query credentials and GET bodies are refused.

### Resources and validators

Paths are portable ASCII `RelativePath` resources confined to the configured
**game root**, never native/absolute paths. Traversal, backslashes, drive/UNC
paths, ADS, Windows device/trailing-dot aliases, links/reparse points/hardlinks
and protected state/config/TLS paths are refused. Exact resolved resources must
pass live scope policy. Generic file/upload paths and restored original paths
also refuse components named `server.properties`, `whitelist.json`,
`banned-players.json`, `banned-ips.json`, `ops.json` (case-insensitive), preventing
structured-admission confirmation bypass. This exclusion is enforced against
submitted paths, canonical resolved resources and opened-handle resources, so
Windows 8.3 short names and case variants cannot bypass it. It applies to reads,
all mutations, uploads (including staged finish), rename source/destination,
trash restore and provider install targets. The structured properties editor
has a narrow exception for the exact root `server.properties` resource only,
not nested properties files or other admission documents.

Create requires **`If-None-Match: *`**; overwrite/delete requires
**`If-Match: <sha256>`** (unquoted exact lowercase 64-character hex). Missing,
weak/quoted/multiple/simultaneous conditionals return 400 `invalid_request`.
Create never upserts; rename/restore never replace. Stale/existing-target
preconditions return 412 `precondition_failed`, with `error.current_sha256`
(digest or null). Hashes are optimistic validators, never authority. Responses
contain only relative resources, never native roots/state paths.

### Exact shapes and capabilities

Paths are relative to `/api/v3/manage`. Byte sizes/offsets are canonical unsigned
decimal **strings**; hashes are lowercase hex. `Change` is
`{outcome:"succeeded",sha256,size,trash_id}`, where `trash_id` is null except for
an overwrite's recoverable original. All synchronous mutations return 200.

| Method / path | Exact request | Exact `data` / status | Existing capabilities |
| --- | --- | --- | --- |
| GET `/files` | Optional `path`, `cursor`, `limit`; absent/empty path selects root | 200 `{items:[{name,kind:"file"\|"directory",size,modified_at}],next_cursor}` | `files.list` on directory and visible children |
| GET `/files/content` | Required `path`; optional single `Range` and `If-Range`, no body | 200 legacy full body or 206 raw octet-stream, headers below | `files.read` exact file |
| GET `/files/tail` | Required `path`; optional `lines` (default 100, max 1000) | 200 `TextPage` below | `files.read` exact file |
| GET `/files/lines` | Required `path`; optional `from_byte`, `limit` (default 100, max 1000), `direction:"forward"\|"backward"` | 200 `TextPage` below | `files.read` exact file |
| GET `/files/follow` | Native WebSocket upgrade; required `path`, optional byte `cursor`, bearer; no body/cookie/Origin | 101; byte-preserving file notices below | `files.read` exact file |
| PUT `/files/content` | Required `path`, raw bytes, create/overwrite conditional | `Change` | `files.create` or `files.overwrite` exact path |
| POST `/files/uploads` | `{path,total_size,sha256}`, create/overwrite conditional | `{upload_id,offset:"0"}` | Same destination create/overwrite authority |
| PUT `/files/uploads/{id}` | Required `offset` query, raw chunk; no conditional | `{offset}` actual staged length | Original upload create/overwrite authority, same principal/device |
| POST `/files/uploads/{id}/finish` | `{}`, no query/conditional | `Change` | Same upload authority, checked again at final effect |
| DELETE `/files/uploads/{id}` | Empty body, no query/conditional | `{outcome:"succeeded"}` | Same upload authority and subject |
| DELETE `/files` | Required `path`, If-Match, empty body | `{outcome:"succeeded",trash_id}` | `files.delete` exact file |
| GET `/trash` | Optional `cursor`, `limit` | 200 `{items:[{id,path,sha256,size,trashed_at_ms,expires_at_ms}],next_cursor}` | `files.read` original paths; only this principal/device's unprotected entries |
| POST `/trash/{id}/restore` | `{}`, no query/conditional | `Change`, null trash_id | `files.read` and `files.create` original path, same principal/device owner |
| POST `/files/mkdir` | `{path}`, no query/conditional | `{outcome:"succeeded"}` | `files.mkdir` exact new directory |
| POST `/files/rename` | `{from,to,expected_sha256}`, no query/conditional | `Change`, null trash_id | `files.rename` both paths |
| POST `/content/install` | `{provider:"modrinth"\|"curseforge",project_id,version_id}`, no query/conditional | `Change`, null trash_id | `content.install` and exact resolved `files.create` |
| GET `/config/server-properties` | No body/query/conditional | 200 `{values:{key:typed_value},sha256}` | `config.view` plus `files.read` server.properties, one read snapshot |
| PATCH `/config/server-properties` | `{changes:{key:typed_value},expected_sha256,confirmation_token?}`, no query/conditional | `Change` plus `restart_needed:true`; weakening preparation 202 below | `config.edit`, `files.read`, `files.overwrite` server.properties |

The legacy GET without Range stays bounded/buffered to **1,048,576 bytes**;
larger files still return 413. Its unchanged raw bytes, `X-Vesta-Sha256: <hash>`
and quoted content-SHA-256 `ETag` remain compatible. It additionally reports
`X-Vesta-File-Size` and `X-Vesta-Encoding`. All binary successes include
`X-Vesta-Api-Version: 3`, `X-Vesta-Request-Id: request-N`; errors remain JSON.
Listings omit SHA-256 (no content hashing); `modified_at` is epoch milliseconds
or null.

### Large-file ranges, text pages and follow

All new reads use the same device session, live `files.read` scope checks, path
policy and **submitted/canonical/opened-handle** protected/admission-document
exclusions. File content is **not redacted**: `files.read` is a powerful raw-content
grant. Bytes are never normalized, decoded lossily, or line-ending transformed.

Range supports exactly one `bytes=a-b`, `bytes=a-`, or `bytes=-N` suffix range.
Offsets are canonical decimal; inclusive end clamps to EOF. Empty files, zero
suffixes, reversed/out-of-file starts, malformed/multipart ranges and resolved
ranges over **4 MiB** return 416 `range_not_satisfiable` with
`Content-Range: bytes */<current-size>`. Successful 206 responses include
`Content-Range: bytes <start>-<end>/<current-size>`, `Content-Length`,
`Accept-Ranges: bytes`, `X-Vesta-File-Size`, `X-Vesta-Encoding` and a quoted strong
metadata ETag. This ETag is SHA-256 of size, nanosecond-resolution reported mtime
and native file identity (Windows volume/file index or Unix device/inode), **not
content SHA-256**. It detects ordinary append/truncate/replace/mtime changes, not
malicious rewrites restoring identical metadata. The legacy full-read SHA-256
ETag uses a separate validator scheme; start range sessions with a range response.
Ranges do not report `X-Vesta-Sha256`, since hashing the whole file would be unbounded.

`If-Range` exact equality with the range ETag honors Range. Mismatch (including
weak/date values) ignores Range and falls back to the legacy full read: 200 for a
file ≤1 MiB, **413 for a larger file**, retaining current range ETag/size headers
so the client can resynchronize with a fresh bounded range. Duplicate Range or
If-Range headers and If-Range without Range refuse with 400. There is no
unbounded full-download fallback. Each disk range holds one of two global range
slots through its last outstanding chunk; one 64-KiB blocking read at a time,
no read-ahead queue or file-sized allocation. It reuses artifact streaming's
8-KiB/s throughput floor over each configured request-timeout window, with at
most one 64-KiB chunk of burst credit carried between windows so socket
backpressure does not falsely classify an active reader as stalled. Credit is
bounded even after a large burst; socket buffers and the drain deadline remain
bounded. Metadata changes during the range abort the
body before completing the advertised length; clients retry/reconcile, not assume
a pinned immutable snapshot.

`TextPage` is the ordinary JSON envelope's `data`:

```json
{"bytes_base64":"<standard padded base64>","encoding":"utf-8",
 "start_byte":"0","cursor":"123","next_byte":null,"prev_byte":null,
 "size":"123","etag":"\"<metadata digest>\""}
```

`encoding` is `utf-8` or `binary`. Tail selects the last N physical LF-delimited
lines (a trailing LF does not add an empty line; CRLF and an unterminated final
line are retained exactly), returning its end byte as `cursor`. Lines returns
complete lines plus `next_byte` (end, null at EOF) and `prev_byte` (start, null at
BOF); request the latter with `direction=backward` to page backwards. With no
`from_byte`, direction defaults backward from current EOF; with `from_byte`,
defaults forward. Explicit forward without a cursor starts at zero. Non-EOF/BOF
cursors must follow LF; out-of-file or mid-line cursors refuse 400. Limits are
1–1000. Scan/output storage is bounded to **4 MiB** per page, read in 64-KiB
chunks; an exceptionally long line/page exceeding that budget returns 413 rather
than truncating a line. The encoded JSON can exceed the ordinary 1-MiB projection
bound, but remains bounded to 6 MiB. Pages are live observations, not retained
snapshots; compare the metadata ETag when combining them.

Encoding is a conservative hint, not a whole-file validation: legacy full reads
and text pages validate their returned bytes; ranges examine only the first
64 KiB; follow validates each returned chunk. NUL or invalid UTF-8 indicates
`binary`; multibyte splits at a sample/chunk boundary can conservatively report
binary. Without `decompress=1`, no byte transformation occurs and `.log.gz` files
are read as compressed binary bytes. The opt-in bounded cached gzip view is
documented under **Rotated gzip logs** below; follow remains raw-only.

Follow uses the existing native realtime codec/task ownership, **32 global / two
per device-session** cap shared with console and pack sockets, 1024-byte inbound
ceiling, 128-KiB bounded outbound buffer, two-second send deadline and control/idle
budgets. Slow sends disconnect; there is no subscriber queue. Every 250-ms sample
reauthenticates the session and opens/rechecks the exact canonical file under live
scope policy. Default cursor is the observed EOF; explicit cursor begins there.
At most one **16-KiB raw-byte** append per tick (64 KiB/s; base64 JSON frame <24 KiB).
File shrink or native identity replacement emits `files.rotated`, resets to zero
and resumes. A supplied cursor beyond current EOF also resets. A temporarily
missing/unreadable file closes the stream; reconnect after rotation completes.
Truncate-and-regrow entirely between samples can escape shrink detection. Cursors
are byte offsets, not persistent generation-bound references; compare ETags after
reconnect and use tail/lines/range to resynchronize. Native clients must service
Ping/Pong and reconnect after idle or authorization closure. Exact messages:

```json
{"type":"files.available","api_version":"3","cursor":"123","size":"123","etag":"\"<digest>\""}
{"type":"files.rotated","api_version":"3","cursor":"0","size":"42","etag":"\"<digest>\""}
{"type":"files.append","api_version":"3","from_byte":"0","cursor":"42","size":"42","etag":"\"<digest>\"","encoding":"utf-8","bytes_base64":"<standard padded base64>"}
```

Pages default 50, maximum 100. Directory order is ASCII case-insensitive name,
then exact name; bounds remain 1000 returned/4000 scanned entries. Trash retains
existing deterministic order. Cursors bind server/device/collection/directory,
all observed metadata and offset; changed fingerprints return 409 `cursor_gap`.
They are **not retained snapshots**; changes preserving observed metadata are
not detected. Cursors max 512 bytes; query max 8192 bytes with strict percent
decoding. IDs grant no authority.

### Journal, retries, uploads and limitations

Game-root effects use existing journaled FileBrowser policy/native mutation/
success-audit completion. Durable HTTP success commits with audit/journal status.
No endpoint restarts the game. Delete/overwrite use recoverable trash; no purge,
permanent or recursive delete. Default trash: seven days, 10 GiB/10% volume,
10,000 entries; deletion max 256 MiB, space reserve 64 MiB. Rename is regular-file
only; mkdir creates one directory. Remote trash list/restore are strictly bound
to both creating principal and device; even full file grants do not confer
cross-owner restore. Trash records preserve the opened-handle canonical original
relative resource. Ambiguous legacy original paths containing `~` are refused
on remote restore; no cross-owner or alias override capability is added.
Native checks are not a hostile same-account,
handle-relative ancestor-walk confinement guarantee.

Writes/upload totals max 16 MiB, chunks max 1 MiB, additionally limited by listener
configuration. Upload defaults: four per subject, 64 global, 256 MiB reserved,
fixed one-hour expiry. Gaps/mismatched overlap return 412 with null current hash;
identical overlaps are accepted. Persisted actual stage length permits reconnect
resume. Finish verifies full size/SHA-256, rechecks original precondition and
atomically retires upload metadata with journaled completion. Abort affects
staging only. Failed staging may retain an identical prefix; replay validates it,
never installs unchecked staging.

Schema 14's durable outcomes bind server/principal/device and a digest of HTTP
method/route/key. Canonical intent covers paths, validators, offsets and byte
digests. Exact successful retries return original outcomes without effect/audit/
provider work, even after rename, trash restore or upload retirement, but always
reauthorize **current stored exact capability/resource requirements**. Changed
intent returns 409 `idempotency_conflict`. Keys are per route/method. Safe ordinary
failures release pending reservations. Interrupted/uncertain pending records
return 409 `recovery_required`; native journal recovery does not currently resolve
the HTTP outcome ledger. No blind replay. Schema 15 adds terminal completion
timestamps and an indexed seven-day retention window. Each remote admission
purges at most 128 terminal outcomes older than seven days before lookup/reserve;
pending/recovery rows are never purged. Existing terminal rows are dated at
migration time. A purged key no longer promises its original outcome; retries
outside retention are new requests and must satisfy current preconditions.
At 10,000 retained rows/server new mutations still fail 503: sufficient pending
interruptions can exhaust capacity and require operator remediation.
A process-wide admission mutex serializes ordinary remote mutations and install
reservation/final admission, but is released throughout provider resolution and
download. These are explicit MVP limits, not automatic reconciliation promises.

Fixed errors: 400 `invalid_request`; 401 `management_session_invalid`; 403
`management_forbidden`; 404 `resource_not_found`; 409 `idempotency_conflict`,
`confirmation_invalid`, `cursor_gap`, `recovery_required`; 412
`precondition_failed`; 413 `request_too_large`; 503 `temporarily_unavailable`.
No native errors, provider URLs, paths outside the root or secrets are exposed.

### Provider installs

Only provider/project/version identifiers are accepted (nonempty ASCII
alphanumeric/hyphen/underscore, max 128 bytes each). No request URL, executable,
filename/path, hash, catalogue source or overwrite choice. Existing resolution
derives a safe mods/plugins `.jar` destination, enforces fixed metadata/CDN HTTPS
host allowlists and redirect policy, verifies provider SHA-512 (Modrinth) or SHA-1
(CurseForge), then create-only journaled write/audit. Returned SHA-256 covers the
installed bytes. Synchronous install is privileged trusted-operator code-content
management, not caller-selected native execution; never auto-restarts.
Production CurseForge uses the build-time application key (`CURSEFORGE_API_KEY`,
required by nightly CI), with trusted runtime overrides `VESTA_CURSEFORGE_API_KEY`
then `CURSEFORGE_API_KEY` (nonempty visible ASCII, max 1024 bytes). Missing/invalid
credentials fail closed 503. Credentials never come from request fields or
responses. Remote installs have separate two-install admission and a dedicated
two-worker transport blocking pool, not the ordinary HTTP file/config workers.
After durable actor/request reservation, bounded provider metadata/download work
runs outside global mutation admission. The narrow final admission rechecks live
authority and create-only precondition before journaled effect. The whole remote
install budget is 120 seconds, including admission waits, metadata and body;
production HTTP adapters use the remaining budget, body/effect checks reject
expired work. Native mutation-slot acquisition uses bounded try-lock with that
same remaining deadline, including after final global admission. Expiry returns
503 `temporarily_unavailable`, releases both admission scopes, and safely removes
the pending HTTP reservation when no uncertain effect occurred. All ordinary
remote mutations also share a 120-second request/global/native admission budget;
trusted local mutation APIs retain their existing lock behavior. An abandoned HTTP request can leave admitted work until this bound,
not indefinitely. Trusted custom provider/download adapters must bound their
blocking calls to the supplied deadline; wrappers cannot interrupt arbitrary
trusted Rust implementations. Tests use existing fakes, no real provider network.

### Properties schema and confirmation

Known valid values are typed; `rcon.password`, unknown values and malformed known
projections are always `"[redacted]"`. Source max 65,536 bytes, 256 decoded keys,
4096 bytes/original value. Duplicate decoded keys/escaped aliases are refused.
GET also recognizes safe read-only vanilla settings such as world generation,
pack selection, transfer/spam settings, compression and pause-when-empty. This
read schema does not add PATCH authority. Management secrets, keystore paths and
passwords, filtering configuration, private resource-pack URLs and unknown/modded
keys stay hidden.
PATCH accepts 1–32 unique known keys, rejecting unknown/duplicate changes and wrong
types/ranges. Untouched physical blocks retain comments, unknown lines, escapes
and line endings; changed keys serialize safely and final admission is revalidated.
This is a limited UTF-8/ASCII editor, not arbitrary Java properties emulation or
agent host configuration editing.

- Boolean: `online-mode`, `white-list`, `enforce-whitelist`,
  `enforce-secure-profile`, `prevent-proxy-connections`, `enable-rcon`,
  `enable-query`, `enable-command-block`, `pvp`, `allow-flight`, `spawn-monsters`,
  `spawn-animals`, `spawn-npcs`, `allow-nether`, `force-gamemode`, `hardcore`,
  `hide-online-players`, `log-ips`, `enable-status`.
- Integer: `max-players` 1–1000; `view-distance`/`simulation-distance` 2–32;
  `server-port`/`rcon.port`/`query.port` 1–65535; `spawn-protection` 0–1024;
  `player-idle-timeout` 0–10080; `op-permission-level`/
  `function-permission-level` 1–4.
- String enums: difficulty peaceful/easy/normal/hard; gamemode
  survival/creative/adventure/spectator.
- Strings: `motd` max 256 ASCII bytes; `rcon.password` 1–128 ASCII bytes.
  Controls, slash and backslash refused; secrets never enter success outcomes.

Weakening includes disabling online-mode/enforce-secure-profile (default true),
disabling enabled whitelist/enforce-whitelist/proxy prevention, enabling
RCON/query/command blocks or allow-flight (default false), raising permission
levels (defaults op=4/function=2), and any spawn-protection reduction (effective
default 16 when absent or malformed). Other listed schema keys are not classified
as security weakening. Exact source hash and exact proposed changes bind the
receipt. Without a receipt, no file effect and **202** data is
`{operation_id,state:"awaiting_confirmation",confirmation_token,expires_at_ms}`.
Explicit second submission repeats the **same key, changes and expected hash**,
adding `confirmation_token`. Existing one-time repository receipts bind server,
subject/device, operation type/target, intent and source revision; expire after
120 seconds. Repreparation supersedes prior receipt. Acceptance, approval-operation
success, file effect, audit and HTTP outcome commit together; failed completion
compensates effect and rolls back receipt consumption. Refusals burn receipts
with denial audit in a separate transaction after effect rollback. Tokens are not
in audits/durable HTTP outcomes. A changed safe intent cannot reuse a prepared
weakening key. Exact committed retries need no second receipt. Incomplete approvals
have no game-runner binding and existing restart recovery invalidates them, not
executes them. `restart_needed:true` reports desired/effective divergence only.

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

**The multi-server paths in this table remain NOT IMPLEMENTED.** Singular-server
implemented paths are listed in section 2.1. Phase labels are prerequisites, not implementation
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

## 4A. Implemented automation, notification and backup management

This subsection is **implemented**, not a promise about the remaining draft
routes. Paths below are relative to `/api/v3/manage`. They use the existing
admitted management session and V3 envelope (`api_version`, `request_id`, then
`data` or `error:{code}`). Timestamps ending in `_ms` are epoch milliseconds;
nullable fields are emitted as JSON `null`. Unknown body fields, unsupported
methods, GET bodies, unknown/duplicate query keys and mutation queries are rejected.
Every mutation requires exactly one `Content-Type: application/json` and one
`Idempotency-Key` containing 16–128 visible ASCII bytes. Mutation bodies are
limited to 70 KiB (and the listener's smaller configured limit, if any).

### Workflow requests and durable mutation results

`automation.view` authorizes global workflow/run/step observations. Mutations
require `automation.manage`; acting for another execution principal additionally
requires its current rank to be strictly lower than the caller's. Self is allowed.
Owner/Administrator receive both capabilities; automatic access sources cannot
receive `automation.manage`. Creation does not confer execution authority: every
mutating engine action is reauthorized against the persisted execution principal.

| Method and path | Exact request body/query | Success `data` |
|---|---|---|
| `GET automation/workflows` | optional `after=<id>&limit=<1..100>`; default 50 | `{workflows:[WorkflowSummary],next_after:string|null}` |
| `POST automation/workflows` | `{name:string,execution_principal_id:string,definition:Definition,enabled:boolean}` | HTTP 201 `{kind:"workflow",workflow_id:string,revision:integer}` |
| `GET automation/workflows/{id}` | no query | `{workflow:WorkflowDetail}` |
| `PUT automation/workflows/{id}` | `{expected_revision:integer,name:string,execution_principal_id:string,definition:Definition}` | `{kind:"workflow",workflow_id:string,revision:integer}` |
| `DELETE automation/workflows/{id}` | `{expected_revision:integer}` | `{kind:"deleted",workflow_id:string,deleted:true}` |
| `POST automation/workflows/{id}/enabled` | `{expected_revision:integer,enabled:boolean}` | `{kind:"workflow",workflow_id:string,revision:integer}` |
| `POST automation/workflows/{id}/run` | `{}` | `{kind:"run",workflow_id:string,run_id:string|null,skipped:boolean}`; HTTP 202 if admitted, otherwise 200 |
| `GET automation/workflows/{id}/runs` | optional `after=<id>&limit=<1..100>` | `{runs:[RunSummary],next_after:string|null}` |
| `GET automation/runs/{id}` | no query | `{run:RunDetail}` |
| `GET automation/runs/{id}/steps` | optional `after=<0..191>&limit=<1..100>` | `{steps:[RunStep],next_after:integer|null}` |
| `POST automation/runs/{id}/discard` | `{}` | `{kind:"discarded",run_id:string,discarded:true,operation_cancelled:false}` |

List cursors are exclusive, ordered ascending by opaque ID. `next_after` is the
last returned cursor when the page is full, otherwise null; a full final page may
therefore be followed by an empty page. Step cursors flatten `index*6+attempt`.
Revisions range from 1 through 9,007,199,254,740,991. A stale expected revision
returns HTTP 412 `precondition_failed`. Other state conflicts return HTTP 409
`automation_conflict`. Missing resources return 404 `resource_not_found`.

Exact observation shapes (the two detail shapes add only `definition`):

Run listings (`automation/runs` and `automation/workflows/{id}/runs`) accept
optional `order=newest`. Default ordering remains ascending ID for compatibility.
Newest mode sorts by `(started_at DESC,id DESC)`; `after` is the last returned run
ID and advances toward older rows using that time/ID pair. The same query mode
must be used for every page. `next_after` remains nullable and opaque. Workflow
summaries include `trigger`, so listing cards does not require reading every full
definition. Fetch full definitions only when editing or duplicating.

```text
WorkflowSummary = {trigger:Trigger,id:string,name:string,enabled:boolean,
  execution_principal_id:string,created_at_ms:integer,updated_at_ms:integer,
  revision:integer}
WorkflowDetail = WorkflowSummary + {definition:Definition}
RunSummary = {id:string,workflow_id:string,workflow_revision:integer,
  execution_principal_id:string,trigger:object,state:string,current_step:integer,
  attempt:integer,next_wake_at_ms:integer|null,started_at_ms:integer,
  finished_at_ms:integer|null,outcome_code:string|null}
RunDetail = RunSummary + {definition:Definition}
RunStep = {run_id:string,index:integer,attempt:integer,cursor:integer,state:string,
  operation_reference:string|null,output:JSON|null,error_code:string|null,
  started_at_ms:integer,finished_at_ms:integer|null}
```

Definitions are pinned into runs, including revision and execution principal.
Workflow detail intentionally includes the owner-authored console line; it is
not executable shell text. A definition is at most 65,536 serialized bytes:

```json
{
  "trigger": {"kind":"manual"},
  "missed_policy": {"kind":"skip_and_notify"},
  "steps": [{
    "id":"announce",
    "action":{"kind":"notify","severity":"info","title":"Ready","body":"Done"},
    "timeout_secs":120,
    "retries":{"max_retries":0,"backoff_secs":0},
    "on_success":{"kind":"next"},
    "on_failure":{"kind":"fail"}
  }]
}
```

`missed_policy`, `retries`, `on_success` and `on_failure` default to the values
shown. Every nested object is strict, including tagged unit variants. `steps`
contains 1–32 uniquely named steps; retries are at most 5. Branches may only move
forward, never loop. The closed tagged unions are:

```text
trigger:
  {kind:"manual"|"agent_startup"|"server_started"|"server_stopped"|
    "server_crashed"|"player_count_zero"|"first_player_joined"|"player_first_joined"|"update_available"}
  {kind:"cron",expr:string,utc_offset_minutes:integer}
  {kind:"interval",seconds:integer}
  {kind:"backup_completed"|"backup_failed",profile:string|null}
missed_policy:
  {kind:"skip_and_notify"|"run_latest"} | {kind:"catch_up",max:1..10}
action:
  {kind:"delay",secs:integer}
  {kind:"backup"|"verify_backup",profile:string}
  {kind:"start"|"stop"|"restart"|"clean_trash"}
  {kind:"console_command",line:string}
  {kind:"notify",severity:"info"|"warning"|"critical",title:string,body:string}
  {kind:"script_preset",preset:string,params:{string:string}}
on_success:
  {kind:"next"|"end"} | {kind:"goto",step_id:string}
  {kind:"if_output_equals",key:string,value:JSON,then_step:string,otherwise:string|null}
on_failure: {kind:"fail"|"next"} | {kind:"goto",step_id:string}
```

Cron is numeric five-field cron, with lists/ranges/steps, Sunday 0/7 and ordinary
restricted day-of-month/day-of-week OR semantics. Offsets are fixed UTC minutes
in -840..840; no host timezone or DST interpretation. Console lines contain no
control characters and are at most 1,024 bytes. Script presets are owner-approved,
not uploaded scripts, shell commands or free-form executable paths; a host may
refuse unsupported actions. Outputs are bounded to 16 KiB and nesting depth 8.

Workflow names and step IDs are nonblank, control-free strings of at most 128
bytes. Step timeouts and delays are 1–86,400 seconds; retry backoff is 0–86,400
seconds, persisted exponentially per attempt. Intervals are 60–31,536,000 seconds.
A notify action's title/body are bounded to 256/4,096 bytes. Script preset names
are at most 128 bytes; at most 32 parameters have control-free keys/values bounded
to 64/2,048 bytes, with the entire parameter map also subject to output bounds.
The manual-run endpoint requires an enabled workflow with the manual trigger.

Only one run per workflow can be running/suspended. Ordinary duplicate triggers
skip; explicit catch-up queues at most ten occurrences serially. Delay/retry wake
times and submitting intents survive restart. A persisted handle is polled,
not blindly resubmitted; an interrupted submitting intent reuses its durable
idempotency key. Timeouts and uncertain results fail without retry and **do not
cancel** the underlying operation. Deadlines govern persisted Delay steps too,
and are checked before wake completion; a delay observed at/after its timeout
fails through its failure branch. Only an explicit authorization `forbidden`
suspends the run and emits a critical notification. Authorization `busy` or
`unavailable` keeps a durable pending attempt, retried once per subsequent tick,
without notification spam and bounded by the original step deadline (starting at
the first pending attempt). Authorization `unsupported`/`invalid` fails the step
without retry through its ordinary failure branch. Authorized discard also does
not cancel an underlying operation.
Deleting a workflow preserves pinned run history. Retention is ninety days,
1,024 finished/2,048 total runs, 128 workflows and 4,096 notifications; active work
is not evicted, and capacity pressure skips admissions with a warning.

Workflow and inbox mutations atomically store their small result with the
mutation/audit commit. Receipts are scoped to server, principal, device, method,
path and domain. Identical normalized intent replays its original result even
after a run completes or a workflow is deleted; a changed intent under the same
key returns 409 `idempotency_conflict`. Every replay rechecks current capability
and current target rank. Receipts retain seven days, with a shared ceiling of
4,096 and separate 256-record workflow/inbox domain ceilings; saturation returns
503 `temporarily_unavailable`. After retention clients must not assume old keys
still deduplicate work. These APIs do not expose trusted producer/tick/event APIs.

### Notification inbox

All four routes require `notifications.view` (Owner, Administrator and Moderator
have it by default). State is per principal, shared by that principal's devices.

| Method and path | Exact request | Success `data` |
|---|---|---|
| `GET notifications` | optional `cursor=<nonnegative integer>&limit=<1..100>`; defaults 0/50 | `{notifications:[Notification],next_cursor:integer,oldest_cursor:integer|null}` |
| `GET notifications/unread` | no query | `{unread_count:integer}` |
| `POST notifications/read` | `{ids:[string]}`; 1–256 IDs | `{kind:"notifications_read",marked_read:true}` |
| `POST notifications/acknowledge` | `{ids:[string]}`; 1–256 IDs | `{kind:"notifications_acknowledged",marked_read:true,acknowledged:true}` |

```text
Notification = {id:string,cursor:integer,created_at_ms:integer,
  severity:"info"|"warning"|"critical",title:string,body:string,source:string,
  read:boolean,acknowledged:boolean}
```

Fetch is exclusive of the monotonic cursor. `next_cursor` advances only over
returned entries; `oldest_cursor` lets a client detect a retention gap. Reading
and acknowledging are idempotent; acknowledgment also marks read. Notification
production is host-only. Title/body/source are bounded to 256/8,192/128 bytes.

### Backup jobs, inspection restore and confirmed in-place restore

| Method and path | Capability | Exact request | Success `data` |
|---|---|---|---|
| `GET backups` | `backups.view` | optional `before=<opaque cursor>&limit=<1..200>`; default 50 | `{profiles:[BackupProfile],jobs:[BackupJob],next_before:string|null,restore_mode:"inspection_only",recovery_required:boolean}` |
| `GET backups/jobs/{id}` | `backups.view` | no query | `{job:BackupJob}`; also `restore_mode:"inspection_only"` for non-in-place jobs |
| `GET backups/snapshots` | `backups.view` | required `profile=<profile>` only | `{profile:string,snapshots:[{id:string,time:string}]}` |
| `POST backups/run` | `backups.create` | `{profile:string}` | HTTP 202 `{job_id:string,kind:"backup",restore_mode:"inspection_only"}` |
| `POST backups/verify` | `backups.verify` | `{profile:string}` | HTTP 202 `{job_id:string,kind:"verify",restore_mode:"inspection_only"}` |
| `POST backups/restore` | `backups.restore` | `{profile:string,snapshot:string,paths:[string]}` | HTTP 202 `{job_id:string,kind:"restore",restore_mode:"inspection_only"}` |
| `POST backups/restore-in-place` | `backups.restore` | `{profile:string,snapshot:string,paths:[string],skip_safety_snapshot?:boolean}`; default false | HTTP 202 `{job_id:string,kind:"restore_in_place",state:"awaiting_confirmation",confirmation_token:string,expires_at_ms:integer}` |
| `POST backups/jobs/{id}/confirm` | `backups.restore` | `{confirmation_token:string}` | HTTP 202 `{job_id:string,state:"queued"}` |
| `POST backups/jobs/{id}/resolve-recovery` | `backups.restore` | `{}` | HTTP 200 `{job_id:string,state:"failed",recovery:"resolved_by_user"}` |
| `GET backups/safety-snapshots` | `backups.view` | required `profile=<profile>` only | `{profile:string,snapshots:[{id:string,time:string}]}` |
| `GET backups/policy` | `backups.view` | no query | `{source:"host_config"|"stored",revision:integer|null,profiles:[PolicyProfile],limits:{max_snapshots:integer|null,max_repository_bytes:integer|null,min_free_bytes:integer}}` |
| `PUT backups/policy` | `backups.configure` (owner/administrator-reserved) | `{expected_revision:integer|null,profiles:[PolicyProfile],limits?:{max_snapshots?:integer,max_repository_bytes?:integer,min_free_bytes?:integer}}`; 0–16 profiles; no zero caps | `{source:"stored",revision:integer}`; stale `expected_revision` is 409 `backup_policy_revision_conflict`; never replay-cached |
| `POST backups/policy/reset` | `backups.configure` (owner/administrator-reserved) | `{expected_revision:integer}` | `{source:"host_config",revision:null}`; 409 on a stale revision |
| `POST recovery/export` | `recovery.export` (owner-reserved) | `{passphrase?:string}` (8–1024 bytes) | HTTP 200 with the export itself: `application/json` without a passphrase, `application/octet-stream` (encrypted format, see recovery-export.md) with one. Built in memory up to 64 MiB; larger is 413 `recovery_export_too_large_use_local_command`. Audited with whether it was encrypted; the passphrase is never stored, audited or replay-cached |
| `GET backups/storage` | `backups.view` | no query | `{repository_bytes:integer,restore_points:integer,safety_snapshots:integer,free_bytes:integer|null,limits:{max_snapshots:integer|null,max_repository_bytes:integer|null,min_free_bytes:integer},backups_stopped:"backup_snapshot_limit_reached"|"backup_storage_limit_reached"|"backup_disk_space_low"|null}`; single listing at a time like snapshot listing |

```text
BackupProfile = {name:string,mode:"online"|"stopped",keep_last:integer,
  keep_daily:integer,keep_weekly:integer,keep_monthly:integer,
  on_mutation:"warn"|"fail-world"|"fail-any",warn_seconds:integer,
  max_snapshots:integer|null}

PolicyProfile = {name:string,mode:"online"|"stopped",keep_last?:integer,keep_daily?:integer,
  keep_weekly?:integer,keep_monthly?:integer,on_mutation?:"warn"|"fail-world"|"fail-any",
  warn_seconds?:integer,max_snapshots?:integer|null,include?:string[],exclude?:string[]}; omitted fields take the host.toml
  defaults (retention 0, fail-world, 60 s warning, no cap). Unknown fields are refused;
  repository, S3 and restore-root settings cannot be set through the API.
BackupJob = {id:string,profile:string,kind:"backup"|"verify"|"restore"|"restore_in_place",
  state:"awaiting_confirmation"|"queued"|"running"|"succeeded"|"failed"|
    "uncertain"|"recovery_required",
  requested_by:string,snapshot_id:string|null,error_code:string|null,
  created_at_ms:integer,started_at_ms:integer|null,finished_at_ms:integer|null,
  result:BackupResult|null}
BackupResult = optional structured facts only:
  snapshot_id:string; files_new/files_changed/files_unmodified/data_added/
  total_bytes_processed/source_changes:nonnegative integer;
  consistency:"world_save_consistent"|"stopped";
  world_changed/verified/truncated:boolean; retention_error:string|null;
  changed_paths:[relative string]; restored_to:string.
In-place BackupResult = optional validated facts only:
  mode:"in_place"; safety_snapshot_id:32-lowercase-hex string|null;
  files_written/files_unchanged/files_removed/dirs_removed:nonnegative integer;
  hold_kept/modified:boolean; recovery:"resolved_by_user"|"superseded_by_restore";
  job:bounded ID (only with recovery:"superseded_by_restore").
```

Profile names are 1–32 lowercase ASCII letters/digits/hyphens and cannot start
with `-`. Snapshot IDs are exactly 32 lowercase hex characters. Restore paths are at
most 64 confined literal relative paths; pattern characters `* ? [ ] \\ { }`
are rejected rather than interpreted. An empty array selects the full snapshot.
`POST backups/restore` remains **inspection-only**, into a fresh configured
restore-root/job-ID folder. It never replaces a running/stopped game's files,
starts the game or changes its configured world. The listing's legacy
`restore_mode:"inspection_only"` describes that inspection route, not in-place
jobs; in-place mutation and job-detail responses do not claim inspection-only. `restored_to` is returned only when it matches the
current trusted configuration-derived job directory. Repository paths are exposed only by
the reserved `backups/config` setup route; S3 credentials and native engine/storage
diagnostics are never projected. Snapshot
projections deliberately omit internal tags and native source paths, and validate
canonical RFC3339/RFC3339Nano dates before returning `time`; malformed metadata
returns a fixed unavailable error rather than echoing native text. `changed_paths`
is included only when every path passes the caller's current scoped `files.read`
check; otherwise that field is omitted. Fixed safe error codes replace unknown
persisted engine errors.

In-place restore prepares the exact snapshot, paths and safety-snapshot choice;
confirmation is one-time, expires after two minutes and is bound to the preparing
principal and device. Malformed tokens return the same 409
`backup_confirmation_invalid` as wrong, expired or already-used confirmations.
Validly parsed wrong tokens burn the prepared request. Tokens appear only in the
prepare response body, never job projections, logs or replay receipts. All three
in-place mutation routes require the usual valid idempotency key but bypass
receipt/replay caches: repeating prepare creates a fresh job and token, while
confirm and resolve recheck durable state and do not replay success. Clients
must not blindly retry prepare expecting the original token.

The game must already be affirmatively stopped; the worker prevents starts and
pending crash restarts while applying. No route stops, kills or starts it.
Only the profile's promoted restore points or safety snapshots are restorable.
Unless explicitly skipped, a safety snapshot is taken first; the newest five per
profile are retained and listed by `backups/safety-snapshots`. Ignored/protected
paths are never written or removed. Success projects `mode`, `safety_snapshot_id`,
the four numeric file/directory counts and `hold_kept`; failure after applying
projects `mode`, `safety_snapshot_id` and `modified`. An interrupted modified tree
requires recovery and blocks starts across restarts. `recovery_required` in the
listing reflects that block. An authorized resolve accepts the current tree
without rollback; alternatively, a successful full restore supersedes earlier
recovery jobs. Cleared jobs project only the fixed `recovery` marker and, for
supersession, the bounded replacement `job` ID. Unexpected result keys and
invalid field types/values are dropped, never echoed.

Backup keys bind server/principal/device/method/path; `requested_by` is the
admitted principal ID, never a caller body field. The backend receives the admitted
Subject, durably records the principal/device binding and rechecks current backup
capability when a queued job starts; a schedule or queue cannot retain old access.
For `run`, `verify` and inspection `restore`, repeating an accepted intent
returns the same durable job ID. Changed profile/kind/restore target under the
same key returns 409 `backup_idempotency_conflict`. Jobs are ordered by creation
timestamp descending, then ID descending. `before` is an opaque exclusive
composite cursor covering both timestamp and ID, so equal-timestamp jobs are not
skipped. A full page returns its last row's cursor as `next_before`; clients pass
that string back unchanged. The current versioned encoding is
`b1.<16 lowercase hex timestamp digits>.<lowercase hex UTF-8 job-ID bytes>`;
it is bounded to 276 bytes and malformed/noncanonical cursors are rejected.
No configured backup service returns 404
`backups_not_configured`, after authentication, capability and shape checks.
Other fixed errors are `backup_forbidden` (403),
`backup_profile_unknown`/`backup_job_unknown` (404),
`backup_invalid_request` (400), `backup_busy` (409), and
`backup_unavailable`/`backup_state_unavailable` (503). Snapshot listing has a
short backend deadline and single-flight admission; its busy refusal specifically
returns 503 `temporarily_unavailable`, not a job-admission conflict; this applies
to both normal and safety snapshot listings. Confirmation refusal returns 409
`backup_confirmation_invalid`. In-place job error codes additionally include
`backup_confirmation_invalid`, `backup_confirmation_expired`,
`backup_snapshot_not_restorable`, `backup_snapshot_unknown`,
`backup_restore_path_not_in_snapshot`, `backup_restore_target_unsafe`,
`backup_restore_restart_pending`, `backup_safety_snapshot_failed`,
`backup_restore_interrupted` and `backup_requires_stopped_server`. Backup and
restore jobs refused by a budget fail with `backup_snapshot_limit_reached`,
`backup_storage_limit_reached` or `backup_disk_space_low`; storage status uses the
same single-flight listing admission. These are fixed job outcome codes, not
free-form diagnostics or immediate HTTP statuses.

Across this subsection, current capability refusal returns 403
`management_forbidden`, invalid sessions return 401 `management_session_invalid`,
invalid shapes return 400 `invalid_request`, listener/adapter body overflow returns
413 `request_too_large`, and storage/receipt infrastructure failures return 503
`temporarily_unavailable`. No SQL, filesystem, command or native engine error text
is copied into an error envelope.

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

Recovery import over the network, SFTP, Discord integration, sleep/MOTD-specific
automation and metrics remain future work; no routes or promises are defined here.
Bounded workflow scheduling, the notification inbox, backup jobs, inspection restore
and confirmed in-place restore are implemented exclusively as described in section 4A.
Also excluded: web UI hosting/proxying, shell/script execution, arbitrary downloads,
remote bootstrap via bearer/config, automated recovery/restart, server creation/
adoption/deletion, signed binary updates, file archives, trash purge, role-management routes and an alternative raw TCP management path.
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

## Users, roles and permissions (implemented 2026-10-11)

All paths are under `/api/v3/manage`, require a device session, use the standard envelope, and every mutation needs an `Idempotency-Key`. The required capability is rechecked inside the same transaction as the change and its audit record.

| Endpoint | Request | Capability |
|---|---|---|
| GET `/users` | optional `offset`, `limit` (default 50, max 100) | `principals.view` |
| GET `/users/{id}` | none | `principals.view` |
| GET `/roles` | none | `principals.view` |
| GET `/access-sources` | none | `access-sources.manage` |
| GET `/users/{id}/devices` | none | `devices.revoke`; own devices or a lower-rank user |
| PUT `/users/{id}/roles` | `{add:[role], remove:[role], expected_version}` | `principals.manage`, plus `roles.assign` to add and `roles.revoke` to remove |
| POST `/roles` | `{name, rank, capabilities:[{capability, scope}]}` | `roles.create` |
| PATCH `/roles/{id}` | `{name?, rank?, capabilities?, expected_version}` | `roles.manage` |
| DELETE `/roles/{id}` | `{expected_version}` | `roles.manage` |
| PATCH `/access-sources` | `{anonymous?, new_user?, whitelist?, whitelist_enabled?, password?, expected_version}` | `access-sources.manage` |
| POST `/invitations` | `{role, expires_minutes}` (1–60) | `invitations.create` |
| DELETE `/devices/{id}` | empty body | `devices.revoke` |

Rank rules:
- A user can change only users of strictly lower rank, never themselves.
- Added roles must rank strictly below the actor and must not be the owner or administrator roles, which can only be granted locally. Removed roles must rank below the actor; only the owner (superadmin) role can never be removed remotely.
- Created or edited roles must rank below the actor and may contain only capabilities the actor holds. Built-in roles cannot be edited or deleted. A role in use, or used by an automatic source, cannot be deleted.
- The anonymous, new-user, whitelist-sync and password roles can never contain management capabilities.
- Invitation codes are for roles below the actor, are shown once, and recheck the inviter's current permissions when redeemed. Redemption checks the resulting user's assigned roles and active allow overrides, not only the invitation role: every grant must fit the inviter's current capability scopes, and all assigned ranks must remain below the inviter. The same resulting-user ceiling applies to direct user-role changes and to role-rank increases for every affected user. Expired/revoked allow overrides and deny overrides do not grant authority and are not delegated.
- Superadmin is granted or revoked only with `vesta-agent superadmin grant|revoke` on the server machine. The last superadmin cannot be removed.

Errors: `management_session_invalid` (401), `management_forbidden` (403), `invalid_request` (400), `resource_not_found` (404), `precondition_failed` (412, stale `expected_version`), `idempotency_conflict` and `permission_conflict` (409), `temporarily_unavailable` (503).

Retries: receipts replay the stored result for the same `Idempotency-Key` for seven days. Older receipts are purged in batches of 128 on each change (schema v17). The store holds at most 4,096 receipts; only more than 4,096 changes within seven days returns 503.

## Rotated gzip logs (implemented 2026-10-11)

Add `decompress=1` to `files/content`, `files/tail` or `files/lines` to read a rotated `*.gz` log as its decompressed text. Without the flag, the archive bytes are served unchanged.

- Only files whose name ends in `.gz` **and** that start with gzip magic bytes are decompressed. Anything else returns 400 `not_compressed`. Any other value of `decompress` returns 400 `invalid_request`.
- `files/content?…&decompress=1` requires a `Range` header (400 `range_required` otherwise). Ranges, `X-Vesta-File-Size` and line cursors refer to the decompressed text.
- The decompressed view has its own strong ETag of the form `"gz-…"`, which changes when the archive changes. A stale `If-Range` returns 412 `precondition_failed`; there is no full-body fallback.
- Corrupt or truncated archives (including a failed CRC or length check) return 422 `archive_corrupt`, and nothing partial is served.
- An archive that would expand beyond 512 MiB, or more than 256 times its compressed size (with a 1 MiB minimum allowance), returns 413 `request_too_large`. Decoding also caps compressed input at 512 MiB, including concatenated empty members. The 60-second work deadline is checked beneath the decoder and at EOF, including when no text is produced; deadline expiry returns 503 `temporarily_unavailable`.
- The first read decompresses the archive once into an owner-only cache in protected state. Later reads reuse that copy for 30 minutes after it was made, then it is rebuilt on the next read. This TTL is best-effort while a view is held open: on Windows, an open download can prevent deletion, so an expired view may remain usable until its handles close. Failed deletions remain counted, rather than silently freeing capacity. Only one decompression runs at a time, and a concurrent first read returns 503 `temporarily_unavailable` to retry. The cache keeps at most 8 views and 1 GiB; expired and oldest eligible views are removed first. Before creating a stage, admission reserves its maximum possible output allowance against both cache limits. If capacity cannot be reclaimed, staging refuses with 503 `temporarily_unavailable`.
- Staging preserves the existing 64 MiB protected-state volume free-space reserve. Volume-space preflight covers the maximum output allowance before staging, then runs before each decompressed write. Insufficient free space refuses with 503 `temporarily_unavailable`; no partial view is published. These preflight checks are not an atomic disk reservation against unrelated writers.
- `files/follow` has no decompressed mode.
- Authority and protected-document checks are those of the archive itself (`files.read` on its path).

## File download (implemented 2026-10-11)

`GET /api/v3/manage/files/download?path=<file>[&decompress=1]` downloads a whole file of any size, streamed from disk in 64 KiB chunks. It needs `files.read` on the path; protected documents are refused as for other reads.

- **Response:** 200 with `Content-Disposition: attachment; filename="<name>"`, `Content-Length`, a strong `ETag`, `X-Vesta-File-Size` and `Accept-Ranges: bytes`. `decompress=1` downloads the decompressed text of a `.gz` log and drops `.gz` from the file name.
- **Growing files:** the download contains the bytes present when it started. Lines appended afterwards are not included. Before starting, the requested interval is fingerprinted with fixed-size disk reads; bytes delivered are checked against that opening digest before the final frame is released. Overwrites and truncate-regrow changes therefore abort rather than complete mixed content under the opening strong ETag, even if the file has grown. File identity, size and modification checks also reject incompatible changes. A pure append remains permitted, and a rotated log keeps downloading from the original held handle without reopening the pathname. A change during initial fingerprinting returns 412 `precondition_failed`; a change after headers aborts the body, which the client must treat as an incomplete download.
- **Live authorization:** downloads and disk-backed ranges reauthenticate the retained session and recheck `files.read` on the submitted, canonical and held-handle resources before and after each 64 KiB read. Device revocation, capability/scope removal, or session expiry aborts subsequent chunks; a rotated pathname cannot redirect that check to a different resource. Bytes already delivered cannot be recalled.
- **Text pages:** `files/tail` and `files/lines` revalidate the held file after scanning. If it changed, they return 412 `precondition_failed` instead of a page carrying mixed text and a stale ETag; reopen and retry with the new observation.
- **Resume:** send `Range: bytes=N-` with `If-Range: <etag>` from the first response to get 206 with the rest of the file. If the ETag no longer matches, the whole current file is sent again as 200. Only `bytes=N-` is accepted; any other form, or `N` at or beyond the end, returns 416 `range_not_satisfiable`.
- **Limits:** downloads share the two-at-a-time disk read budget with ranged reads (503 `temporarily_unavailable` when busy) and the connection throughput floor, so a stalled client is dropped. Opening integrity fingerprinting adds a complete read of the requested interval before response headers, although memory stays bounded to fixed-size chunks. Large or slow files can incur substantial startup latency or exceed the existing request/admission timeout; that refusal is intentional rather than weakening the strong-ETag integrity check. A file changing during this opening pass can likewise be refused; retry once a stable opening observation is available.

## Typed moderation (F34, implemented 2026-10-13)

POST `/players/moderate` accepts one internally tagged, unknown-field-denying object:
`{action,player,expected_generation,reason?,duration_seconds?,confirmation?}`.
Actions are `kick`, `ban`, `pardon`, `whitelist_add`, `whitelist_remove`,
`op`, `deop`, `kill`, and `lightning`. Only kick/ban accept reason; only ban
accepts duration_seconds (60–31,536,000, absent/null means permanent). Only OP
accepts confirmation: prepare with no token, then resubmit the identical intent
and Idempotency-Key with the returned token in `confirmation`. The token uses the
existing exact actor/device/payload/generation-bound confirmation mechanism.
OP/de-OP require the separately appended `players.operator` capability, granted by
default only to owner/administrator. All other actions require `players.moderate`.
ConsoleWrite is neither required nor sufficient. Moderator has player visibility
and typed moderation but no raw console authority by default.

Players are Java Edition ASCII `[A-Za-z0-9_]{1,16}` names, not UUIDs/selectors.
Reason is trimmed, 1–256 characters and at most 512 UTF-8 bytes if supplied;
invalid reasons return the existing invalid_request code. Every control character
and Unicode line/paragraph separator is refused, even at trimmed edges. A ban without reason
renders/stores the fixed default “Banned by an operator.” Reasons remain trailing
arguments, never command verbs or additional lines. Commands are rendered on the
server and use the same generation-bound fresh-policy native-write guard as console
commands. No execute-as-player action exists; lightning uses `execute at`, never
`execute as`. Lightning requires Java 1.13+; a known earlier configured version
returns 412 `lightning_unsupported_version`. Unknown/non-numeric version identifiers
cannot establish compatibility and retain the documented 1.13+ prerequisite.

Actions require the exact Running generation. Admission is not delivery and a
succeeded operation means **native acknowledgement of the fixed stdin write**, not
proof Minecraft applied the action. No console-output parsing invents application
success. Existing fixed admission errors apply: 400 `invalid_request`, 403
`management_forbidden`, 409 `idempotency_conflict`/`confirmation_invalid` or
`operation_conflict`, and 412 `precondition_failed`.

Bans, ops and whitelist are protected/no-follow reads of the configured game root,
bounded to 1 MiB and 10,000 entries each. Missing files mean empty lists; malformed,
oversize or unreadable files return 503 `players_file_malformed`,
`players_file_too_large`, or `players_file_unavailable`, without document contents.
Entries written by the server are display-only: names are bounded (1–64 characters,
no control characters) rather than restricted to the Java Edition command set, so
offline-mode names do not hide the whole list. OP level (1–4) is read-only. Vanilla has no per-player op-level command: changing it
requires a stopped-server ops.json edit, which is out of scope for these routes.

History records actor/device/operation/action/player/effective reason/duration,
created_at_ms, expires_at_ms, outcome and expiry_state. The operation admission,
history insertion and operation.submit audit commit together. Audit metadata includes
action, player and reasonGiven, **not reason text**. History is included in schema-v22
credential-free recovery export. Page cursors use the server-bound moderation scope.

The existing one-second agent automation driver sweeps at most 32 due successful
temporary bans per tick. Duration runs from native-write acknowledgement, not queue
wait. It uses the original principal's keyless automation device
and current policy; lost authority marks expiry blocked and inserts an inbox warning
in the same writer. Stopped games keep expiry pending. Pardon admission and its
submitted state/operation reference commit together; pre-effect failure can retry on
a later generation, while an uncertain started write is never blindly repeated.
Later ban/re-ban/permanent ban/manual pardon supersedes older expiry case-insensitively,
including a queued expiry's fresh effect guard. The sweeper checks the bounded current
banned-players.json entry and exact effective reason; missing/changed entries are
superseded with an inbox warning; malformed/unavailable documents leave intent pending and are retried (the game rewrites the file in place); if the file is still unreadable one hour after expiry, the expiry is blocked with an inbox warning and no pardon is sent.
External edits can still race that observation and a native write (no filesystem/game transaction exists).
Expiry is durable across ordinary agent restarts; recovery import preserves history,
not missing operation execution state, so uncertain/submitted history requires manual
review rather than an invented replay. IP bans, mute, teleport and inventory remain
out of scope.

### Ban plugins are not supported yet

Vesta currently offers only its own vanilla-command moderation. Ban plugins such as
LiteBans, LibertyBans, AdvancedBan and BanManager often replace /ban, /pardon and
/kick and store bans in their own database instead of banned-players.json. With
such a plugin, Vesta commands may be handled by the plugin; the vanilla ban/OP/
whitelist read views and Vesta history do not reflect plugin ban databases. History
covers only actions issued through Vesta: bans made in-game are not recorded.
A temporary ban whose current vanilla file entry cannot be confirmed is marked
superseded and produces an inbox warning, not a silent permanent ban. Review that
ban manually. Plugin and connector integration is future work; there are no plugin
adapters or plugin settings in this feature.

## Sleep and MOTD presentation

See [sleep-and-motd.md](sleep-and-motd.md) for GET/PUT `/api/v3/manage/sleep/policy`, GET/PUT `/api/v3/manage/motd/policy`, POST `/api/v3/manage/motd/preview`, and public GET `/api/v3/status`. Policy writes use optimistic revisions and atomic audits. The existing authenticated server status adds `lifecycle` without changing process `state`. Offline player identities/nonzero counts are never fabricated. Live MOTD still uses the authorized server.properties editor and requires Minecraft restart; offline/app templates do not alter the live game response.

## Activity operation details and audit identities

GET `/api/v3/manage/operations` and `/operations/{id}` retain all existing fields
and add these fields (normal API envelope and list pagination are unchanged):

```json
{
  "operation_id": "example-operation",
  "state": "succeeded",
  "progress_percent": 100,
  "cancel_requested": false,
  "created_at_ms": 1791115200000,
  "updated_at_ms": 1791115200001,
  "finished_at_ms": 1791115200001,
  "kind": "console.command",
  "actor": {"principal_id":"principal-id","display_name":"PlayerOne","device_name":"Desktop"},
  "summary": {"command":"say Hello, players!"},
  "summary_purged": false,
  "outcome": "stdin_written"
}
```

`kind` is closed: `lifecycle.start`, `lifecycle.stop`, `lifecycle.restart`,
`lifecycle.kill`, `lifecycle.sleep`, `console.command`, `players.moderate`, `other`.
These are the bindings currently stored in the operation ledger. Backups,
workflow runs, file intents and pack publications have their own APIs/journals;
they are not fabricated into this ledger. Future operation bindings require
an explicit projection before any request input can be disclosed.

Every read checks **current `tasks.view`**. Console `summary.command` additionally
requires current `console.read` **or the same initiating principal** (including
another enrolled device). Otherwise summary is `{"command_redacted":true}`.
The complete validated command line is preserved, subject to the same exact
known configured-path and retained agent-issued credential replacements as
console output. Output-only masking of incomplete credential fragments at
capture boundaries does not apply to complete typed fields. This deterministic
redaction is not a general detector for arbitrary third-party passwords.
Names and free-text moderation reasons also use known credential/path redaction.
Credentials, absolute host paths, hashes and raw exception/result text are never
added as diagnostic fields.

Moderation requires current `players.view`, with no issuer exception. Its summary
is `{"action":"ban","player":"PlayerOne","reason":"Griefing","duration_seconds":3600}`;
reason and duration are omitted when absent. Without that capability the summary
is `{"details_redacted":true}`. Lifecycle summary is
`{"target_generation":"32-lowercase-hex","result_generation":"32-lowercase-hex"}`.
Start has `target_generation:null`; result is included only when recorded and
validated. Kill additionally includes `confirmed:true|false`, based on a consumed
confirmation receipt, never a supplied boolean. No generation is a PID.

`outcome` is null before completion. Its closed vocabulary is `stdin_written`,
`process_exited`, `process_started`, `succeeded`, `cancelled`, `recovery_required`,
`timeout`, `stale_generation`, `write_failed`, `forbidden`, `input_unavailable`,
`confirmation_rejected`, `maintenance_active`, `failed`. `stdin_written` means
native delivery was acknowledged, not that Minecraft/plugin semantics succeeded.
Unrecognized stored errors become `failed`.

Purging follows existing retention: `summary:null, summary_purged:true` when
canonical input is purged, even for its issuer. No audit/history fallback rebuilds
that summary. Legacy/unrecognized/unavailable/corrupt canonical input yields
`summary:null, summary_purged:false`; safe lifecycle/outcome fields remain.

The list accepts optional `kind=<one-closed-kind>` and `actor=me`; filters apply
before the bounded page limit. `me` matches the initiating principal across devices,
including that principal's automation device. Duplicate/unknown filters are 400.
The native internal lifecycle journal remains input-free historical invalidations;
there is currently no remote operations stream. Consumers refresh the authorized
detail/list projection rather than receiving canonical inputs in event payloads.

GET `/api/v3/manage/audit` still requires current `audit.view`. Each item keeps
`cursor,at_ms,action,outcome` and adds `actor` with the same shape above and
`target:string|null`. Labels resolve from retained identity rows, preferring the
linked Minecraft name to the principal label. Revocation does not erase historical
attribution. Keyless automation devices resolve to
`{"kind":"workflow","name":"Vesta automation"}`; system/unresolved actors resolve
to `{"kind":"system","name":"Vesta agent"}` (automation engine records use
`{"kind":"workflow","name":"Workflow engine"}`). Arbitrary stored source text
is never echoed. Safe file-mutation targets are validated relative paths from the
allowlisted audit path field; server-properties targets are `server.properties`.
Retained role-update records resolve the current role name; operation records
with a retained moderation target resolve the player name only with current
`players.view`. Deleted/unresolvable roles have null labels.
Unknown, absolute, traversal, protected and abbreviated paths produce null. Raw
metadata, payloads, audit-chain hashes and credentials remain withheld.

GET `/api/v3/manage/automation/runs?after=<run-id>&limit=<1..100>` now exposes
cross-workflow run history, including retained runs whose workflow was deleted.
It requires current `automation.view` and uses the same bounded run projection
as `/automation/workflows/{id}/runs`: `{runs:[...],next_after:string|null}`.
List entries omit workflow definitions; per-run detail remains the existing
`/automation/runs/{id}` route. Pagination and cursor semantics are unchanged.


## Launcher invitation and device inventory (schema v25)

All shapes below are `data` inside `{ "api_version":"3", "request_id":"...", "data":... }`.
Errors use the existing envelope/status codes. Mutations require exactly one
`Idempotency-Key` (16-128 visible ASCII bytes); receipts are scoped to server,
principal, device, method and route. Current session and policy are checked in the
writer before mutation or receipt replay. Mutation, audit
`management.permissions.mutate` (target is the route), and receipt commit together.

- `GET /invitations?offset=0&limit=50`: requires `invitations.create`.
  Returns `{ "invitations":[{"id":"opaque","role":{"id":"viewer","name":"Viewer","rank":20},"created_at_ms":0,"expires_at_ms":1,"created_by":{"display_name":"Alice"}}],"next_offset":null }`.
  Only pending unexpired remotely created invitations visible to their creator or
  callers strictly above the invited role's current rank are included. Local CLI
  pairing codes have no remote creator and are not listed. Codes and hashes never
  appear. IDs are random and independent of the enrollment secret/hash; v25 assigns
  IDs to existing remote invitations. Redemption/expiry/revocation remove visibility.
- `POST /invitations` keeps `{role,expires_minutes}` and all existing response fields,
  adding `id` and first-response `pairing_link`:
  `{ "issued":true,"id":"opaque","code":"one-time-code","role":"viewer","expires_at_ms":1,"pairing_link":null }`.
  With configured listener `public-endpoint` and current listener TLS fingerprint,
  the link is `vesta://pair?endpoint=<encoded>&code=<encoded>&fp=<encoded>&sid=<encoded>`,
  exactly the CLI format. Address is never inferred from request headers. Receipts
  retain the ID and old nonsecret fields, omitting both `code` and `pairing_link` on
  replay. Existing creation/delegation/rank rules are unchanged.
- `DELETE /invitations/{id}`: empty body; `invitations.create` and either creator
  principal or strictly higher rank than the invited role. Returns `{ "revoked":true }`.
  Unknown, redeemed or expired IDs return 404 `resource_not_found`; an existing
  target outside the rank rule returns 403. A successful retry replays its receipt.
- `GET /devices?offset=0&limit=50`: requires `devices.revoke`; lists own devices and
  devices of active users strictly below caller rank, including revoked devices.
  Returns `{ "devices":[{"id":"device-id","user":{"id":"user-id","display_name":"Alice"},"name":"Desktop","created_at_ms":0,"last_seen_at_ms":null,"revoked":false,"current":true}],"next_offset":null }`.
  `current` compares the calling device ID; another device of the same user is false.
  Last-seen is currently unrecorded and always null. Keyless automation devices may
  appear as devices of their owning user. Historical revoked devices remain visible.
- `PATCH /devices/{id}`: strict JSON `{ "name":"My desktop" }`, own principal only;
  requires a current management session, with no additional capability. DeviceLabel
  validation is unchanged. Returns `{ "id":"device-id","name":"My desktop" }`.
  Renaming another user's device is forbidden even for owner. Unknown IDs are 404.
  Own revoked-device labels may also be renamed; this never reactivates the device.

Inventory pagination defaults to offset 0 / limit 50; limit is 1-100, offset is
0-1,000,000. Filtering precedes pagination. `next_offset` is null when exhausted.
Duplicate/unknown/malformed query keys are 400. GET bodies and DELETE bodies are
empty; JSON mutation objects reject unknown/duplicate fields. These offset views
are current-state views, so concurrent changes can shift pages.


## Server branding

Branding uses `config.edit`: these are durable server-owned settings, with the
same authority as other presentation configuration. GETs retain `status.view`,
matching existing server identity visibility. The settings and normalized icon
survive restart and are included in credential-free recovery export. They do not
change pack name/version, discovery manifests, live server.properties, Minecraft
`server-icon.png`, or the separately configured sleep/MOTD policy icons. Existing
Minecraft presentation uses state-specific MOTD icons; it has no shared live
server-icon setting to update safely from this route.

- `GET /server` retains all fields and adds `description` and `revision`:
  `{ "server_id":"id","name":"Hosted","description":null,"revision":"0","agent_version":"0.1.0","minecraft_version":"1.21.1","pack_version":"1.0.0","configured_mode":"same_account" }`.
  Before a branding override, name/description fall back to trusted pack metadata.
  Revision is a decimal string, initially `"0"`, incremented on each successful PATCH.
- `PATCH /server`: strict JSON `{ "expected_revision":"0","name":"My server","description":"Welcome" }`.
  At least one optional string is required; null is invalid. Omitted fields are
  preserved. Name: 1-128 Unicode scalars, at most 512 UTF-8 bytes, not all whitespace,
  no controls. Description: 0-1024 scalars and at most 4096 UTF-8 bytes; LF is the
  only permitted control. Empty description clears its text. Returns the complete
  GET shape at the new revision. Stale revision: 412 `precondition_failed`.
- `GET /server/icon`: 200 raw `image/png` bytes with strong `ETag: "<lowercase SHA-256>"`
  of the returned normalized PNG; 404 `resource_not_found` when unset. No JSON
  envelope for successful bytes; errors retain it. No conditional GET or ranges.
- `PUT /server/icon`: exactly one `Content-Type: image/png`, raw PNG up to 262144
  bytes, `Idempotency-Key`, and either `If-None-Match: *` (create only) or exact
  strong `If-Match: "<sha256>"` (replace). No wildcard/list/weak If-Match. Returns
  `{ "etag":"\"<sha256>\"" }`, also in the ETag header. PNG must be square,
  64-512 pixels inclusive, RGB/RGBA 8-bit and non-interlaced. CRCs, chunk ordering,
  deflate length/checksum and row filter values are validated with bounded
  decompression; ancillary metadata is stripped. Other PNG encodings return 400
  `invalid_request`. The normalized output also fits 256 KiB. Invalid PNG is 400;
  oversized input 413 `request_too_large` (the configured listener body cap may be
  lower). ETag describes stored normalized output, not the submitted file bytes.
- `DELETE /server/icon`: empty body, `Idempotency-Key` and exact strong `If-Match`;
  returns `{ "deleted":true }`. Unset is 404; stale condition is 412.

PATCH and icon mutations check current `config.edit` inside the settings writer;
setting change, `management.permissions.mutate` audit and durable receipt are atomic.
PUT/DELETE missing or malformed conditions return 400; valid unmet conditions return
412. Successful retries replay their receipts even after later changes. Icon
conditions and bytes are part of the idempotent intent. Icon writes do not change
the server text revision. All these routes reject query strings.


## Backup snapshot pages and detail

All routes require current `backups.view` and use the normal JSON envelope.

- `GET /backups/snapshots?profile=daily[&before=<opaque>&limit=50]` and
  `GET /backups/safety-snapshots?profile=daily[&before=<opaque>&limit=50]` return
  `{ "profile":"daily","snapshots":[{"id":"32-lowercase-hex","time":"2026-10-22T00:00:00.000Z"}],"next_before":null }`.
  `profile` remains required. Limit defaults to 50 and is 1-100; the old unbounded
  list is now newest first, with ID descending as the timestamp tie-breaker.
  Pass `next_before` unchanged to continue; it is null when exhausted. Cursor is
  bound to server, profile and normal/safety collection, and contains its ordering
  position, so removal of the cursor snapshot does not invalidate continuation.
  Unknown/duplicate/malformed parameters and foreign cursors return 400. These
  metadata reads still share the engine's one-listing budget and bounded
  repository scan (at most 10,000 snapshots); busy listing returns 503
  `temporarily_unavailable`. Pagination bounds response size, not repository I/O.
- `GET /backups/snapshots/{id}?profile=daily` returns
  `{ "profile":"daily","snapshot":{"id":"32-lowercase-hex","time":"2026-10-22T00:00:00.000Z","size_bytes":null,"added_bytes":null,"file_count":null,"tags":["profile=daily"]} }`.
  This reuses cheap tag metadata for committed snapshots in that profile; unknown
  ID/profile membership returns 404 `resource_not_found`. Size, newly added bytes
  and file count are not present in the engine's cheap index, so remain null.
  No manifest/world scan is performed to invent these numbers. Safety-only
  snapshots are listed separately and have no new detail route. Disabled backups
  and unknown profile retain existing 404 `backups_not_configured` /
  `backup_profile_unknown` errors. GET bodies are empty.

Unified `/activity` was deliberately deferred. Operations, backup jobs, file
intents and pack publications have different retention, actor visibility and
cursor stores; merging them safely is more than an additive quick projection.
Existing operations/activity, backup-job and audit endpoints remain available.

### Launcher observation capacity (2026-10-23)

The HTTPS control worker pool remains bounded to two actual jobs. Up to 32 callers
may wait for admission for at most 500 ms; cancelled HTTP waiters cannot release a
worker executing native/database work. Session preflight runs in that blocking
pool and waits for in-memory session-store contention, rather than returning
503 for normal overlapping requests. Realtime observation uses its own blocking
pool and the existing two-socket session budget; neither occupies game stdin.
Management error envelopes preserve HTTP keep-alive. Malformed/protocol-level
refusals may still close the connection because the request body is not safely
reusable. No rate limits were raised.


## Phase 1 explicit Sleep / Wake (D3)

`POST /api/v3/manage/lifecycle/sleep` takes `{expected_generation}` and uses
`lifecycle.stop`; it gracefully stops Minecraft and durably enters Sleeping only
after proven exit. It requires a configured offline responder (`offline_status`).
`POST .../wake` takes `{}` and uses `lifecycle.start`, only from Sleeping.
`POST .../stop` takes `{expected_generation}` while running, or `{}` while already
stopped/sleeping; success clears sleep intent and turns off the game-port listener.
Start takes `{}` from Stopped. All use the normal device/path-scoped Idempotency-Key,
durable operation queue and audit. Manual sleep may disconnect players; the idle
timer retains its zero-player evidence checks and only moves Running to Sleeping.
Network joins can wake only Sleeping: Stopped never opens a responder/listener.

`[process] on-agent-start = "stay_stopped" | "sleep" | "start"` defaults to
`stay_stopped`. Legacy `start-on-agent-start = true/false` maps to start/stay_stopped;
specifying both is invalid. Sleep startup requires configured offline status. A
supervised live generation is never restarted or relabelled by the startup policy.
Status retains process `state` and canonical `lifecycle` (`offline` means Stopped),
adds `sleep_available` and `on_agent_start`, and retains `sleep_blocker` for idle
eligibility. The launcher exposes `server_sleep`, `server_wake`, the generic
`server_lifecycle`, and TS lifecycleControls returns Sleep/Wake plus menu Stop.

UI follow-up: show Sleep as primary when `sleepAvailable`, Wake while sleeping,
Stop in the menu (including while sleeping), and expose the startup policy in
settings. Header changes in this phase only supply labels required by the expanded
contract; no page layout is built.


## Confirmed raw Minecraft files (D4)

`GET /api/v3/manage/files/content?path=server.properties` (also whitelist.json,
ops.json, banned-players.json, banned-ips.json) requires `config.edit` plus scoped
`files.read`. The built-in owner and administrator have these capabilities.
They appear in directory listings with `protection: "confirm_edit"`. Private
Vesta paths, wrappers, VCS directories and the private state root stay hidden;
canonical/opened-handle and symlink checks are unchanged. Other generic mutations
(rename/delete/upload) still refuse these protected documents.

Raw PUT uses the same `/files/content?path=...`, exact `If-Match` and Idempotency-Key.
First submit validates the bytes and returns `{state:"awaiting_confirmation",
operation_id,confirmation_token,expires_at_ms}` without changing the file. Repeat
identical bytes, validator and intent with `&confirmation_token=...` within 120 s.
The one-time receipt binds actor, device, old hash and payload (including path).
Success is `{state:"applied",sha256,size,trash_id,restart_needed:true}`. Lost-success
replays return the committed outcome. File replacement, receipt consumption,
audit and outcome commit together; prior bytes go to recoverable trash. Properties
use the existing Java-properties parser, duplicate-key and typed validators;
JSON documents must parse as arrays. Read and edit audits contain paths, not bytes.
Raw reads intentionally include Minecraft secrets for these authorized roles.
The synchronous receipt operation retains its legacy `config.server_properties`
internal name; its payload hash additionally binds the raw path and content.

Launcher: `server_file_write_protected`, `fileWriteProtected(...)` returns the
existing `PropertiesUpdateResult` union. `native-files.ts` distinguishes `hidden`
from `confirm_edit`. UI follow-up (owned by the files UI engineer): use the server
protection flag; allow reading/editing confirm_edit; show a two-step confirmation
with the Settings ? Players hint for player lists. Keep generic rename/delete and
uploads disabled for these files. Do not treat protected Minecraft files as hidden.


## Java management

See [java-runtimes.md](java-runtimes.md) for GET `/java`, POST `/java/select`,
POST `/java/download`, job and software compatibility contracts (D5).


Startup policy can also be set remotely as nullable `on_agent_start` in the
existing sleep/policy document (native/TS `SleepPolicy.onAgentStart`). Null inherits
host.toml; an explicit value is persisted with the policy. Start/sleep authority
is rechecked against the policy actor on agent startup; refusal leaves it stopped
and raises a notification. `sleep` requires offline_status. The trusted local
environment override `VESTA_AGENT_STAY_STOPPED=1` forces stay-stopped at startup;
the dev shell sets it for staged agents so a restart never boots Minecraft.


## World details

See [world-details.md](world-details.md) for extended GET `/world`, generation-bound
POST `/world/datapacks/toggle`, saved metadata semantics and D1 launcher removals.


## Launcher history consumption

See [metrics-history.md](metrics-history.md) for D9 native/TS range requests,
columnar unknown/gap semantics and fixture coverage.

## Remote backup configuration (launcher setup)

This extends the backup API without requiring a host `[backups]` table. All paths
below are relative to `/api/v3/manage/`; the normal authenticated JSON envelope applies.
Both routes require `backups.configure`, reserved to Owner/Administrator and not
remotely delegable (schema v26). Lower roles get 403 before any repository effect.

| Route | Request | Data response |
|---|---|---|
| GET `backups/config` | no body/query | `{source:"host_config"|"stored",revision:integer|null,profiles:PolicyProfile[],limits:PolicyLimits,repository:{kind:"local"|"s3",location:string|null,initialized:boolean},backups_configured:boolean,schedule_owner:"automation"}` |
| PUT `backups/config` | `{expected_revision:integer|null,profiles:PolicyProfile[],limits?:PolicyLimits,initialize_repository?:boolean}` + Idempotency-Key | `{revision:integer}` |

The profile list is the complete replacement: create/update/delete by adding/editing/
omitting entries. Empty disables backups and preserves stored snapshots. Revisions
are safe integers 1..9007199254740991, null for host/default policy. Responses after
a replay retain their original revision; GET observes current state. Conflicts are
409 `backup_policy_revision_conflict` or `backup_idempotency_conflict`; invalid input
is 400 `backup_invalid_request` (closed wire decode errors use `invalid_request`).
The agent uses bounded durable receipts and audits in the same policy transaction.

`PolicyProfile` retains existing name/mode/retention/mutation/warning/cap fields and
adds `include?:string[]` and `exclude?:string[]`, each defaulting to empty and bounded
to 64 entries. Includes are literal game-relative roots; empty includes the whole
game, including worlds and configs. Excludes are single-line gitignore-subset
patterns, max 512 bytes each, with no negation/comment prefix. Host file ignore rules
still apply. All JSON documents are closed; repository paths, S3 credentials and
schedule fields in PUT are refused.

Without host configuration, storage is fixed at `<instance base>/backups/repository`
and inspection restores at `backups/restores`. Initialization is opt-in, idempotent,
and checks game/state separation and local linked ancestors. The host alone may
choose another repository or S3 configuration. GET does not create storage. It
reports `initialized:false` if repository readiness cannot be proven. See
[backups.md](backups.md) for filesystem trust and interrupted initialization behavior.

Schedules remain in Automation: create/edit workflows separately with
`automation.manage`. A configuration save never starts/stops the game or schedules
a backup. `backups_configured` on GET server is true exactly when the effective
policy has at least one profile, including after configuration replacement/reset.
# Server resource management (Stage 1)

Full operational bounds and recovery behavior: [server-content.md](server-content.md).
These routes do not edit or publish the separate client modpack.

| Method | `/api/v3/manage/` route | Request / result |
|---|---|---|
| GET | `content` | `ContentInventory {items,capabilities,minecraft_version,software,truncated}` |
| POST | `content/install` | `{kind,provider,project_id,version_id}` -> `ContentChange` |
| POST | `content/update` | Install identifiers plus `{path,expected_sha256}` -> `ContentChange` |
| POST | `content/mutate` | `{action:remove\|enable\|disable,kind,path,expected_sha256}` -> `ContentChange` |
| POST | `content/cancel` | `{id}` -> cancelled `ContentChange` |
| POST | `content/identify` | `{path}` -> nullable provider reference; optional bounded hash lookup |
| POST | `software/custom-jar/plan` | `{path,expected_revision}` -> reviewed custom JAR plan |
| POST | `software/restore-managed/plan` | `{expected_revision}` -> reviewed managed restoration plan |
| POST | `software/custom-jar/apply` | `{plan_id,confirmation_token,idempotency_key}` -> software job |

POST requests carry `Idempotency-Key`; custom apply also binds the same body key.
Custom plans are ephemeral, actor/device/revision-bound, expire after 120 seconds,
and require an owner. The token must never be exposed to webview code.

`kind` is `mod|plugin|datapack`. Each inventory item has `kind,path,filename,size`
(decimal bytes), nullable `enabled,sha256,provider,declared,pending,pending_id,pending_error`.
Provider references are `{provider,project_id,version_id}`. Declared metadata has
nullable `loader,side,name,version` and string `dependencies`. Unreadable and
unidentified JARs stay visible. Datapack enabled state reuses saved world metadata;
toggle through the existing world routes rather than renaming datapacks.

`ContentChange` has `id,state:pending|applied|cancelled,pending,path,error,trash_path`.
`pending` is `install|remove|update|enable|disable` or null. Folder datapack removal
retains a journaled directory tree at `trash_path`; ordinary files use file trash.
Content changes reuse `content.install` plus the precise file mutation scopes.
Inventory capabilities expose `list,install,update,remove,toggle,cancel`,
`mods_supported,plugins_supported,datapack_toggle_offline,datapack_toggle_running`.

Running/draining/uncertain Minecraft never receives live managed JAR writes.
Pending changes apply before the next explicit start/restart; a durable marker also
prevents autonomous supervisor launches while they remain. Failures retain pending
state and block launch. Cancellation is available to the originating device or a
current owner before an effect starts. Current actor authority and original file
validators are checked again during application. No implicit restart is performed.

Incompatible software families return 409 `content_mods_unsupported` or
`content_plugins_unsupported`; a datapack filename toggle returns 409
`content_datapack_toggle_requires_world`. Other file errors retain existing mapping.
Custom status includes `current.custom_jar:{path}` and its job kinds are
`custom_jar|restore_managed`; existing software errors, activation journaling,
selected Java/JVM flags and recovery semantics apply.

## Client pack workspace (Stage 2)

See [pack-workspace.md](pack-workspace.md) for lifecycle, payloads, bounds and
compatibility rules. All paths below are under `/api/v3/manage/`. Reads/edits use
`pack.edit`; preview additionally uses `pack.build`; publish uses all three of
`pack.edit`, `pack.build`, `pack.publish`, plus `pack.force_update` when required.
Existing inventory/config scopes protect the server mismatch report. Writes
require `Idempotency-Key`; preview is read-only and does not require one.

| Method | Route | Contract |
|---|---|---|
| GET | `pack/draft` | Revisioned draft, MC drift, publication/minimum and snapshot advice |
| GET | `pack/draft/icon` | Nullable PNG/JPEG MIME type and base64 bytes |
| POST | `pack/draft` | `{expected_revision,manifest}` replaces draft |
| POST | `pack/draft/add` | `{expected_revision,entries}` or pinned provider root + selected dependencies |
| POST | `pack/draft/delete` | `{expected_revision}` clears entries/loader |
| POST | `pack/draft/upload` | Small base64 upload -> SHA-256 |
| POST | `pack/draft/uploads/begin` | `{upload_id,total_size,sha256,archive}` |
| POST | `pack/draft/uploads/chunk` | `{upload_id,offset,content_base64}` |
| POST | `pack/draft/uploads/finish` | `{upload_id}` verifies bytes and returns receipt |
| POST | `pack/draft/uploads/cancel` | `{upload_id}` discards unused transfer |
| POST | `pack/draft/import` | Archive bytes/upload or current release, with expected draft/release revisions |
| GET | `pack/suggestions` | Read-only missing/newer/removed/drift suggestions and warnings |
| POST | `pack/preview` | `{draft_revision,resolved_entries}` -> validation, diff, manifest, review digest |
| POST | `pack/publish` | Existing configured-source body, or `{expected_revision,draft_revision,resolved_entries,review_sha256,required_update,acknowledge_lower_version}` |
| POST | `pack/minimum` | Existing monotonic minimum policy, unchanged |

Draft and release revisions are separate decimal-string clocks. The workspace
loader is independent of server software. HTTP uses snake_case; native IPC uses
camelCase. Provider identities contain only Modrinth/CurseForge IDs. Successful
workspace publication atomically records the release, published manifest, audit
and exact-body receipt. `resolved_entries` are ephemeral dependency additions or
exact optional-to-required promotions, never arbitrary replacement of draft pins.

### Backup retention

Profiles accept `keep_total` (zero disables the cap) and `keep_hourly`, in addition to the existing retention buckets. The total cap is applied after the bucket union, retaining the newest matches. With no buckets selected, keep the newest `keep_total` backups. The cap applies to each profile; safety snapshots retain their separate bounded policy. Host TOML uses `keep-total` and `keep-hourly`.

### Content write timing

Content changes apply immediately only when the agent acquires stopped-process ownership. While Minecraft runs, the implementation durably stores bytes in `server_content_changes` and writes them before the next start. Whole-pack applies always use this journal. `pending` and `staged_until_restart` therefore mean actual deferred writes, rather than installed files awaiting reload. The start barrier prevents Minecraft starting before these writes finish.

### CraftBukkit and Spigot catalogue

Discovery uses https://hub.spigotmc.org/versions/ and one pinned recipe per Minecraft version. This provides many Minecraft versions but one current source build per version. BuildTools (https://www.spigotmc.org/wiki/buildtools/) compiles that recipe locally. GetBukkit distributes third-party binaries without this installer's required first-party source pins and reference checksums, so they are not mixed into this catalogue. The UI labels this limitation beside build choices.
