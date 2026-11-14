# Launcher integration, first host slice

The native `vesta-host-client` crate provides PAKE and identity signing bytes. It
intentionally provides no HTTP stack, certificate exception, keychain or Mojang
access token handling. Use native HTTPS/WebSocket I/O and an Ed25519 device key.
All binary signatures and public keys use `vesta_host_client::encode`.

## Claim once

1. Connect to `https://address:25500` (or the explicit port). GET
   `/v1/host/discovery` gives `hostId`; discovery is a hint. Use one dedicated
   TLS 1.3 connection with resumption and early data disabled for steps 2�6.
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

## Sign in on a new device (no code)

For a claimed machine the user already has a role on (owner or any server
role). Permissions follow the Minecraft account; no link code or approval.
Generate a fresh Ed25519 key for this machine (never reuse one across hosts).
Use one dedicated TLS 1.3 connection (resumption and early data disabled) for
steps 1-5; no Authorization header until the session.

1. Connect and record the observed leaf certificate SHA-256 `F`. GET
   `/v1/host/discovery`; require `fingerprint == F`. Decide the pin for
   `(hostId, F)`: from an invite/link the launcher holds, from the same machine
   already known on another profile, or by trust on first use. For first use,
   show the machine name and address and let the user confirm before any account
   step. An existing pin that differs from `F` is a "machine identity changed"
   error; never re-pin silently.
2. POST `/v1/host/accounts/challenge`
   `{accountId,name,deviceKey,purpose:"signin",binding:F}` (no Authorization).
   Reply `{challengeId,serverHash,nonce,expiresAt}`.
3. Compute `requests::account_server_hash(hostId, deviceKey, "signin", F,
   accountId, nonce)` from your own values. If it differs from `serverHash`,
   abort and do not call Mojang. Otherwise call Mojang session `join` with
   `serverHash`.
4. Sign `requests::signing_bytes("vesta.host.account.v1", value)` where value is
   `{hostId,deviceKey,challengeId,serverHash,purpose:"signin",binding:F}`. POST
   `{challengeId,signature}` to `/v1/host/accounts/complete` -> `{accountProofId,accountId,expiresAt}`.
5. Sign `requests::signing_bytes("vesta.host.signin.v1", value)` where value is
   `{hostId,fingerprint:F,accountProofId,deviceKey,deviceName}`. POST
   `{accountProofId,deviceKey,deviceName,signature}` to `/v1/host/sign-in`.
   200 `{state:"enrolled",hostId,fingerprint,accountId,deviceId,deviceName,session,expiresAt,authGeneration}`:
   persist pin, hostId, deviceId and key; use the session. `deviceName` is the
   stored (sanitised, at most 64 characters) name.

Errors: 403 `account_has_no_access` (this account has no role here: "Ask an
owner to give you access"); 403 `device_approval_required` (the owner requires
approval: fall back to the link-code flow, identity.md "Device link"); 409 `device_key_unusable`
(make a new key and restart at step 2); 401 `pairing_failed` (expired, reused or
mismatched proof: restart at step 2); 429 `rate_limited` (5 challenges per minute
per address); 503 `account_verification_unavailable` (Mojang unreachable). If the
step 5 reply is lost, recover the deviceId with the `{deviceKey}` session
challenge below. Owners see the new device in GET `/v1/host/devices` and can
revoke it; a revoked key must be replaced by a new one.

Owner settings: GET `/v1/host` shows owners `newDeviceSignIn`. PATCH `/v1/host`
`{name?,newDeviceSignIn?}` renames the machine (1-63 UTF-8 bytes) or switches
between `"account"` (default) and `"approval"`. GET `/v1/host/owners` lists
`{accountId,role,name}` for the primary owner and owners.

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

Your own permissions on one server: GET `/v1/servers/{serverId}/manage/session`
(no body or query) returns
`{server_id,principal_id,device_id,roles:[{id}],capabilities:[id],session_expires_at_ms}`,
the same shape as the old per-server session projection, computed from the
trusted server subject the host authorized for this request. Owners and the
primary owner hold role `owner` with every capability of the server's services;
server roles get exactly their grants (Admin as `administrator`, Moderator as
`moderator`, Player as `pack-user`). Identity administration (roles, devices,
invitations, credentials) is never a server capability; it lives on host
routes. A caller with no role on the server gets 404 `not_found`. Use this
instead of client-side role presets.

### After the Vesta Server restarts

A Vesta Server machine brings its servers back by itself after a reboot or an
in-place update. By default a server that was running (or starting) when the
Vesta Server stopped is started again when it comes back; a server the owner
stopped stays stopped; a sleeping server sleeps again (when it can still
sleep). "Running" means the last successful start, restart or wake was not
followed by a stop, sleep or kill. A server whose crash loop suspended
automatic restarts is not started again. Nothing is started twice: a server
whose game is still running is left alone.

An explicit startup policy always wins over this default: the sleep policy's
`on_agent_start` (`stay_stopped`, `sleep` or `start`, set through the existing
sleep/policy document) or the server's host.toml `[process]` setting. Status
keeps reporting `on_agent_start` as before (`stay_stopped` when nothing is set
explicitly), so show the default as "Comes back as it was" when the sleep
policy's `onAgentStart` is null, rather than as "Stay stopped". Clients need no
action after a restart: poll the server list or status as usual.

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

## Import wizard (owner only)

Every call uses the host bearer. Show the wizard only when GET `/v1/host` lists
`import-jobs` in `capabilities` and `role` is `primary_owner` or `owner`. Poll at
most every 2 seconds: the host allows 60 requests per minute per address and
answers 429 `rate_limited` (with `Retry-After`) beyond that.

1. Pick a folder. GET `/v1/host/imports/candidates` for detected AMP or
   Minecraft folders (each with a `folderToken`), or browse: GET
   `/v1/host/folders/roots`, POST `/v1/host/folders/resolve`
   `{rootId,relativePath:""}`, then POST `/v1/host/folders/list`
   `{folderToken,cursor:null}` and descend with each entry's `folderToken`.
   Tokens expire after 10 minutes; resolve again on `folder_token_expired`.
   Candidate warnings with a `remediation.command` (for example
   `sudo vesta import-root add '<path>'`) are for someone at the server's own
   terminal: show the command with a copy button, never run it.
2. Optional preview: POST `/v1/host/imports/inspect` `{folderToken,gamePort:null}`
   returns what was detected without starting a job.
3. Plan: POST `/v1/host/imports/plan`
   `{folderToken,name,gamePort:null,memoryMiB:null,launchChoice:null,minecraftVersion:null}`
   gives 202 `{jobId,state:"planning"}`. Poll GET `/v1/host/imports/jobs/{jobId}`
   until `state` is not `planning`. Show `progress` (files and bytes read) while
   it hashes a large folder.
   - `review`: show `review` (detected software, Java, port, memory, destination,
     size) and every `review.warnings` entry as a checkbox. Ask the owner to
     accept the Minecraft EULA and to confirm the original server is stopped
     and will not auto-restart.
   - `blocked`: show `review.blockers` (table below). Every fix needs a new plan:
     go back to step 3 with the same or a fresh `folderToken`.
   - `failed`: show `error` (usually a folder token problem); start over.
4. Apply: POST `/v1/host/imports/apply` with header `Idempotency-Key: <random>`
   and `{planId,confirmation,acknowledgedWarnings:<all review.warnings>,eulaAccepted:true,sourceStoppedConfirmed:true}`.
   202 `{jobId,serverId}`. Retry a lost reply with the same key and body. Poll
   the job: `applying` with `phase` `conflicts`, `snapshot`, `copy`,
   `configure`, `register`; show `progress` as a bar (bytesDone/bytesTotal).
   A Cancel button calls POST `/v1/host/imports/jobs/{jobId}/cancel`; `applying`
   in the reply means it stops at the next file, keep polling until `cancelled`.
   The original folder is never changed.
5. When the job is `ready` (phase `register`) the server already appears in
   GET `/v1/host/servers`. Start: POST `/v1/host/imports/jobs/{jobId}/start`
   (202). If the reply is `ready` with an `error`, show it and offer Start
   again after the owner fixes it. Otherwise poll the job while `starting`
   (startup can take minutes) until `online`, or `ready` with `start_failed`.
6. From `online` on, manage the server through the normal
   `/v1/servers/{serverId}/manage/...` routes (console, files, players,
   lifecycle). GET `/v1/host/servers` shows `state` `starting`, `online`,
   `stopped` or `failed` as the host observes it.

If a job is `interrupted` (the host restarted), offer Resume: POST
`/v1/host/imports/jobs/{jobId}/resume` (202) and keep polling; a job interrupted
while planning comes back `failed` with `replan_required`.

Blockers and job errors, with suggested wording:

| Code | Tell the owner |
|---|---|
| `source_running` | The original server is still running. Stop it (and turn off auto-restart in AMP), then check again. |
| `source_process_unknown` (warning) | Vesta cannot inspect a possible Java server process. Confirm that you stopped the source server. |
| `source_port_listening` / `source_recently_active` | Stop the server, wait a minute, then check again. |
| `source_unreadable` | Vesta Server can't read this folder. Ask someone at the server to run the shown `vesta import-root add` command, or copy the folder into an import folder. |
| `source_changed` | Files changed since the check. Make sure the original server is stopped, then check again. |
| `source_too_large` | This folder has too many files to import (over 2 million, or a file over 16 GiB). |
| `unsafe_path` | The folder contains links or special files that can't be copied safely. Remove them from a copy and try that copy. |
| `not_minecraft_server` | This doesn't look like a Minecraft server folder. Pick the folder that contains `server.properties`. |
| `software_unresolved` / `launch_unresolved` | Vesta Server couldn't tell which server jar to run. Choose the jar (and Minecraft version), then check again (`launchChoice`, `minecraftVersion`). |
| `java_unresolved` | No matching Java was found. Choose the Minecraft version so the right Java can be downloaded. |
| `insufficient_space` | Not enough free disk space on the host for the copy. Free some space and check again. |
| `game_port_in_use` / `port_state_unknown` / `port_unresolved` | The game port is busy or couldn't be checked. Pick another port or stop whatever uses it. |
| `memory_budget_exceeded` / `memory_invalid` / `memory_unresolved` | That memory amount doesn't fit this machine (at least 512 MiB). Pick a smaller amount. |
| `server_capacity_reached` | This Vesta Server already manages a server. |
| `plan_expired` / `confirmation_invalid` / `plan_blocked` | The check is out of date. Check again. |
| `copy_failed` / `state_unavailable` | The copy was interrupted. Resume to continue where it stopped. |
| `destination_conflict` | A previous attempt left files in the way. Start a new import. |
| `configure_failed` | Vesta Server couldn't prepare the copied server. Resume to retry. |
| `start_failed` | The server stopped while starting. Check the console for the reason, then Start again. |
| `replan_required` | The check was interrupted. Check again. |

Warnings (all must be acknowledged): `eula_not_inherited` (the owner accepts the
EULA here, the original's answer is not reused), `rcon_disabled` (remote console
is turned off in the copy; the Vesta console works), `amp_controller_running`
(AMP itself may keep running), `disable_amp_auto_restart` (turn off AMP's
auto-start for this instance), `auxiliary_port_in_use` (a query port is busy).

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
installed. At startup every registered imported server (managed `<servers>/<serverId>/host.toml`)
gets its worker back; `worker-config` still serves one externally configured server.
Optional keys: `servers = 'servers'` (managed import storage) and
`java-cache = 'java'` (checksum-verified managed Java), both relative to host.toml.


Import stopped-state codes and suggested text:
- `source_process_unknown` is a warning requiring the existing explicit source-stopped confirmation and warning acknowledgement at apply: "Vesta cannot inspect a possible Java server process. Confirm that you stopped the source server."
- `source_running` remains a blocker for a process positively correlated with the source.
- `source_port_listening` blocks independently of the destination port: "Something is listening on the source game port. Stop the server, then check again."
- `source_recently_active` blocks: "The world is locked or world/log files changed within the last 60 seconds. Stop the server, wait a minute, then check again."
- `world_outside_source`: "Choose a folder containing the world and use a relative level-name inside that folder."
- `bind_address_cleared` warns: "The imported server will bind to all interfaces; Vesta clears server-ip in the copy."
- `launch_arguments_unsupported`: "The launch arguments contain unsupported options or paths. Use a self-contained server folder with supported Forge arguments, then check again."

### Effective startup policy

Server status `on_agent_start` is `restore_previous` when no stored or host.toml policy was explicitly chosen. This means resume the server's last lifecycle state after an agent restart. Explicit `stay_stopped`, `sleep`, and `start` choices and the local stay-stopped override retain their meaning.

Provider transfers use a 3s connect timeout, 120s idle read timeout, and a bounded overall job deadline. Idempotent metadata GETs retry transient transport/429/5xx failures with backoff. Installer catalogue queries retain their existing sub-15s total budget and persisted catalogue cache; installation metadata and artifacts use the longer job budget. Pack downloads retain destination validation, byte limits and their caller deadline.
