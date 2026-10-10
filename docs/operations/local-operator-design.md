# Local operator design

## Inventory and authority

| Owner need | Classification | Local boundary |
| --- | --- | --- |
| Status | Read-only | Redacted identity, configured modes; not live process health. |
| Pack status/revision | Read-only | Configured and latest hosted revision/minimum; artifact presence only. |
| Publish pack | Mutating | Canonical local owner authority; explicit publication and audit. |
| Adopt/install server | Mutating | Canonical local owner authority; validated plan, confirmation, durable intent and audit. |
| Check config | Read-only | Validate without preparing state or starting runtime. |
| Pending-intent/recovery view | Read-only | Counts now; any future listing must redact targets and payloads. Reconciliation/retry is mutating. |
| Audit tail | Read-only | Future bounded, redacted projection; never raw metadata, principals, device IDs or targets. |

Read-only inspection needs no administrative authority beyond OS permission to read the redacted state. It must not manufacture authority or initialize/migrate state. Publication, adoption, owner setup, recovery actions and device management remain design only. Named installation and recoverable deletion use the explicitly chosen OS-local boundary described below; they do not appoint a canonical administrative owner.

## Recommended authority model (not implemented)

Implement genuine, explicit one-time owner bootstrap: verify the caller's OS identity using a reviewed native adapter (Windows token SID / Unix effective uid), bind it to the canonical server policy, and restrict the state-directory ACLs/permissions to the required identities. Bootstrap must refuse an already initialized owner policy and must not silently infer an owner from files. Every subsequent mutating command must verify the OS identity against current canonical policy, recheck ACL/policy integrity, resolve capability/confirmation requirements, and audit admission and outcome. Owner replacement/recovery needs a separately designed, explicit, audited procedure; no reset-on-missing-file shortcut.

Mere file ownership is insufficient: files can be copied, created by the agent/service, or replaced by a same-account process. Reading a config, possessing a connector/bearer, owning a process, OP, loopback/LAN access, or Minecraft receipts never grants administration. ACLs protect policy storage; they do not themselves appoint an owner. Microsoft/Minecraft identity remains the normal player flow; pack passwords are pack-access only.

Alternatives include a separately enrolled administrative credential/device with proof of possession and canonical policy, or a privileged OS-mediated management broker with explicit caller authentication. Neither is implemented; an arbitrary bearer or password is not a substitute for enrollment and verification. Production OS-local bootstrap/device-management authentication remains unavailable until genuinely implemented.

### Open owner decisions

Agent/game separation into different OS accounts is P3, not resolved here. Options include running both under one owner account (no same-account isolation), a dedicated agent account with separate game account, or an OS-mediated broker separating operator and service identities. Decide who may read state and which service identities need access without treating those identities as owners. Separately choose administration by enrolled OS identity, enrolled administrative credential/device, or a future broker, and define recovery/rotation. No option is selected on the owner's behalf.

## Implemented CLI boundary

`vesta-agent status <path>` and existing `check-config <path>` are read-only. Strict parsing rejects unknown commands/flags, flag-like config paths, missing arguments, duplicate flags and extra arguments. `serve` remains a runtime command, not an administrative grant. Its protected local-control loop is started only by `serve`; local lifecycle clients do not open another runtime.

Status reads existing current-schema state only, using an immutable read-only SQLite connection. It refuses any WAL, SHM or rollback-journal companion before and after inspection: this initial implementation is for quiescent/offline state, not a live status endpoint. Stop the agent normally before inspection; status does not stop it. No state preparation, ACL tightening, migrations, runner lock, recovery, artifact directory creation, audit append, listener or game execution occurs. Missing state and failures yield fixed redacted errors. No configured paths, secrets, SQL diagnostics, display names, hashes or raw audit payloads are printed. Artifact presence means a plain file of the recorded size, not digest verification or a download grant; artifact and database observations are not one atomic filesystem snapshot. Filesystem checks are point-in-time same-account hygiene, not hostile-race confinement; a caller must keep state quiescent throughout inspection. A future live read-only snapshot API needs its own design rather than ignoring WAL.

Status reports a redacted identity marker, configured pack-access/listener modes, configured and latest hosted pack revision/minimum, latest artifact presence, file-intent pending/recovery-required counts, operation pending/recovery-required counts, and last audit sequence. A detailed audit-tail command is deferred. No launcher, service, firewall, Minecraft or Java changes are included.

## Owner decision (2026-10-10)

For now, local CLI commands rely on the operating-system account, as the owner decided. The owner-bootstrap recommendation above remains the design for remote management and any stronger local authority. See section 21 of docs/SERVER_SOFTWARE_IMPLEMENTATION_PLAN.md.

## Basic OS-local commands

```text
vesta-agent version
vesta-agent update
vesta-agent create <name> --install-root <root> --minecraft-version <version> --java <path>
    [--loader <vanilla|paper|folia|purpur|fabric|quilt>] [--loader-version <version>]
    [--bind <loopback-ip>] [--port <port>] [--approve-install]
    [--ack-warnings] [--ack-legacy-checksum]
vesta-agent delete <name> --confirm <name> [--install-root <root>]
vesta-agent config <name> [--install-root <root>]
vesta-agent start <name> [--install-root <root>]
vesta-agent stop <name> [--install-root <root>]
vesta-agent restart <name> [--install-root <root>]
vesta-agent serve <path> [--local-only]
```

Names are bounded lowercase ASCII letters/digits/hyphens, start with a letter/digit, and exclude Windows device names. Named commands operate on the registry created by `create`, not an arbitrary folder or adopted existing server. Except for `create` (which requires an explicit root), root selection is `--install-root`, then `VESTA_SERVER_ROOT`, then `vesta-servers` under the caller's current directory. The selected root must be an existing, explicitly server-only local directory; client/profile exclusions cannot be inferred from a name. `create` validates this root with `InstallRoots`, takes a local registry lease, resolves the installer plan, and passes explicit install/warning/checksum flags to `Approval::review`. The selected existing Java is probed by the production installer; no game is launched. Forge/NeoForge are refused because the automatic installer does not support them. `--loader-version` applies only to Fabric/Quilt.

Create checks the configured loopback bind/port using a bound socket without calling listen or accept. Defaults are `127.0.0.1:25565`; zero/public binds are refused. Availability is point-in-time, not a port reservation. Installation writes fresh host/launch documents, a name record, loopback-only game properties, and `eula=false`. It never changes an existing EULA. The owner must read and accept it themselves. The emitted host path is used with `serve <path> --local-only`; this mode opens no TCP transport, regardless of the configured transport. Existing `serve <path>` still requires TLS, now on loopback only, and still refuses a disabled listener without `--local-only`. Local start/restart also refuse pending/missing/ambiguous EULA files. A successful Start is ownership, not readiness.

Before opening runtime ownership, `serve` persists an `operator-session-pending` marker for each managed instance while holding its external lease. Only proven runtime and I/O shutdown archives that marker into `operator-sessions`; crash/unwind/error does not erase it. A retained marker refuses subsequent `serve` and deletion with `local_shutdown_unproven_requires_supervisor_recovery`: released OS locks alone cannot prove an orphaned game stopped. No automatic marker reset, orphan adoption or supervisor recovery is implemented; that remains reviewed P3 work.

Delete checks exact typed confirmation, takes the registry and external instance leases, and refuses an unproven prior shutdown or a running agent/runner (including a serving-but-currently-stopped host, conservatively). It moves the entire instance and its name record into `<root>/recoverable/<uuid>/server` and `name.json`. No recursive removal or world deletion occurs. If an existing state database is available, the existing audit chain records admission and the successful recoverable move; no new database is manufactured merely to audit a never-served install. The two filesystem moves and database audit are not one atomic transaction: partial failures retain bytes for manual recovery. Failed final auditing is explicitly reported as a completed move with failed audit outcome. Recovery/re-adoption is not automated. A failed post-install publication similarly retains an unregistered installer instance rather than erasing it.

Config opens the protected host configuration file, never dumps its contents. `EDITOR` takes precedence over `VISUAL`; either must be a literal executable path/name, not a shell command or argument string. Set-but-invalid values refuse. Fallbacks are PATH-detected `nano`, then `vi`, on Linux, or `notepad.exe` on Windows. Windows scripts/batch files and relative executable paths are refused; configured absolute executable paths containing spaces are supported. The selected editor controls its own interactive I/O.

Local IPC is a four-byte big-endian length followed by JSON, capped at 1024 bytes. Version 1 requests have only `version`, `op` (`start`, `stop`, `restart`) and `name`; responses contain only `version` and a closed redacted result code. Frame I/O is time-bounded and unknown fields/operations are refused. Windows uses a local-only named pipe with an atomic protected current-token-SID-only DACL; clients also check the server process token SID. Linux uses an owner-only state directory and mode-0600 Unix socket, checks peer uid in both directions, refuses stale sockets instead of unlinking unknown endpoints, and cleans up only the socket inode created by this agent. Same-account processes remain trusted; this is not game/agent isolation. No TCP IPC, remote administration, service or firewall setup is added.

The bounded local mailbox serializes with the existing runtime's sole owned process driver, refuses active competing operations, and uses its configured launch/owned graceful-stop primitives. It never selects commands/paths/arguments from lifecycle IPC or silently escalates stop/restart to force-kill. Restart waits for actual exit and I/O completion before starting a new generation; a stop timeout retains the old generation and does not restart it. A disconnected/timed-out client can have an unknown admitted outcome and must not blindly retry. `version` says updates are not signed; `update` reports the current version and available version as unknown, exits refused, and never replaces a binary (F41).

P3 still owns durable per-server supervisors, process-tree confinement/continuity, restart/reboot recovery and readiness. This first IPC adapter is not those features. Agent shutdown retains live ownership and may wait for an uncooperative child; stop the game through the running agent before shutting the agent down. Linux Unix-socket path limits and supported-local-filesystem restrictions apply; Windows and Linux native IPC need their own platform gates.


## P2 local owner setup (2026-10-11)

The binding owner decision now treats the CLI OS account as owner for protected
local `pair`, `devices list/revoke`, `owner link/unlink/list`, and `access show/set` commands. They
require a running `serve` and reuse the native current-account-only control
channel; no HTTP link/setup route or offline direct-state shortcut exists.
See [management API contract section 2](management-api-contract.md#2-authentication--implemented-unified-users-sessions-and-permissions)
for exact grammar, bounds, key enrollment/session shapes, unlink/revocation
semantics, live TLS fingerprint comparison, safe automatic role sources and the
single canonical session used by pack/download/realtime and management.
This supersedes this document's older statements that all owner setup and device
management are design only. It does not implement permanent SID/UID enrollment,
stronger local recovery, same-account game isolation or operational remote effects.

Local IPC still bounds requests to 1024 bytes. Identity/list responses may be up
to 1 MiB (at most 1000 devices / 32 owner links); lifecycle responses remain small.
Identity operations serialize on the protected channel and own their canonical
policy/mutation/audit write transaction. Native peer checks remain unchanged.
A client timeout/disconnect can leave an unknown admitted result; do not retry
blindly or assume an unseen pairing code was never created.
