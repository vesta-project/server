# Optional built-in SFTP

Implementation contract (2026-10-24). The service is disabled by default. This
document is the integration contract for the launcher and host wiring; progress
and verification are recorded at the end as implementation proceeds.

## Management JSON contract

All routes use the existing v3 management bearer authentication and error
envelope. No route accepts local authority from a network address. Configuration
and credentials require a current owner role AND the indicated capability;
authorization is reloaded within each state mutation.

* `GET /api/v3/manage/sftp`: `config.view`; returns the status object below.
* `PATCH /api/v3/manage/sftp`: `security.manage`; accepts a partial configuration
  object with `enabled`, `bind_address` (literal IPv4/IPv6), `port` (1..65535),
  `allow_cidrs`, `deny_cidrs`, and `limits`. Unknown fields are rejected. Returns
  status after applying. Binding failure is explicit and leaves the service off.
* `GET /api/v3/manage/sftp/keys`: `credentials.manage`; returns
  `{ "keys": [ KEY ] }`, including revoked keys, bounded to 256 registrations.
* `POST /api/v3/manage/sftp/keys`: `credentials.manage`; accepts
  `{ "principal_id": "canonical UUID", "label": "Work laptop",
     "public_key": "ssh-ed25519 AAAA...", "expires_at_ms": null }`.
  Expiry may be null or a future Unix epoch millisecond integer. Returns KEY.
  Only public SSH keys are accepted; never send a private key. Keys create a
  dedicated canonical device authentication method, not another user or role.
* `DELETE /api/v3/manage/sftp/keys/{id}`: `credentials.manage`; revokes that
  method and its canonical device. Returns `{ "revoked": true }` idempotently.

KEY: `{ "id": "UUID", "principal_id": "UUID", "label": "Work laptop",
"fingerprint": "SHA256:...", "public_key": "ssh-ed25519 AAAA...",
"created_at_ms": 0, "expires_at_ms": null, "revoked_at_ms": null }`.

Status: `{ "config": { "enabled": false, "bind_address": "127.0.0.1",
"port": 2222, "allow_cidrs": [], "deny_cidrs": [], "limits": {
"sessions": 8, "sessions_per_ip": 2, "handles_per_session": 16,
"transfers_per_principal": 4, "bytes_per_second": 8388608,
"bytes_per_session": 1073741824, "max_file_bytes": 16777216,
"idle_timeout_secs": 120 } }, "running": false, "bound_address": null,
"host_key_fingerprint": null, "active_sessions": 0,
"authentication": ["publickey"], "username": "canonical_principal_uuid" }`.

`limits` is also partial; values outside the documented hard ceilings fail
validation. A configuration change disconnects existing SSH sessions. The
locally generated Ed25519 host key is created on first enable, persists in the
protected state directory, and its SHA-256 fingerprint is returned for pinning.
Disabled startup performs no key generation, opens no listener and spawns no
periodic work. SFTP has its own port; HTTPS proxies do not carry SSH automatically.
Passwords and Minecraft UUID aliases are not enabled in this first version.

## Protocol and policy

Clients select SFTP (SSH), canonical Vesta principal UUID as username, the
configured port, and a registered dedicated SSH private key. The server never
accepts Microsoft, pack, or management passwords. Only the `sftp` subsystem is
available. Shell, exec/SCP, PTY, agent/X11 forwarding and TCP forwarding are refused.
Remote chown/chmod, symlinks, hardlinks and arbitrary metadata mutation are refused.

All filesystem work uses the host's existing FileBrowser instance. Canonical
identity, role, scope, protected-path and link checks are repeated on operations;
credential expiry/revocation is repeated on each request. The same process-wide
mutation admission, quota, trash, ignore and audit behavior applies. File paths
are virtual POSIX paths under the game root; native drive/UNC/ADS and parent
traversal are refused. Directory enumeration is bounded by FileBrowser limits.

Uploads use live semantics: a successful OPEN with truncate creates an empty
visible target, an overwritten original is retained by FileBrowser trash, and
each successful WRITE changes visible bytes. Interruptions leave the partial
target and recoverable original. Writes with stale content identity fail rather
than replacing a concurrently edited file. Per-request IO runs on a bounded
blocking pool; transfer, byte/rate and handle budgets are enforced before IO.
Unsupported optional extensions return SSH_FX_OP_UNSUPPORTED.

## Research and implementation progress

Primary sources consulted: [russh](https://github.com/Eugeny/russh),
[russh SFTP example](https://github.com/Eugeny/russh/blob/main/russh/examples/sftp_server.rs),
[russh-sftp](https://github.com/AspectUnk/russh-sftp),
[SFTP v3](https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-02).
Both libraries provide actual SSH/SFTP wire protocols and server handlers;
protocol handlers must supply Vesta policy and limits themselves.

The management facade, persistent host key, SSH listener, shared FileBrowser
adapter and launcher/native integration are implemented. Focused tests cover
OpenSSH batch interoperability with a pinned host key, create-only uploads,
revocation, malformed packets, shell/tunnel refusal and owned shutdown. A 2 MiB
upload retains exactly one original recovery copy rather than copying every chunk.

Current transfer ceiling is 16 MiB per file, including uploads; this intentionally
matches the existing browser writer's recovery bound. Large world archives need
a separate streaming recovery implementation before raising this bound. Writes
use file identity, length, modification time and active writer versions to reject
concurrent changes. This is not confinement against another process under the
same OS account that can race the underlying files directly.

Management mutations require an Idempotency-Key. Receipts are durable, bound to
the current principal/device and exact input, and capped at 4,096 without eviction.
An interrupted pending request returns `sftp_outcome_unknown` rather than repeating
its effects. Configuration replay returns the original receipt; a fresh GET gives
current listener status. Authority is checked before returning any receipt.
