# Launcher integration, first host slice

The native `vesta-host-client` crate provides PAKE and identity signing bytes. It
intentionally provides no HTTP stack, certificate exception, keychain or Mojang
access token handling. Use native HTTPS/WebSocket I/O and an Ed25519 device key.
All binary signatures and public keys use `vesta_host_client::encode`.

## Claim once

1. Connect to `https://address:25500` (or the explicit port). GET
   `/v1/host/discovery` gives `hostId`; discovery is a hint. Use one dedicated
   TLS 1.3 connection with resumption and early data disabled for steps 2–6.
2. `SetupCode::parse(code)`, then `Client::start(&code, Purpose::Claim, None,
   hostId, device_public_key)` returns `(client, Start)`. POST Start to
   `/v1/host/pake/start`, deserialize `Started`.
3. Obtain the observed leaf certificate SHA-256 and 32 exporter bytes from that
   same TLS connection: label `EXPORTER_LABEL`, context empty bytes (present).
   `client.finish(&started, &fingerprint, &exporter)` returns confirmation state
   and `Finish`. POST Finish to `/v1/host/pake/finish`; deserialize `Finished`.
   `confirmation.verify(finished)` returns the PAKE token. Restrict any temporary
   certificate exception to this bootstrap connection and these identity calls.
4. POST `/v1/host/accounts/challenge` with `Authorization: Pake <token>` and
   `{accountId,name,deviceKey,purpose:"claim",binding:started.exchangeId}`.
   Call Mojang session `join` using the signed-in Java profile/access token and
   returned `serverHash`. Never send that access token to Vesta Server.
5. Sign `requests::signing_bytes("vesta.host.account.v1", value)` where value is
   `{hostId,deviceKey,challengeId,serverHash,purpose:"claim",binding}`. POST
   `{challengeId,signature}` to `/v1/host/accounts/complete`, same Pake header.
6. Sign `requests::signing_bytes("vesta.host.claim.v1", value)` where value is
   `{hostId,exchangeId,accountProofId,deviceKey,deviceName}`. POST
   `{exchangeId,accountProofId,deviceName,signature}` to `/v1/host/claim` with
   the same Pake header. Persist the proven fingerprint, hostId, deviceId and
   private key after success. Response includes a host session. An identical
   lost-reply retry is supported on this connection; a new claim is refused.

## Sign in on subsequent launches

Use pinned TLS; no setup code or online account proof is required. POST
`/v1/host/sessions/challenge` with `{deviceId}`. Sign
`requests::signing_bytes("vesta.host.session.v1", {hostId,deviceId,challengeId,
nonce,expiresAt})`. POST `{deviceId,challengeId,signature}` to
`/v1/host/sessions`. Use `Authorization: Bearer <session>` thereafter. Sessions
last at most 15 minutes and do not survive process restart. Repeat this sequence
on expiry. DELETE `/v1/host/sessions/current` logs out. If an enrollment reply
lost deviceId, send `{deviceKey}` to the challenge route; its response also
includes deviceId, but a fresh possession signature is still mandatory.

## Servers and management

GET `/v1/host`, then GET `/v1/host/servers`. The latter returns only accessible
servers; owners also see servers registered later. Store `(hostId,serverId)`.
For example GET `/v1/servers/{serverId}/manage/server/status`, or GET
`/v1/servers/{serverId}/manage/server` for branding. Replace only the old
`/api/v3/manage/` prefix with `/v1/servers/{serverId}/manage/`. Retain feature
payloads, `Idempotency-Key`, `If-Match`, `If-None-Match`, `Range`, and `If-Range`.
Unknown and inaccessible ids both return 404 `not_found`.
Discovery is POST `/v1/servers/{serverId}/discovery` with the existing discovery
request. Pack status is GET `/v1/servers/{serverId}/pack/status?installed=N`.
POST `/v1/servers/{serverId}/pack/ticket` returns a 30-second, single-use ticket
and prefixed artifactUrl; GET that URL with `Authorization: Bearer <ticket>`.
A current host session may also authenticate the artifact GET directly.

## Realtime for this slice

The host multiplexer is deferred (authenticated GET `/v1/host/realtime` returns
501 `realtime_multiplexer_deferred`). Open native WebSockets with the host bearer
in the upgrade header, no Origin/Cookie or credentials in URLs:

- `/v1/servers/{serverId}/manage/console/stream`
- `/v1/servers/{serverId}/manage/files/follow?path=<percent-encoded-relative-path>&cursor=<cursor>`
- `/v1/servers/{serverId}/manage/metrics/stream`
- `/v1/servers/{serverId}/realtime?installed=N` (pack events)

These preserve existing per-server channel frames/cursors, without a subscribe
message or host multiplex envelope. Files follow uses the existing query schema;
console/metrics accept no query or `installed=0`. Connections close within 29s:
reconnect with a current session and refetch snapshots/reconcile cursors. Initial
worker subjects expire after 30s. Device/session revocation is checked at 250ms
intervals; the H07 full role-revocation/queued-action barrier remains deferred.
Cap is 32 host streams. Pack channels use an internal single-use ticket; launchers
only supply the host session. No `/realtime/ticket` call is needed.

## Host process configuration

Run `vesta-agent host /path/to/host.toml`. Minimal configuration (paths relative
to the config file; certificate/key must already exist):

```toml
state = 'host-private'
certificate = 'tls/server.pem'
private-key = 'tls/server-key.pem'
# bind = '0.0.0.0'
# port = 25500
name = 'people-server'
local-only = false
import-roots = ['/srv/vesta-imports']
discovery-hostname = 'people-server.local.'
discovery-interfaces = ['eth0']
# Required when a server is registered:
# worker-config = 'servers/survival/server.toml'
```

LocalOnly forces loopback and disables mDNS. Explicit interface selection is
required for mDNS; an empty list leaves manual IP connections available. The
same HostIdentity serves HTTPS and the protected local CLI channel. The process
prints a fresh setup code while unclaimed. SIGINT/Ctrl-C shuts down discovery,
local control, HTTPS, then the retained worker/supervisor owner. No service is
installed. Registry-to-worker composition at startup currently admits one server;
H16 wiring must call `HostRouter::add_worker` for a server imported while running.
