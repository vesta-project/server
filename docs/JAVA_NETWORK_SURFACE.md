# Java wrapper network surface (migration input)

Read-only inventory of the endpoints the Java host exposes, used to plan the Rust transport wave.
Drafted by a Bunny helper (self-reported `space-bunny-free`) on 2026-10-09; the parent spot-checked
the loopback power bypass, connector link codes, CORS reflection and OP-level mapping against source
(the OP mapping is `core/src/main/java/com/vesta/sync/auth/CapabilityEngine.java:144`, not a wrapper
file). Other line citations are the helper's and unverified.

**Rule for the Rust port:** reproduce the useful pack/discovery/sync behavior, not the authority
shortcuts in section 6. OP status, loopback/LAN location, connector possession, config possession,
bearer possession and open mode never grant management in Rust; management routes stay unavailable
until a canonical device/session boundary exists.

Java listeners: Jetty HTTP/WebSocket (`server/VestaHttpServer`), plaintext TCP sync
(`TcpSyncServer`, optionally on the same port) and loopback control IPC (`ipc/ControlIpcServer`),
wired in `VestaWrapper.java:106-138`. Paths below are under `wrapper/src/main/java/com/vesta/wrapper/`
unless stated.

## 1. HTTP routes (`server/VestaHttpServer.handleRequest`, :258-431)

| Method | Path | Java auth | Request → response | Notes |
|---|---|---|---|---|
| any | `/`, `/devtools` | loopback if `devtoolsLoopbackOnly` | static HTML | dev tooling |
| any | `/api/v1/server/descriptor` | none | descriptor, endpoint map; caps only if authed | exposes TLS fingerprint |
| GET | `/api/v1/modpack/info`, `/manifest`, `/archive`, `/versions`, `/versions/{v}/manifest` | `pack.fetch` | manifest/archive bytes | archive read fully into memory |
| POST | `/api/v1/sync/check` | `pack.fetch` | `current_version`, `installed_hashes` → `needs_update`, `new_version`, `actions`, `manifest` | |
| GET | `/api/v1/files/{sha256}` | `pack.fetch` | file bytes | |
| POST | `/api/v1/files/upload` | `content.manage` | `{path, content_base64}` → `{path, sha256, size, version}` | no body size limit found |
| POST/DELETE | `/api/v1/content/remove` | `content.manage` | `{path}` → `{removed, version}` | |
| GET | `/api/v1/status` | none | security mode, counts, TLS fingerprint | |
| GET | `/api/v1/players` | `stats.read` | player query | |
| POST | `/api/v1/server/power` | loopback **or** `server.power` | `{action: start\|stop\|restart\|kill}` | loopback bypass (:365) |
| GET | `/api/v1/console` | `console.read` | `?since&limit` → lines | |
| POST | `/api/v1/console` | `console.execute` | `{command}` | arbitrary console command |
| GET | `/api/v1/fs/**` | `files.read` | listing / bytes | `files/ServerPathPolicy` |
| PUT/DELETE | `/api/v1/fs/**` | `files.write` | raw body / `{path}` | |
| POST | `/api/v1/auth/token` | none | `{password}` → bearer | open mode issues anonymous token |
| POST | `/api/v1/auth/link` | none | `{code}` → bearer | codes can come from the connector |
| POST | `/api/v1/auth/minecraft/challenge` | none | `{profile_uuid, profile_name}` → challenge | |
| POST | `/api/v1/auth/minecraft/complete` | none (+ `pack.fetch`) | `{challenge_id}` → bearer | Mojang `hasJoined` |
| POST | `/api/v1/realtime/ticket` | bearer + `pack.fetch` + linked UUID | → `{ticket, expires_at}` | scopes incl. console |
| any | `/api/v1/admin/security` | loopback only | security settings | changes mode, password, levels |
| GET | `/api/v1/private/files` | none | 410 | deprecated |

Global: CORS reflects any `Origin` (:1523); no request-size cap configured.

## 2. TCP and WebSocket

TCP codec (`core/.../protocol/TcpMessageCodec.java`): 4-byte big-endian length + JSON, max 16 MiB.
Messages: `handshake` (bearer or pack password; none in open mode) → `handshake_response`
(session id, 90 s TTL); `sync_check`/`sync_check_response`; `file_request`/`file_response`
(returns a download URL); `heartbeat`; `push_update_available` (if `pack_updates` advertised);
`sync_complete`; `disconnect`. Sessions are in-memory UUIDs, not IP-bound.

WebSocket `/api/v1/realtime` (`server/VestaRealtimeSocket.java`): `authenticate{ticket}` → `ready`,
`ping`/`pong`, `status` → `server_status`; server pushes `pack_update`, `console_line`,
`access_changed`. Tickets are single-use, 30 s, server-id bound; bearer and `pack.fetch` are
rechecked per message; 45 s idle, 64 KiB frames, message-rate limit; origin allow-list.

## 3. Connector IPC (`ipc/ControlIpcServer.java`)

Loopback TCP, 4-byte length JSON ≤ 1 MiB, no peer authentication (last connection wins).
`heartbeat`; `permission_check`/`permission_result` (an ALLOW raises the caller's level);
`link_code{code, uuid}` registers a code that `/api/v1/auth/link` redeems into a linked bearer.

## 4. Sessions and credentials

Bearers: 32 random bytes, SHA-256-hashed in memory and in `.vesta/tokens.json`, default 24 h TTL,
revoked wholesale on mode/password/TTL change. Roles: `anonymous`, `operator` (password), `linked`.
Java pack passwords use salted SHA-256 (Rust uses Argon2id). Minecraft challenges: 32-byte nonce,
single-use, server-id bound, Mojang `hasJoined`.

## 5. TLS

Self-signed RSA-2048 certificate generated when `tlsAutoGenerate` and absent, 10-year validity,
PKCS#12 keystore; fingerprint published unauthenticated. TLS disabled or `bindLocalOnly` binds
127.0.0.1. Rust must use explicitly configured certificate material, never regenerated trust.

## 6. Legacy authority shortcuts (do not reproduce)

1. Loopback grants server power (`VestaHttpServer.java:365`).
2. Loopback grants security administration (`/api/v1/admin/security`, :380-386, :1103).
3. Loopback grants devtools.
4. The pack password grants level 4 (full operator) by default (`AccessControlConfig.java:73`).
5. Minecraft OP level maps directly to Vesta level (`core/.../auth/CapabilityEngine.java:144`,
   `OpsJsonReader` reads `ops.json` live).
6. An `operator` bearer is level 4.
7. Config possession defines all authority; the password is held in plaintext in memory.
8. Open security mode authenticates everyone.
9. Connector possession mints link codes and can answer permission checks with ALLOW.
10. `LocalAuthority` is the intended owner bootstrap channel but is unimplemented in Java.
11. CORS reflects any origin for every API response.
