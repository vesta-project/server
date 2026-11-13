# Worker link v1

Worker is one server's control/runtime, not the game-process supervisor and not a host identity authority. Host alone owns TLS, account/device/session DB, registry, invites, recovery and Java catalog. Worker owns server files, operations, packs, backups, sleep and runtime. Keep existing supervisor ownership, launch identity, save leases, orphan refusal and restart journals; host disconnect must not stop Minecraft or start a duplicate. No worker HTTPS or SFTP listener.

## Transport and handshake

Same application frames over Unix socket/Windows pipe now or mTLS stream later. Each frame is uint32 big-endian byte length then UTF-8 JSON; reject zero/>1 MiB, duplicate keys, unknown fields/types/versions before allocation. Binary chunks base64url, <=256 KiB decoded; streams chunk, never load artifacts into a JSON frame. Max 16 inflight requests, 4 MiB queued per worker, control/revocation reserved lane; blocked output cannot starve revocation. Frame read deadline 10s, heartbeats 5s. No arbitrary URL, filesystem root, shell command or executable from the wire.

Host and worker get a random 32-byte per-worker secret at provisioning through inherited handle/root-owned credential file, never arguments or game-readable environment. Local endpoint ACL admits host and worker identities only; verify OS peer credentials in both directions. Linux worker/game cannot read host DB, signing key or privileged recovery socket. Because game code must not steal worker-link authority either, keep link credential and worker private state inaccessible to the game account (separate game uid or equivalent enforced sandbox). Today's same-account supervisor is not this guarantee. Windows uses explicit ACLs; do not merely reuse current-account-only pipe policy for different identities.

Host first sends `{v:1,type:"hello",hostId,workerId,serverId,epoch,nonceH}`; epoch UUID changes on every worker launch. Worker sends `{v:1,type:"hello_ack",workerId,serverId,epoch,nonceW,mac}`; both nonces 32 bytes. `B=JCS({v:1,hostId,workerId,serverId,epoch,nonceH,nonceW})`; worker mac HMAC-SHA256(secret,`"worker\0"||B`), host replies `{v:1,type:"authenticate",mac:HMAC(secret,"host\0"||B)}`. HKDF-SHA256(secret,salt=SHA256(B),info="vesta.worker.link.v1",L=32) yields link key. Subsequent frames `{v:1,seq,body,mac}` use separate increasing sequence counters each direction starting "1"; mac HMAC(linkKey, direction ASCII `H` or `W` || UTF8(seq) || NUL || JCS(body)). Reject gap/replay/wrong epoch; reconnect new nonces and counters, never resume old credentials.

Later mTLS additionally validates both enrolled node certificates and binds host/node/worker/server ids to enrollment registry. Mix the TLS exporter into HKDF salt as SHA256(B||TLS-Exporter("EXPORTER-Vesta-worker-v1",SHA256(B),32)). Application messages and permission rules unchanged. No network listener for this in the first slice. Never expose supervisor's local wire over TCP.

## Messages inside `body`

| Type | Fields and result |
|---|---|
| `policy_sync` | `{generation,grants,revokedDevices}` complete snapshot of this server's effective account/device grants and host-owner expansion; worker durably replaces snapshot -> `policy_ack {generation}`. Initial sync mandatory before serving. |
| `request` | `{requestId,serverId,method,path,headers,subject,deadline,body}` -> `response {requestId,status,headers,body}`. Path is server-relative approved management suffix, never host route. Host strips Authorization/Cookie and external forwarding headers; allowlist content-type, ETag/range/idempotency headers. |
| `subject` in request | `{hostId,serverId,accountId,deviceId,generation,issuedAt,expiresAt,capabilities}`; <=30s lifetime, signed transit via link MAC. Host derives capability set; worker intersects with current policy. No session tokens. |
| `stream_open` | `{requestId,serverId,channel,cursor,subject}` -> stream id or denied; events `{streamId,cursor,payload}`. Worker rechecks grant before delivery. |
| `chunk` / `stream_close` | `{streamId,index,bytes}` / `{streamId,error}`; bounded sequential chunks, backpressure; cancellation does not undo committed effects. |
| `heartbeat` | `{epoch,generation,gameState,supervisorGeneration}` -> ack; no public keys or paths. |
| `drain` | `{requestId,deadline}` -> refuses new mutations, awaits owned operations, reports state. No implicit game kill. Explicit stop is a normal authorized lifecycle request. |

Requests reuse existing server payload/error semantics, operation ids and durable idempotency. No automatic replay of mutations after disconnect without same persisted idempotency key; return/query operation id for accepted jobs. A deadline expiring cancels admission only, never pretends a committed operation was rolled back. Replies/events always scoped by serverId/epoch; host rejects cross-server ids even from a valid link.

## Revocation and outages

Worker executes only if subject generation equals its persisted current generation, device active, capability allowed, server id matches, not expired and policy lease valid. Initial handshake gives no authority. Recheck at actual effect time, including queued operations; don't rely on admission alone. Public/player anonymous actions get narrowly derived policy subjects, not forged owners.

Host commits a grant/revocation and new generation, immediately denies old sessions/streams, pushes full snapshot to every registered worker (generation is host-wide), waits for durable ack before returning successful mutation. Sessions with active devices may continue with freshly resolved authority; their cached generation is not authority. Worker ack occurs after queued actions and subscriptions are invalidated and currently executing authorization-critical effects reach a safe barrier. Completed effects cannot be undone; in-flight long jobs stop at their next safe checkpoint. Unreachable workers keep games running but no privileged management; host returns 503 `revocation_pending` with operation id, not a false global success. Retry/status completes after sync or explicit worker quarantine. Policy lease <=10s, renewed by authenticated heartbeat only for matching generation. Expired lease/disconnect blocks new effects and output; if unreachable worker might retain a lease, host waits lease expiry before declaring revocation barrier complete. Never authorize from stale cached owners after host outage. Persist generation floor across worker restart; no rollback on reconnect.

Tests required: forged peer/MAC, reused nonce/sequence, oversized frame, wrong host/server/epoch, stale and future generation, revoke while queued/streaming, disconnected lease expiry, resync after crash, hung output while revoke arrives, operation retry, host/worker restart with live game and save lease. OS checks must prove mod code cannot read host or worker credentials or reach recovery.
