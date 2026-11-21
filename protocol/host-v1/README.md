 The read-only projection GET `/api/v3/manage/session` remains as GET `/v1/servers/{serverId}/manage/session` (the caller's server id, principal, roles and capabilities from the trusted server subject; see LAUNCHER_GUIDE).# Host protocol v1

Phase 0 contract, 2026-11-13. Normative words MUST/SHOULD apply to new implementations, not claims about existing code. Freeze this document with [identity](identity.md), [worker](worker.md), and [import](import.md). No migration or old route compatibility is required. Feature payloads continue using existing protocol versions; host routing version is independent.

## Common wire rules

HTTPS, default management port 25500. TLS 1.3 is mandatory for initial PAKE. UUIDs are lowercase canonical hyphenated UUIDs. `hostId` is persistent random UUID, unrelated to server ids; `serverId` is host-unique and never reused. Fingerprint is lowercase SHA-256 hex of leaf certificate DER (64 characters). Times are UTC RFC3339; durations are integer seconds; generation/cursor counters are decimal strings to avoid JavaScript precision loss. Binary fields are unpadded base64url. JSON is UTF-8, rejects duplicate/unknown request fields. No cookies or credentials in URLs; exact one `Authorization: Bearer <token>` except anonymous/PAKE calls. New success responses have the documented JSON fields; errors are `{ "error": { "code": "...", "requestId": "..." } }`. Inherited management payloads retain `api_version:"3"` envelopes. 400 malformed, 401 invalid session, 403 denied host operation, 404 unknown or unauthorized server, 409 conflict/stale plan/generation, 412 ETag mismatch, 413 oversized, 429 rate limit, 503 unavailable. Fixed codes, no OS errors or paths except owner browse/import responses. `Retry-After` on 429/503.

All new mutations require `Idempotency-Key` (UUID); persist actor+method+route+body hash+result for 24h; same key/different body is 409 `idempotency_conflict`. PAKE messages and session challenge/completion instead use their single-use exchange/challenge ids. Use `If-Match` revision on role, registry and endpoint changes. Limit ordinary JSON to 64 KiB and 100 list entries/page; inherited upload limits remain. Host capabilities advertised in authenticated host response gate later features; absent capability means unsupported, not authorized.

## Router and host surface

| Method/path | Request -> response; authority |
|---|---|
| GET `/v1/host/discovery` | Anonymous `{hostId,fingerprint,protocol:"host-v1",name}`; add `claimState:"unclaimed"|"claimed"` only to directly connected LAN/loopback peers. Do not trust forwarded headers. Outside LAN omit name and claimState. No registry. |
| POST `/v1/host/pake/start`, `/pake/finish` | Under the same `/v1/host` prefix; see identity. Claim starts are possible while unclaimed even outside LAN, but no separate public claim-state response. |
| POST `/v1/host/accounts/challenge`, `/accounts/complete`, `/claim` | PAKE-bound account proof and atomic claim, identity.md. Without a Pake header the account routes accept only `purpose:"signin"`, bound to the connection's certificate fingerprint. |
| POST `/v1/host/sign-in` | No code: signin account proof + device key -> `{state:"enrolled",hostId,fingerprint,accountId,deviceId,deviceName,session,expiresAt,authGeneration}` when the account already holds a role; 403 `account_has_no_access` / `device_approval_required`. identity.md "Account sign-in on a new device". |
| POST `/v1/host/sessions/challenge`, `/sessions` | Device proof -> host bearer; identity.md. |
| DELETE `/v1/host/sessions/current` | Current bearer -> 204. |
| GET `/v1/host/operations/{operationId}` | Owner -> `{operationId,state:"running"|"complete"|"blocked"|"failed",error,authGeneration}` for a host mutation awaiting worker barriers; durable retry/status, never a server-operation alias. |
| GET `/v1/host` | Bearer -> `{hostId,name,fingerprint,endpoints,authGeneration,capabilities,role,revision}`; owners also get `newDeviceSignIn:"account"|"approval"`. |
| PATCH `/v1/host` | Owner `{name?,newDeviceSignIn?}` (at least one) -> `{hostId,name,newDeviceSignIn,revision}`. Name: control/bidi characters removed, whitespace collapsed, 1..63 UTF-8 bytes, else 400 `invalid_request`. Audited; renaming re-announces mDNS within seconds and changes discovery/`GET /v1/host` names. |
| GET `/v1/host/owners` | Owner -> `{owners:[{accountId,role:"primary_owner"|"owner",name}]}`, primary first; `name` is the last Minecraft name verified on this host by claim or sign-in, or null. |
| DELETE `/v1/host/servers/{serverId}` | Machine owner `{confirmName,idempotencyKey}` -> `{serverId,removedAt,restoreUntil}`; graceful stop and recoverable removal with a seven-day default. Advertised as capability `server-remove`; a 404 from a machine that lists it means the server is already gone. |
| GET `/v1/host/servers?cursor=...` | Bearer -> `{servers:[{serverId,name,gamePort,state,role}],nextCursor}`; owners see all, others only current server grants. |
| GET `/v1/host/devices` | Owner -> `{devices:[{deviceId,name,accountId,revoked,createdAt}],nextCursor}`; accountId is Minecraft UUID. |
| DELETE `/v1/host/devices/{deviceId}` | Owner -> 204; primary-owner protection and barrier in identity.md. Self-revoke allowed. |
| PUT `/v1/host/owners/{accountId}` | Primary owner, `{role:"owner"}` -> `{authGeneration,revision}`; DELETE demotes owner only. |
| POST `/v1/host/ownership/transfer` | Primary owner, fresh device signature over targetAccountId, nonce, expiry and current revision -> `{authGeneration,revision}`; target must already be an owner. |
| POST `/v1/host/link-codes` | Optional fallback to account sign-in. Owner `{targetAccountId}` -> `{linkId,code,expiresAt}`; local privileged `vesta link` may omit target, then approval shows the verified account. |
| POST `/v1/host/links`, GET `/links/{linkId}`, POST `/links/{linkId}/approve` | Identity.md. GET is either owner-authenticated or authenticated with the link's PAKE capability. |
| POST `/v1/servers/{serverId}/invitations` | Admin or owner `{role,expiresAt,maxUses}` -> `{inviteId,invite,expiresAt}`; only server roles, never owner. DELETE same path plus `/{inviteId}` revokes. |
| POST `/v1/host/invites/redeem` | Verified account+device, `{invite,accountProofId,deviceKey,signature}` -> `{hostId,serverId,role,session}`; proof purpose binds hash of exact invite payload. Atomic usage count and grant; never execute pack installation on redemption. |
| GET `/v1/host/realtime` | 501 `realtime_multiplexer_deferred` in the first slice; interim per-server streams below. |
| `/v1/host/folders/*`, `/imports/*` | Owner-only import.md. |

Owner mutation and transfer signatures use `Sign("vesta.host.owner.v1\0" || JCS({hostId,deviceId,action,targetAccountId,nonce,expiresAt,revision}))`. Nonce comes from POST `/v1/host/ownership/challenge` returning `{nonce,expiresAt}` (60s, single-use). Revoking a primary owner's other devices is restricted to that primary account or privileged local recovery. Owners cannot change owners, transfer ownership or impersonate the primary account.

### Existing route mapping

Dispatch MUST select an authorized registry entry before invoking the existing handler. Never allow a caller-supplied upstream, filesystem root or alternate server id. The new session is converted to an internal current server subject, never forwarded as an old per-server bearer.

| Existing in `crates/vesta-transport/src/` | New path |
|---|---|
| `/api/v3/manage/{suffix}` in `manage*.rs` | `/v1/servers/{serverId}/manage/{suffix}`; method, body, ETag, range, operation idempotency and limits preserved except identity routes below. Includes lifecycle, console, files, players, metrics, content, packs, backups, automation and configuration. |
| `/api/v3/status` | `/v1/servers/{serverId}/status`, authenticated projection. |
| `/api/v2/discovery` | `/v1/servers/{serverId}/discovery`, authenticated feature manifest, existing payload. |
| `/api/v2/pack/{ticket,status,artifact}` | `/v1/servers/{serverId}/pack/{ticket,status,artifact}`; tickets bind host, server, account/device, artifact and generation. |
| `/api/v3/manage/pack/{sync,artifact}` | Prefix rule above; retain digest/range and reconciliation semantics. |
| `/api/v2/realtime/ticket`, `/api/v2/realtime` | Interim `/v1/servers/{serverId}/realtime?installed=N` accepts a host bearer directly; no public ticket route or anonymous feed. |
| `/api/v3/manage/{console/stream,files/follow,metrics/stream}` | Interim `/v1/servers/{serverId}/manage/{console/stream,files/follow,metrics/stream}` WebSockets with host-session authentication. |
| `/api/v3/auth/*`, management pair/enroll/Minecraft/session/password | Host identity endpoints replace them. Pack-password gating, if configured, is additional server policy, not host ownership or device enrollment. |
| management devices/principals/roles/invitations | Host devices and owners use host routes; server role APIs may remain under manage but cannot create owner authority or revoke host devices. Canonical invite creation/redemption is above. |
| `public_resource_packs.rs` `/resource-packs/{digest...}` published URLs | Host adds `/v1/servers/{serverId}` before the existing path; rewrite generated URLs and retain immutable digest policy, byte/range bounds and public exposure setting. Anonymous Minecraft asset delivery is an explicit exception, never file browsing. |
| `/api/v3/setup/*` in `pending.rs` | Removed; claim/import stay on the persistent host listener. |

Host realtime multiplexing is deferred. The implemented per-server WebSocket paths, limits and reconnect requirements are specified in the implemented-slice section below and [LAUNCHER_GUIDE.md](LAUNCHER_GUIDE.md).

## Anonymous exact-port lookup

GET `/v1/here?port=25565`: exactly one decimal port 1..65535, no body. Lookup only the committed registry's effective game-port binding (including sleeping/wake binding), never MOTD/name or a stale import plan. If exactly one server is explicitly public, return `{hostId,fingerprint,serverId,gamePort,pack}`. `pack` is null or `{provider:"modrinth"|"curseforge",projectId,versionId,name}`; no URLs, custom archive, credentials or executable metadata. Distinct multiple-address bindings sharing a port are ambiguous: return the same 404 `not_found` as private/unknown ports. Port or visibility changes invalidate cached lookup atomically. 60 requests/min/IP, 600/min/host; bounded response, no enumeration route, `Cache-Control: no-store`. Launcher caches misses locally 60s and caps discovery attempts at 3s. Result is a hint until pin/PAKE/invite trust is established; pack install always explicit.

## Invites

Transport string `vesta://invite/v1#<base64url(JCS(payload))>`, at most 8 KiB. Fragment avoids HTTP referrer leakage. Payload:

```json
{"v":1,"hostId":"11111111-1111-4111-8111-111111111111","fingerprint":"<64 lowercase hex>","serverId":"22222222-2222-4222-8222-222222222222","endpoints":[{"host":"192.168.1.67","port":25500,"scope":"lan","transport":"https"}],"code":"<32 random bytes base64url>","expiresAt":"2026-11-14T00:00:00Z","pack":null}
```

Endpoint host is IP literal without brackets or DNS hostname, no scheme/path/userinfo; bracket IPv6 when constructing a URL. Scope `lan|public|tunnel`; use actual external management port, never infer 25500 for invite entries. Max eight endpoints. `pack` is null, provider ref above, or `{provider:"vesta",releaseId,sha256}`; custom packs only through authenticated redemption and review. Host stored invite determines role, server, expiry, max uses and pack; mismatching payload rejected. Store only code hash. Code >=256 bits, default 24h/single-use; retry by same bound device returns same grant without another use. Rate-limit misses uniformly. Optional short code requires explicit host endpoint and >=80 random bits, not a mandatory hosted resolver. Fingerprint comes from out-of-band invite; discovery cannot replace it. Launcher tries endpoints without leaking credentials to mismatched certificates and never executes an automatic pack update solely from invite text.

## mDNS

DNS-SD `_vesta._tcp.local.` instance `Vesta-<hostId>`, SRV target machine `.local` hostname, SRV port actual management port, A/AAAA only selected LAN interfaces. TXT keys: `v=1`, `id=<hostId>`, `fp=<fingerprint>`, `name=<display name>`, `state=unclaimed|claimed`, `tls=1`. Name <=63 UTF-8 bytes; each TXT string <=255 bytes, total <=900 bytes. No codes, account ids, paths, server lists or pack metadata. TTL uses the stock advertiser defaults (record-dependent; currently 120s for host/SRV records and 4500s for PTR/TXT); announce changes and goodbye on shutdown. State advertised LAN only. Ignore TXT for trust; re-resolve after DHCP and confirm host identity over pinned TLS or PAKE. Disable advertising with LocalOnly; manual IP works if multicast is unavailable.

## Conformance gate

Before implementation sign-off, both clients must share fixtures for route mapping, JCS signatures, PAKE confirmations, account-purpose binding, invite encoding and worker frames. Required negative cases: wrong host/server, unknown fields, TLS reconnect/exporter mismatch, simultaneous claim, revoked device/queued action, private-port lookup, ticket replay across servers and malformed IPv6. No implementation or security review is implied by this phase 0 document.

## Implemented first slice (H08/H10)

See [LAUNCHER_GUIDE.md](LAUNCHER_GUIDE.md) for runnable configuration and exact
client calls. The listener is `Transport::start_host(HostRouter, &TlsListener)`;
`vesta-agent host <host.toml>` owns it, the host identity/local CLI, zero or one
registry worker, and optional explicitly selected LAN discovery interfaces.

The host realtime multiplexer is deferred. Authenticated `/v1/host/realtime`
returns 501 `realtime_multiplexer_deferred`. Interim native WebSocket paths,
all authenticated directly by the host session in the upgrade header, are:

- `/v1/servers/{serverId}/manage/console/stream`
- `/v1/servers/{serverId}/manage/files/follow?path=<encoded-relative-path>[&cursor=<decimal>]`
- `/v1/servers/{serverId}/manage/metrics/stream`
- `/v1/servers/{serverId}/realtime?installed=<decimal>` for pack changes

These supersede the multiplexer rows above for this slice. Existing channel
frames are unchanged; no subscribe envelope. Streams close after at most 29s
and must reconnect because trusted subjects have a 30s lifetime. Device/session
revocation is polled at 250ms; full worker role/queued-action revocation barriers
remain H07. No legacy session or realtime-ticket endpoint is exposed.

Server discovery retains its existing **POST** method and request body. Pack
status/artifact are GET. POST `pack/ticket` returns `{ticket,artifactUrl}`; ticket
is single-use for 30s, bound to this server, host session/device and generation.
Artifact GET also admits a current host session. All management suffixes retain
existing methods/feature envelopes and validators. Legacy identity/security/SFTP
suffixes are excluded. Published immutable resource packs retain anonymous access
only at `/v1/servers/{serverId}/resource-packs/{digest}.zip`, gated by the existing
published exposure policy. New worker resource-pack builds store that prefixed URL.

First-slice limits: host JSON 64KiB; mapped bodies 4MiB (the H05 adapter bound);
32 realtime streams/host, bounded HTTP connection/body/download admission. Host
lists currently return the small complete set and `nextCursor:null`; cursors and
100-item host pagination are deferred. Host mutation replay uses existing claim,
link and session receipts/challenges; the blanket 24h HTTP `Idempotency-Key`
receipt cache above is **not implemented** for new host/device/owner/browse calls.
Inherited management idempotency is unchanged. Owner mutations require a fresh
single-use signed confirmation and matching revision, so callers refetch after
an ambiguous response. The host response advertises only implemented import
capabilities. Invites, `/v1/here`, endpoint editing and host barrier operations
remain their separate work packages and are not advertised by this router.

Owner PUT/DELETE and transfer bodies contain
`{targetAccountId,nonce,expiresAt,revision,signature}`; PUT additionally contains
`role:"owner"`. `revision` is a decimal string and must equal `If-Match` (quoted
or unquoted decimal). The signing value is the object specified above, with
`action:"promote"|"demote"|"transfer"`. Both owner PUT/DELETE and transfer consume
a challenge from POST `/v1/host/ownership/challenge`. Primary account protection
is enforced again by the durable repository. Signature bytes come from
`vesta_host_client::requests::signing_bytes`.

## Server access and Vesta Server access

Server access is the per-server users, roles and invitations surface under /v1/servers/{serverId}/manage. Vesta Server access is host ownership and device enrollment/revocation under /v1/host. Machine owners appear as immutable owners of every server. Role and capability delegation follows server-local rank and current authority. Owner and Administrator grants require local authority. A role removal increments authGeneration; subsequent requests resolve current grants.

GET manage/users, GET manage/roles, PUT manage/users/{accountId}/roles, GET/POST manage/invitations and DELETE manage/invitations/{inviteId} use the inherited api_version 3 envelopes and snake_case fields. User versions are the host revision; stale expected_version fails 412. See [Host management extensions](management.md) for role editing, assignments and overrides. Devices and users/{accountId}/devices return 404 not_supported and are not advertised. Host devices remain owner-only. Host owners can GET and PATCH manage/access-sources on each server. PATCH accepts only expected_version and sync_whitelist. Sync adds verified Host owners to the Minecraft whitelist and respects bans.

Invite redemption uses POST /v1/host/invites/redeem with invite, accountProofId, deviceKey, signature and optional deviceName. First obtain an account proof with purpose invite and binding SHA-256 hex of exact decoded canonical invite payload. Sign vesta.host.invite.v1\0 plus JCS({hostId,serverId,inviteHash,accountProofId,deviceKey}). Proofs bind the TLS connection and device key. Redemption atomically enrolls the account/device, consumes a use and grants only the stored server role. Repeating redemption on the same device does not consume another use. Revoking an invite prevents future redemption; remove a user's server role separately to end an existing grant. Current generated invites have endpoints:[] because endpoint configuration is deferred; the recipient must retain the separately supplied pinned host address. No pack execution occurs on redemption.

Owner-only removal (capability `server-remove` in `GET /v1/host`; offer it only when listed, and treat `404 not_found` as already removed): `DELETE /v1/host/servers/{serverId}` with JSON `{ "confirmName": "exact current name", "idempotencyKey": "uuid" }` returns `200 { "serverId", "removedAt", "restoreUntil" }`. Durable retries return the original receipt. Errors: `403 forbidden`, `404 not_found`, `422 confirm_name_mismatch`, `409 server_busy` (active create/import/restore/backup/software job), or `409 idempotency_conflict` for changed retry input. The host gracefully stops the game, shuts down its worker, revokes server grants/invites, frees resources, and isolates the directory under `removed/`. See the removed servers contract below for retention and expiry. On the machine, `vesta servers removed` lists names and recovery times; `vesta servers restore <serverId>` restores a stopped server using the same name and port conflict handling as the HTTP route. Grants and invites are not restored.

During process shutdown, host request admission closes with HTTP 503 `host_stopping`. New machine jobs and late automatic starts are refused; interrupted work retains its recovery state. See `docs/operations/shutdown.md` for platform deadlines and service behavior.

The [removed servers contract](removed.md) covers restore, permanent deletion, and retention.
See [machine storage](storage.md) for the snapshot and measurement routes.
Purge receipts include a cached byte estimate when available.
The `server-storage` capability enables the per-server storage view for owners
and server administrators.
Use `storage-cleanup` to preview owner-only cleanup and submit the same targets
with a UUID idempotency key.
The `console-history` capability enables [backward console paging](console.md).
The [host events design](host-events-design.md) describes the planned owner stream.
