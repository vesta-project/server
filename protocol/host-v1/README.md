# Host protocol v1

Phase 0 contract, 2026-11-13. Normative words MUST/SHOULD apply to new implementations, not claims about existing code. Freeze this document with [identity](identity.md), [worker](worker.md), and [import](import.md). No migration or old route compatibility is required. Feature payloads continue using existing protocol versions; host routing version is independent.

## Common wire rules

HTTPS, default management port 25500. TLS 1.3 is mandatory for initial PAKE. UUIDs are lowercase canonical hyphenated UUIDs. `hostId` is persistent random UUID, unrelated to server ids; `serverId` is host-unique and never reused. Fingerprint is lowercase SHA-256 hex of leaf certificate DER (64 characters). Times are UTC RFC3339; durations are integer seconds; generation/cursor counters are decimal strings to avoid JavaScript precision loss. Binary fields are unpadded base64url. JSON is UTF-8, rejects duplicate/unknown request fields. No cookies or credentials in URLs; exact one `Authorization: Bearer <token>` except anonymous/PAKE calls. New success responses have the documented JSON fields; errors are `{ "error": { "code": "...", "requestId": "..." } }`. Inherited management payloads retain `api_version:"3"` envelopes. 400 malformed, 401 invalid session, 403 denied host operation, 404 unknown or unauthorized server, 409 conflict/stale plan/generation, 412 ETag mismatch, 413 oversized, 429 rate limit, 503 unavailable. Fixed codes, no OS errors or paths except owner browse/import responses. `Retry-After` on 429/503.

All new mutations require `Idempotency-Key` (UUID); persist actor+method+route+body hash+result for 24h; same key/different body is 409 `idempotency_conflict`. PAKE messages and session challenge/completion instead use their single-use exchange/challenge ids. Use `If-Match` revision on role, registry and endpoint changes. Limit ordinary JSON to 64 KiB and 100 list entries/page; inherited upload limits remain. Host capabilities advertised in authenticated host response gate later features; absent capability means unsupported, not authorized.

## Router and host surface

| Method/path | Request -> response; authority |
|---|---|
| GET `/v1/host/discovery` | Anonymous `{hostId,fingerprint,protocol:"host-v1",name}`; add `claimState:"unclaimed"|"claimed"` only to directly connected LAN/loopback peers. Do not trust forwarded headers. Outside LAN omit name and claimState. No registry. |
| POST `/v1/host/pake/start`, `/pake/finish` | Under the same `/v1/host` prefix; see identity. Claim starts are possible while unclaimed even outside LAN, but no separate public claim-state response. |
| POST `/v1/host/accounts/challenge`, `/accounts/complete`, `/claim` | PAKE-bound account proof and atomic claim, identity.md. |
| POST `/v1/host/sessions/challenge`, `/sessions` | Device proof -> host bearer; identity.md. |
| DELETE `/v1/host/sessions/current` | Current bearer -> 204. |
| GET `/v1/host/operations/{operationId}` | Owner -> `{operationId,state:"running"|"complete"|"blocked"|"failed",error,authGeneration}` for a host mutation awaiting worker barriers; durable retry/status, never a server-operation alias. |
| GET `/v1/host` | Bearer -> `{hostId,name,fingerprint,endpoints,authGeneration,capabilities,role,revision}`. |
| GET `/v1/host/servers?cursor=...` | Bearer -> `{servers:[{serverId,name,gamePort,state,role}],nextCursor}`; owners see all, others only current server grants. |
| GET `/v1/host/devices` | Owner -> `{devices:[{deviceId,name,accountId,revoked,createdAt}],nextCursor}`; accountId is Minecraft UUID. |
| DELETE `/v1/host/devices/{deviceId}` | Owner -> 204; primary-owner protection and barrier in identity.md. Self-revoke allowed. |
| PUT `/v1/host/owners/{accountId}` | Primary owner, `{role:"owner"}` -> `{authGeneration,revision}`; DELETE demotes owner only. |
| POST `/v1/host/ownership/transfer` | Primary owner, fresh device signature over targetAccountId, nonce, expiry and current revision -> `{authGeneration,revision}`; target must already be an owner. |
| POST `/v1/host/link-codes` | Owner `{targetAccountId}` -> `{linkId,code,expiresAt}`; local privileged `vesta link` may omit target, then approval shows the verified account. |
| POST `/v1/host/links`, GET `/links/{linkId}`, POST `/links/{linkId}/approve` | Identity.md. GET is either owner-authenticated or authenticated with the link's PAKE capability. |
| POST `/v1/servers/{serverId}/invitations` | Admin or owner `{role,expiresAt,maxUses}` -> `{inviteId,invite,expiresAt}`; only server roles, never owner. DELETE same path plus `/{inviteId}` revokes. |
| POST `/v1/host/invites/redeem` | Verified account+device, `{invite,accountProofId,deviceKey,signature}` -> `{hostId,serverId,role,session}`; proof purpose binds hash of exact invite payload. Atomic usage count and grant; never execute pack installation on redemption. |
| GET `/v1/host/realtime` | Authenticated WebSocket host stream, below. |
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
| `/api/v2/realtime/ticket`, `/api/v2/realtime` | Superseded by host realtime; no anonymous feed. |
| `/api/v3/manage/{console/stream,files/follow,metrics/stream}` | Host realtime subscriptions; no per-server WebSocket required. |
| `/api/v3/auth/*`, management pair/enroll/Minecraft/session/password | Host identity endpoints replace them. Pack-password gating, if configured, is additional server policy, not host ownership or device enrollment. |
| management devices/principals/roles/invitations | Host devices and owners use host routes; server role APIs may remain under manage but cannot create owner authority or revoke host devices. Canonical invite creation/redemption is above. |
| `public_resource_packs.rs` `/resource-packs/{digest...}` published URLs | Host adds `/v1/servers/{serverId}` before the existing path; rewrite generated URLs and retain immutable digest policy, byte/range bounds and public exposure setting. Anonymous Minecraft asset delivery is an explicit exception, never file browsing. |
| `/api/v3/setup/*` in `pending.rs` | Removed; claim/import stay on the persistent host listener. |

Host realtime: native client sends bearer in upgrade header. Text messages max 64 KiB. Client `{type:"subscribe",requestId,serverId,channel,cursor}`; channels `console|files|metrics|pack|operations|players`. Host replies `{type:"subscribed",requestId,subscriptionId}` or error, then `{type:"event",subscriptionId,serverId,channel,cursor,payload}` using existing channel payloads. `{type:"unsubscribe",subscriptionId}` removes it. Scope cursors by server+channel+stream epoch; reconnect from cursor or receive `{type:"gap",subscriptionId,reason:"cursor_expired"}` and refetch snapshot. Max 32 subscriptions/host connection and 1 MiB queued; overflow sends gap then closes 1013. Reauthorize every event and operation; revocation closes affected subscriptions before barrier completion. Ping every 30s, close after 60s silence. One connection serves all accessible servers.

## Anonymous exact-port lookup

GET `/v1/here?port=25565`: exactly one decimal port 1..65535, no body. Lookup only the committed registry's effective game-port binding (including sleeping/wake binding), never MOTD/name or a stale import plan. If exactly one server is explicitly public, return `{hostId,fingerprint,serverId,gamePort,pack}`. `pack` is null or `{provider:"modrinth"|"curseforge",projectId,versionId,name}`; no URLs, custom archive, credentials or executable metadata. Distinct multiple-address bindings sharing a port are ambiguous: return the same 404 `not_found` as private/unknown ports. Port or visibility changes invalidate cached lookup atomically. 60 requests/min/IP, 600/min/host; bounded response, no enumeration route, `Cache-Control: no-store`. Launcher caches misses locally 60s and caps discovery attempts at 3s. Result is a hint until pin/PAKE/invite trust is established; pack install always explicit.

## Invites

Transport string `vesta://invite/v1#<base64url(JCS(payload))>`, at most 8 KiB. Fragment avoids HTTP referrer leakage. Payload:

```json
{"v":1,"hostId":"11111111-1111-4111-8111-111111111111","fingerprint":"<64 lowercase hex>","serverId":"22222222-2222-4222-8222-222222222222","endpoints":[{"host":"192.168.1.67","port":25500,"scope":"lan","transport":"https"}],"code":"<32 random bytes base64url>","expiresAt":"2026-11-14T00:00:00Z","pack":null}
```

Endpoint host is IP literal without brackets or DNS hostname, no scheme/path/userinfo; bracket IPv6 when constructing a URL. Scope `lan|public|tunnel`; use actual external management port, never infer 25500 for invite entries. Max eight endpoints. `pack` is null, provider ref above, or `{provider:"vesta",releaseId,sha256}`; custom packs only through authenticated redemption and review. Host stored invite determines role, server, expiry, max uses and pack; mismatching payload rejected. Store only code hash. Code >=256 bits, default 24h/single-use; retry by same bound device returns same grant without another use. Rate-limit misses uniformly. Optional short code requires explicit host endpoint and >=80 random bits, not a mandatory hosted resolver. Fingerprint comes from out-of-band invite; discovery cannot replace it. Launcher tries endpoints without leaking credentials to mismatched certificates and never executes an automatic pack update solely from invite text.

## mDNS

DNS-SD `_vesta._tcp.local.` instance `Vesta-<hostId>`, SRV target machine `.local` hostname, SRV port actual management port, A/AAAA only selected LAN interfaces. TXT keys: `v=1`, `id=<hostId>`, `fp=<fingerprint>`, `name=<display name>`, `state=unclaimed|claimed`, `tls=1`. Name <=63 UTF-8 bytes; each TXT string <=255 bytes, total <=900 bytes. No codes, account ids, paths, server lists or pack metadata. TTL 120s; announce changes and goodbye on shutdown. State advertised LAN only. Ignore TXT for trust; re-resolve after DHCP and confirm host identity over pinned TLS or PAKE. Disable advertising with LocalOnly; manual IP works if multicast is unavailable.

## Conformance gate

Before implementation sign-off, both clients must share fixtures for route mapping, JCS signatures, PAKE confirmations, account-purpose binding, invite encoding and worker frames. Required negative cases: wrong host/server, unknown fields, TLS reconnect/exporter mismatch, simultaneous claim, revoked device/queued action, private-port lookup, ticket replay across servers and malformed IPv6. No implementation or security review is implied by this phase 0 document.
