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
| GET `/server` | no body/query | 200 `{server_id,name,agent_version,minecraft_version,pack_version,configured_mode:"same_account"}`; other fields strings | `status.view` |
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
| GET `/players` | no body/query | 200 unknown `{known:false,player_count:null,items:null}`; known `{known:true,generation:hex,player_count:integer,items:[{name:string}]}` | `players.view` |
| GET `/pack` | no body/query | 200 `{hosted_revision:decimal-string-or-null,minimum_revision:decimal-string-or-null,status:"published\|unpublished"}` | `status.view` |
| POST `/pack/publish` | `{"expected_revision":"0"}` + key; zero means unpublished | 200 `{outcome:"succeeded",hosted_revision:decimal-string,minimum_revision:decimal-string}` | `pack.publish` |
| POST `/pack/minimum` | `{"expected_revision":"2","minimum_revision":"2"}` + key | same synchronous 200 outcome shape | `pack.force_update` only |
| GET `/audit` | optional `limit`, `cursor` | 200 `{items:[{cursor,at_ms:integer,action:string,outcome:string}],next_cursor:string-or-null}` | `audit.view` |

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
Hash-only credential stores cannot recognize an incomplete token after capture
truncation or a physical line split. As a conservative boundary rule, up to 42
consecutive ASCII base64url bytes (`A-Z`, `a-z`, `0-9`, `_`, `-`) at **each** line
edge are replaced with `[redacted]`, after complete known-material replacement.
The rule applies to every line, including nontruncated lines, so both sides of
newline splits remain protected independently of page/socket boundaries, stream
interleaving and retention gaps. It can redact ordinary words at line edges, or
all of a short ASCII-only line; it does not guess at interior text. This protects
all incomplete fragments of the agent's 43-byte credentials without retaining
plaintext tokens or exposing 42 bytes of a truncated token. Interior content and
the original ring's truncation flag are preserved. HTTP and WebSocket use the
same projection, and JSON escapes the resulting text.

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

Players uses the existing bounded live `list` driver with `players.view` checks at
admission, write and response. Stopped/unavailable/timed-out queries are unknown,
never fabricated zero. Names outside the Minecraft ASCII-name projection are
withheld. Successful authorized observations cache the actual count and original
console observation time for the matching incarnation. Status returns this last
known count/time, which may be stale; no count is carried into a new/stopped
incarnation. Status itself does not issue a player query. Uptime measures
in-process monotonic incarnation observation, not readiness.

Remaining gaps: detailed operation result/error projection, retained expiring
pagination snapshots, persistent stopped-control generations and paginated player
collections. Section 2.2 implements a bounded files/content/server-properties
subset. Other unimplemented management paths remain authenticated 501
placeholders. No capabilities were added or admitted into automatic role
sources. TLS inert-child tests cover start → running → graceful stop → stopped,
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
8-KiB/s throughput floor over each configured request-timeout window, bounded
socket buffers and drain deadline. Metadata changes during the range abort the
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
Production CurseForge requires trusted operator environment
`VESTA_CURSEFORGE_API_KEY` (nonempty visible ASCII, max 1024 bytes); missing/invalid
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

```text
WorkflowSummary = {id:string,name:string,enabled:boolean,
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
    "server_crashed"|"player_count_zero"|"first_player_joined"|"update_available"}
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

### Backup jobs and inspection-only restore

| Method and path | Capability | Exact request | Success `data` |
|---|---|---|---|
| `GET backups` | `backups.view` | optional `before=<opaque cursor>&limit=<1..200>`; default 50 | `{profiles:[BackupProfile],jobs:[BackupJob],next_before:string|null,restore_mode:"inspection_only"}` |
| `GET backups/jobs/{id}` | `backups.view` | no query | `{job:BackupJob,restore_mode:"inspection_only"}` |
| `GET backups/snapshots` | `backups.view` | required `profile=<profile>` only | `{profile:string,snapshots:[{id:string,time:string}]}` |
| `POST backups/run` | `backups.create` | `{profile:string}` | HTTP 202 `{job_id:string,kind:"backup",restore_mode:"inspection_only"}` |
| `POST backups/verify` | `backups.verify` | `{profile:string}` | HTTP 202 `{job_id:string,kind:"verify",restore_mode:"inspection_only"}` |
| `POST backups/restore` | `backups.restore` | `{profile:string,snapshot:string,paths:[string]}` | HTTP 202 `{job_id:string,kind:"restore",restore_mode:"inspection_only"}` |

```text
BackupProfile = {name:string,mode:"online"|"stopped",keep_last:integer,
  keep_daily:integer,keep_weekly:integer,keep_monthly:integer,
  on_mutation:"warn"|"fail-world"|"fail-any",warn_seconds:integer}
BackupJob = {id:string,profile:string,kind:"backup"|"verify"|"restore",state:string,
  requested_by:string,snapshot_id:string|null,error_code:string|null,
  created_at_ms:integer,started_at_ms:integer|null,finished_at_ms:integer|null,
  result:BackupResult|null}
BackupResult = optional structured facts only:
  snapshot_id:string; files_new/files_changed/files_unmodified/data_added/
  total_bytes_processed/source_changes:nonnegative integer;
  consistency:"world_save_consistent"|"stopped";
  world_changed/verified/truncated:boolean; retention_error:string|null;
  changed_paths:[relative string]; restored_to:string.
```

Profile names are 1–32 lowercase ASCII letters/digits/hyphens and cannot start
with `-`. Snapshot IDs are 8–64 lowercase hex characters. Restore paths are at
most 64 confined literal relative paths; pattern characters `* ? [ ] \\ { }`
are rejected rather than interpreted by restic. An empty array selects the full snapshot.
Restore is **inspection-only**, into a fresh configured restore-root/job-ID
folder. It never replaces a running/stopped game's files, starts the game or
changes its configured world. `restored_to` is returned only when it matches the
current trusted configuration-derived job directory. Repository/executable/vault
password paths and native restic/process diagnostics are never projected. Snapshot
projections deliberately omit restic tags and native source paths, and validate
canonical RFC3339/RFC3339Nano dates before returning `time`; malformed metadata
returns a fixed unavailable error rather than echoing native text. `changed_paths`
is included only when every path passes the caller's current scoped `files.read`
check; otherwise that field is omitted. Fixed safe error codes replace unknown
persisted engine errors.

Backup keys bind server/principal/device/method/path; `requested_by` is the
admitted principal ID, never a caller body field. The backend receives the admitted
Subject, durably records the principal/device binding and rechecks current backup
capability when a queued job starts; a schedule or queue cannot retain old access.
Repeating an accepted intent
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
returns 503 `temporarily_unavailable`, not a job-admission conflict.

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

Full in-place world restoration, SFTP, Discord integration, sleep/MOTD-specific
automation and metrics remain future work; no routes or promises are defined here.
Bounded workflow scheduling, the notification inbox, backup jobs and inspection-only
restore are implemented exclusively as described in section 4A.
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
