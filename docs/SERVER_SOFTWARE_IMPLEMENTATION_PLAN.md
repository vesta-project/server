# Vesta Server Software — Complete Implementation Plan

Status: completed planning baseline, 2026-10-04. This document authorizes no code
implementation or deployment. The detailed accepted requirements remain in
[SERVER_SOFTWARE_REQUIREMENTS.md](SERVER_SOFTWARE_REQUIREMENTS.md).
[SERVER_SOFTWARE_STATUS.json](SERVER_SOFTWARE_STATUS.json) is the dated source
for the HTML roadmap's status assessment.

## 0. Owner direction (2026-10-07)

The owner clarified the intended product shape. Where this section conflicts with
older text below, this section wins.

1. **One script installs the backend.** A single install script sets up the
   server-side backend (management agent, and later the supervisor) on the host,
   normally as a system service. Headless/answer-file installs use the same flow.
2. **After install, everything is authenticated remote management.** Clients only
   need the server address and sign-in. Microsoft/Minecraft identity (prove the
   Minecraft UUID, then resolve its current server-local role) is the expected
   common method; pack passwords are an alternative, not administrative
   authority. The launcher is the first client. **A web app is planned later**
   that needs only an IP/address and authentication. This supersedes the
   "No remote web panel is planned" sentence in section 1; the web app still
   uses the same authenticated agent API and authority checks, never a weaker path.
3. **Onboarding includes server-software installation.** Picking software and a
   version during onboarding installs it automatically on the backend.
4. **Clients signal desired changes; the backend does the work.** A client sends
   only *what* should change (software, Minecraft version, loader version,
   experimental opt-in, acknowledgements). The backend resolves metadata, plans,
   returns a review (EULA/licence, checksum notes, Java requirement), the owner
   confirms that exact plan, and the backend downloads, verifies and installs.
   Clients never send URLs, digests, sizes, library lists, launch arguments or
   artifact paths.
5. **Changes while the game is running: prepare now, apply when stopped.**
   Planning, download and verification may run while players are online. Switching
   the active software is locked while Minecraft runs and is shown as a pending
   change ("stop the server to apply"). Nothing stops or restarts the game
   automatically, consistent with the existing no-auto-restart rule.
6. **World and configuration persist across software changes.** Software versions
   are installed side by side; the active version is switched, the world/config
   directory is kept. A pre-change backup is taken (Minecraft upgrades worlds one
   way) and the previous software version stays available for rollback.
7. **Rust host-wrapper migration is the next direction.** The owner requested a
   Rust rebuild up to the current Java implementation checkpoint, with the Java
   wrapper/core and Minecraft plugin/mod retained for comparison and interoperability.
   This does not mean every unfinished product requirement is implemented, nor that
   known unsafe legacy trust routes should be copied. The existing Rust installer
   (`piston-lib::game::server_install` in the launcher repository) remains the
   download/verification reference; the Rust agent should reuse a transport-neutral
   library rather than require the whole launcher engine or an unselected Java/Rust
   sidecar. Crate extraction and Rust parity must earn fresh regression evidence.
   See [the Rust migration plan](archive/RUST_WRAPPER_MIGRATION_PLAN.md) and its checked
   file/test accounting; Java-to-Rust translation does not promote the original ledger.
8. **Migration model allocation and review.** Use GPT-6 Luna for bounded mechanical
   work, including existing-test translations, with clear file ownership and parent
   verification/cleanup. Use GPT-6.1 Sol for serious backend work and the final
   independent review. No Anthropic models are to be used for this migration. Preserve
   every original requirement ID and historic evidence; translated tests alone never
   establish complete requirement parity or release readiness.

## 1. Outcome and scope

Build a self-hosted Minecraft Java server management product with the launcher as
its panel. Server operation, authentication, automation, recovery, notifications,
and pack hosting must not require a launcher connection, Vesta account, external
database, or Vesta-operated service. Target individual owners first and community
administrators second; do not turn setup into a hosting-provider control plane.

The first complete release manages one Minecraft process per independently
secured agent instance on Windows and Linux. A stable server_id scopes every
principal, operation, event, pack revision, credential, and storage resource.
Multi-server hosting, Velocity topology, a bring-your-own relay, mobile QR flows,
and a general privileged plugin ecosystem are later work, not missing initial
release essentials.

No remote web panel is planned. CLI and optional strictly local management tools
are supported. Launcher UI work is explicitly out of scope: document the protocol,
capability reporting and expected operator experience, but do not implement it.

## 2. What a complete experience looks like

### Owner: install or adopt

1. Choose an existing directory or a new server implementation/version.
2. Detect Java, launch arguments, ports and supported connector. Show recommended
   settings together; place detailed network/security knobs under Advanced.
3. Preserve worlds, settings and existing scripts. Inspect supported start-script
   patterns rather than executing arbitrary scripts for discovery.
4. Install the agent and supervisor as one product; optionally add the compatible
   connector with explicit consent. The normal flow needs no IPC/script knowledge.
5. Pair the initial owner's device using a local single-use code. Management is
   closed until bootstrap is complete; opening a port never creates an admin.
6. Explain the chosen Local/LAN/internet exposure. Port forwarding is sufficient
   only with a reachable address and suitable router/firewall access. Diagnose
   CGNAT, double NAT and unsupported proxy routes rather than claiming success.
7. Offer opt-in backups, SFTP, connector and integration presets. Secure service
   installation is recommended; portable operation honestly reports weaker
   isolation. Do not require extra everyday credentials for generated backup keys.
8. Adopt process control on an approved restart, not by killing an existing
   server. Keep existing service ownership when an adapter is explicitly chosen.

### Operator: ordinary daily use

The panel can show lifecycle/readiness, player state, metrics, pending restart
changes, task progress, console/log access, files, hosted pack and notifications.
Each surface is capability-gated. A Pack User receives ordinary public-style
status and its pack access, not player names or sensitive operational data.

Changing settings/files does not restart anything. Start, Stop, Restart and Kill
are explicit operations; a timed-out Stop remains visible and offers a separately
confirmed Kill. A restart issues stop, waits for actual exit, applies approved
pending changes, starts once, then checks readiness.

### Pack maintainer: client distribution

Edit a separate manifest/overrides workspace. Provider mods are exact metadata
references; custom files are overrides. Server mod changes produce suggestions,
never automatic pack edits. Manual Build/Publish shows a diff, verifies and signs
a complete artifact, increments revision, then makes it available immediately.
Optional scheduled release activates an approved frozen artifact later.

Only one revision is publicly hosted. Existing clients can remain on a previously
installed revision if it is at or above the minimum required revision. A minimum
revision is an update/compatibility contract, not proof of the client filesystem
or a universal barrier against other launchers joining Minecraft.

### Moderator and delegated helpers

Keep Owner, Administrator, Maintainer, Moderator, Viewer and Pack User presets.
Use one primary role and individual overrides initially, with multiple-role-ready
storage. Explicit deny wins. Maintainer is a trusted operator because executable
content installation can run arbitrary code under the game identity. Narrower
custom roles are available. Moderator has typed player actions and online-player
visibility, but no raw console or broad file access by default.

Owner/Administrator grants and recovery originate locally. Authorized delegation
may grant only lower-rank roles within the delegator's effective capabilities and
scopes. Moderator presets never implicitly grant credential, role or OP authority.

### Owner: recover deliberately

Disconnecting a launcher changes nothing about running work. An agent crash does
not stop Minecraft. Reconnection reconciles generations, operations, logs and
notifications before resuming automation. Missed schedule defaults are
skip-and-notify, configurable per workflow.

Corrupt security/launch state fails closed. Ordinary Minecraft failure does not
automatically restore a backup. Local recovery can repair state, revoke all admin
access, or create a new owner pairing with a protected audit event. Restore is a
separate owner-confirmed workflow with a pre-restore safety backup where possible.

## 3. Architecture and trust boundaries

One installer provides distinct internal components:

- Management agent: HTTPS/events, principals, roles, vault, configuration,
  operations, workflows, notifications, pack hosting and storage orchestration.
- Process supervisor: game launch/identity, stdin, continuous output draining,
  10,000-line bounded live buffer, exit observation and crash-safe save leases.
  It has no network listener. Its only control surface is ACL-protected local IPC.
- Optional connector: loader/server-specific player/tick/admission/flush signals.
  It shares the game trust boundary and cannot grant management authority.
- OS helper: only where platform capabilities require privileged service setup,
  process identity switching, firewall changes or snapshot creation. Keep its
  operations narrowly typed; it is not a remote general shell.
- Local CLI: authenticated OS-local bootstrap, recovery, status and owner controls.
  Loopback TCP location is not itself an administrative credential.

The launcher authenticates to the agent. The agent controls the supervisor.
The supervisor owns Minecraft. The connector sends constrained adapter signals
back to Vesta; it never owns roles, secrets or update approval. Backup/content
workers obtain short-lived operation scopes rather than general supervisor power.

Retain Java/Jetty/core codecs where useful instead of choosing an unnecessary
rewrite. Keep the modern agent runtime separate from old game Java requirements.
Prove Windows named-pipe identity/ACL behavior and Linux Unix-domain socket peer
permissions in P3 before fixing the cross-platform supervisor package format.

Secure installation separates game and control-plane OS identities. The game
identity cannot read/write Vesta vault/state/update artifacts, approved scripts
or supervisor control IPC. Service-account launch privileges belong in a local
helper, not in a publicly reachable all-powerful agent. Optional container,
systemd or Windows hardening strengthens this baseline. Same-user portable mode
is a convenience profile, not a secure sandbox for hostile plugins.

## 4. Storage and domain model

Use supported local-disk SQLite WAL for durable metadata, with foreign keys,
versioned migrations, supported online backup API and transaction checks.
Minecraft roots and backup repositories can live elsewhere. Never place a live
WAL database on the NAS merely because backups use that NAS.

Core records:

- Server identity and key lifecycle; desired versus effective configuration.
- Principal, verified identity bindings, device, credential, role, role assignment,
  scoped grant/deny, expiry and constrained invitation.
- Operation, step attempt, confirmation, resource lock/lease, journal/checkpoint,
  process generation and reconciliation outcome.
- Workflow version, schedule/timezone, trigger occurrence, missed-run policy,
  typed input/output references and approved local preset hash.
- Notification, principal read cursor, delivery route/attempt and bounded output.
- Pack draft version, artifact hash, approved release, current/minimum revision,
  provider references, overrides and stale server/pack suggestions.
- Backup profile/repository, snapshot metadata, consistency grade, verification,
  retention eligibility and recovery-key reference.
- Audit event/checkpoint, bounded metrics aggregates, log offset/gap and player
  reconciliation state.

Keep protected agent data outside the Minecraft/file-browser root. Partition
state, vault, packs, staging, trash, audit, metrics and backup metadata explicitly.
Resolve external roots by canonical identity, not arbitrary remote paths.

All migrations have a protected pre-migration state backup and a failure path.
Do not silently reset corrupt roles to permissive defaults. Recovery export is
encrypted and owner-authorized; diagnostic export is redacted, not a secret dump.

## 5. API and authorization contract

Publish versioned OpenAPI definitions and event schemas. Support pagination,
stable errors, server identity/compatibility negotiation, request limits,
idempotency keys and resumable cursors with explicit retention-gap responses.
Use one HTTPS contract for third-party panels; retire the legacy raw TCP management
path rather than leaving an alternate security boundary unreviewed.

Management requests authenticate a paired device, resolve its principal and
current policy, authorize capability plus resource scope, validate input, record
intent and create a durable operation where needed. Scheduled steps follow the
same current-policy check. A role expiry or revoke invalidates existing sessions,
subscriptions and future step execution; queued authority cannot live forever.

Transport supports pinned local certificates and public-CA certificates. Stable
Vesta identity is distinct from renewable transport certificates. Device mutual
TLS must work alongside unauthenticated bootstrap/public routes without making
management optional-auth. A proxy must provide verifiable device authentication;
trusting an arbitrary forwarded certificate header is not sufficient.

Destructive confirmation binds actor, device, target, operation parameters, source
revision and expiry. A simple panel confirmation can satisfy the UX, but the
server must reject reuse, altered targets or stale state. Public discovery/status
and pack passwords are never implicit management credentials.

Local CLI owner actions use OS authorization and protected IPC. A reverse proxy
arriving from loopback must not inherit local recovery authority. Preserve local
recovery even if remote management is disabled or throttled.

## 6. Durable operation engine

Implement this before long-running features. State includes queued, validating,
awaiting_confirmation, running, cancelling, succeeded, failed, cancelled and
recovery_required. Persist intent before external effects and checkpoint each
safe transaction boundary. Record progress/result separately from bounded logs.

Locks name concrete resources: game generation, server file tree/path set, pack
draft/current release, backup repository, agent migration, maintenance admission
and supervisor activation. Define a stable acquisition order to prevent deadlock.
Read-only work can continue only when it observes stable input.

Cancellation is explicit and safe-point-aware. Stop/Kill has priority, not license
to abandon backup consistency. If a backup is active, obtain confirmation to
cancel that exact task, complete cleanup, then kill. Do not infer success from a
lost TCP response; an idempotency key retrieves the original operation.

Reconciliation claims expired leases, verifies process generation and durable
journals, then resumes only safe steps. Unknown non-idempotent command outcomes
are surfaced for an owner decision. Do not replay historical console commands.
Crash recovery of an unfinished transaction is not automatic restoration of a
completed update or world backup.

## 7. Process lifecycle and existing-server adapters

Separate process alive from game ready. Use versioned log patterns, game-port
probes and connector signals with bounded timeouts. Report uncertainty explicitly.

P3 implements start-on-agent-start and crash-restart as separate opt-in settings,
bounded backoff, crash-loop suspension, graceful OS shutdown and agent-only stop.
Supervisor activation waits until Minecraft exits; an approved restart updates
the supervisor and game once. A failed update stays failed/pending.

Managed servers use executable plus argument array, explicit working directory
and controlled environment. No shell interpolation in the normal path. Existing
scripts are inspected for supported patterns and preserved. Complex scripts can
be approved local presets, provided Vesta can detect the real child and avoid
competing restart loops; otherwise use an existing service adapter and declare
its limits. Adoption never claims to recover another process's stdin handles.

If the supervisor crashes and the game survives, validate PID/start-time/generation,
tail game logs and offer degraded adapter controls. Restoring full stdin can
require an approved restart. Platform service defaults must not accidentally
kill the game when only the agent restarts.

The live ring continuously drains output even without viewers and is bounded by
line and byte budgets. Minecraft owns persistent game logs. Catch-up uses
file-identity/offset cursors and handles rotation, truncation, retained archives
and missing history. Early JVM failures may exist only in live output.

## 8. Files, SFTP, configuration and content

Build one policy-aware file service shared by HTTPS, SFTP, archives, trash,
backup browsing and exports. Normalized string-prefix checks alone are not enough:
existing targets, missing targets with linked parents, Windows junctions/reparse
points, case/volume rules and races need adversarial tests and OS-level confinement.

Implement list/read/create/overwrite/rename/mkdir/delete with path scopes and
protected roots. Three independent ignore policies govern pack build, file
visibility and backup inclusion. Default user-hidden file patterns are empty;
protected agent internals are never exposed by a configurable ignore rule.

HTTPS text edits use content revisions; stale writes fail with compare metadata.
Uploads use bounded resumable staging and chunk/final verification. SFTP remains
optional, disabled before onboarding, and uses standard live transfer semantics
with explicit partial-upload behavior. Register SSH keys or generated separately
revocable credentials on the same Vesta principal, not a separate user database.

Use quota/retention-bounded trash for recoverable changes, preflight free space,
and defend archives against traversal, expansion bombs, links and permission
tricks. Prevent symlink/linked-root escape consistently on all routes.

Structured editors cover server.properties, JVM options, whitelist/bans and
recognized loader configuration with raw text available to authorized operators.
Report live/reload/restart apply modes. Local schema extensions are declarative,
not arbitrary privileged agent code.

Content origin adapters cover Modrinth, CurseForge, uploads, local files and
explicit URLs. Require expected hashes for arbitrary URLs; a computed hash after
downloading is not provenance. Restrict schemes, redirects, SSRF and size. Explicit
owner-approved LAN sources can be supported without letting a public request
probe the host network or cloud metadata endpoints.

For managed updates, stage mod/plugin/server-JAR replacements/removals and apply
only after the game actually exits during an approved restart. File-browser edits
remain live with clear executable-file risks. Preserve previous individual
artifacts via bounded transactions/trash; do not require a full-world backup for
every mod change. Coordinated server modpack updates can opt into a safety backup.
Checks/downloads may automate; restarts do not.

## 9. Client-pack hosting

Use a dedicated draft/overrides directory and versioned VestaIndex-compatible
manifest. Preserve exact provider file/version IDs, hashes, environment metadata
and client download references. Optional local content caching is opt-in.
Missing/withdrawn sources fail clearly; do not resolve mutable latest at install.

Custom .vestapack exports use a manifest plus overrides; accept the agreed legacy
alias. Hosted distribution can expose hashed manifest plus one overrides archive.
Archive compatibility, symlink metadata/consent and declared executable behavior
need documented fixtures; never make an uploaded archive execute on the agent.

Build freezes input, checks storage/paths/sources, produces a signed/hash-bound
artifact in private staging and atomically changes the published pointer only
after verification. Increment internal revision even if an owner lowers its
display version after warning. Show explicit optional versus required choices.

Scheduled publication freezes the exact approved artifact/minimum revision and
revalidates actor authority plus expected current revision before activation.
Draft edits or another release never silently substitute bytes. Older public
revisions are not retained as a hosted version gallery; backups retain recoverable
history. Failed builds leave the previous public pack untouched.

Pack access is independent of management. Public/password/Minecraft-policy modes
consult actual admission data where meaningful. No in-game command is required
to download a pack. Client claimed revision is forgeable; do not promise universal
Minecraft join attestation. Symlinks are a visible per-revision client warning,
not an excuse for arbitrary path escape.

## 10. Backups, consistency and restoration

Adopt signed, bundled restic orchestration rather than new crypto/deduplication.
Support opt-in local, mounted/NAS and S3-compatible repositories. All restic
repositories are encrypted; generate/store the secret and guide owner recovery
export. Native engine key derivation stays native; Vesta authentication uses
Argon2id. Remote storage does not require Vesta cloud services.

A backup profile declares included paths, ignore rules, storage budget, retention,
consistency method, warning routes and mutation policy. Separate dated repository
snapshots from optional temporary filesystem snapshots.

Online workflow: warn players; lock Vesta mutations; establish same-generation
supervisor save lease and known autosave baseline; suspend saving; acknowledge a
version-supported flush; optionally create filesystem snapshot; scan source;
verify; restore previous autosave state; release locks; publish result. Lease
expiry independently aborts work/restores autosave if the agent disappears.

Save-off is not a universal plugin/database write freeze. Report world consistency,
plugin best-effort/crash consistency and observed mutations honestly. Filesystem
snapshot acceleration is optional and does not automatically make plugins
application-consistent. If the process adapter cannot guarantee command/cleanup
behavior, use stopped-server backup. An advanced write guard stays default-off:
open handles and plugin failures make it a limited, dangerous aid, not a promise.

Verification includes engine integrity checks, manifest/hash metadata and
periodic owner-configured restore drills into isolated inspection directories.
Count/age/size budgets retain eligible restore points with working-space reserve;
pruning cannot be treated as free when the disk is already full.

All restores are owner-confirmed. Stop the game, preflight storage, create a
pre-restore safety backup where possible, restore to validated staging, apply
journalled in-place/selective changes, preserve agent secrets/scopes, then verify.
No automatic restore after a failed update/health check. An owner must explicitly
authorize proceeding without the safety backup. A restore never copies arbitrary
archive permissions or paths into control-plane state.

## 11. Workflows, notifications and community operations

Implement local durable sequential workflows with typed inputs/outputs,
conditionals, delays, bounded retries/timeouts and explicit failure behavior.
Triggers include manual/calendar/interval/startup/lifecycle/player/backup/update/
resource events. Use stored timezone and documented daylight-saving behavior.

There are no workflow loops/parallel branches in the initial release. Local
presets can supply scripts and hooks; remote users select approved actions with
validated parameters, not author scripts. Preset definitions sit outside game
write authority and changes require validation/approval. Recheck current
principal/preset authority before each sensitive step.

Notifications use a durable per-principal inbox/read cursor and optional outbound
email, Discord and webhooks. Sign/validate webhook payloads as appropriate, bound
retries and redact tokens. Never let an unavailable provider block core work.
Discord channels and status presentation remain owner-configured; read-only
status defaults do not depend on Vesta picking a community's privacy policy.

Maintenance supports optional advance actions and admission restrictions.
Connector overlays are preferred. Without a connector, use a journalled
whitelist transaction and reload where supported, reconcile owner edits on exit/
crash and avoid overwriting changed lists with stale snapshots.

Sleep is opt-in and driven by reconciled player absence, not a single missed line.
While stopped, a bounded game-protocol responder reports configured offline/MOTD
state. Pings never wake the server. A genuine join attempt may trigger one
rate-limited wake; explain reconnect/readiness limitations instead of promising
transparent login buffering on every loader/version.

Typed moderation, status, metrics and diagnostics use capability-specific
adapters. Kill/lightning and other unsupported version-specific commands report
unsupported clearly. Game OP is not Vesta admin. Console/log access remains
explicit. Track metrics/player history under storage-bounded retention; unknown
TPS/player state is unknown, not fabricated zero. Export redacted diagnostics
manually; do not automatically upload private logs.

## 12. Networking, update distribution and installer

One simple address can discover game and Vesta endpoints through documented
descriptor/adapter mechanisms, with an explicit API endpoint fallback. Treat
discovery as untrusted; verify identity before sending credentials and constrain
redirects. Preserve loopback/LAN management alongside a chosen public proxy path.

Offer reviewed Nginx/Caddy/Traefik configuration only when selected, back up touched
files and validate before reload. Never silently install/rewrite unrelated proxy
configuration. HTTP-01 requires port 80; TLS-ALPN requires port 443. Proxy-managed
ACME or owner-supplied DNS validation handles environments where direct validation
does not work. HTTPS proxies do not automatically carry Minecraft TCP or SFTP.

Public status/download exposure is separately configurable from management.
Disabling remote management closes remote sessions while local recovery stays.
Optional probe testing discloses remote address sharing. VPN/overlay is supported;
no central relay is required, and a later relay must be owner-hostable.

Distribution checks signed metadata/artifacts, compatibility and storage before
approved activation. Stable defaults, optional beta/nightly, rollback protection
and previous signed binaries are supported. Agent update does not restart the
game; supervisor activation waits for stop. Never silently restore world data to
repair an update. Apply state-compatible binary fallback only with clear outcome.

The installer supports Windows/Linux interactive and headless answer-file flows,
connector capability consent, game Java detection/provisioning, existing-root
adoption, service or portable profiles and non-destructive uninstall choices.
Never download/run an unsigned mutable installer artifact merely because a
distribution URL is HTTPS. Optional telemetry is off and nonessential.

## 13. Current implementation assessment

> Historical: this assessment is the 2026-10-04 Java audit and cites Java files. Current Rust status is in section 20.

Audit date: 2026-10-04. Server commit:
6dd558e7bef1a5f6afd3ba84994820ec3ccc82ab. No server/launcher source changed.

Inspected core, wrapper, loader, installer, tests and manifest contract references.
Launcher code was consulted for VestaIndex only; this is not a fresh launcher
feature-status audit. Old PRODUCT_ROADMAP.md and ARCHITECTURE.md describe the
earlier sync MVP; the accepted requirements and this plan supersede contradictory
product decisions without erasing their historical record.

Existing tests: 32 suites, 139 tests, zero failures/errors in JUnit reports dated
2026-10-04 after the core/wrapper rerun. The initial command was cache-up-to-date;
the later rerun produced fresh reports. These are deterministic core/wrapper
tests, not a live Minecraft/connector/platform/deployment security certification.

Status meanings:

- Not started: no target subsystem located in the inspected implementation.
- Prototype: related code exists but follows an older design or lacks core guarantees.
- Partial: useful target foundations exist; listed acceptance work remains.
- Implemented: target behavior exists, but required verification is incomplete.
- Verified: only the specifically named bounded behavior has implementation/test evidence.
- Blocked: implementation cannot proceed pending a concrete prerequisite.
- Deferred: intentionally outside the initial release, not a current failure.

No whole-product completion percentage or estimated delivery date is claimed.
Feature counts are not effort estimates. Absence means no subsystem located in
this audit, not proof that every uninspected branch lacks it.

| ID | Subsystem | Status | Phase | Evidence-backed current state | Source evidence |
| --- | --- | --- | --- | --- | --- |
| F01 | Deployment and instance scope | partial | P1 | Existing roots, config and instance registry exist; independent process/control-plane identities and tested Windows/Linux parity do not. | [install.ps1](../scripts/install.ps1), [ConfigLoader.java](../wrapper/src/main/java/com/vesta/wrapper/config/ConfigLoader.java) |
| F02 | Process lifecycle | partial | P3 | Start/stop/restart/kill exist, but stop force-kills after ten seconds and control is owned by the agent JVM. | [MinecraftServerProcess.java](../wrapper/src/main/java/com/vesta/wrapper/runtime/MinecraftServerProcess.java), [VestaWrapper.java](../wrapper/src/main/java/com/vesta/wrapper/VestaWrapper.java) |
| F03 | Independent supervisor and crash continuity | not started | P3 | No separate supervisor executable or durable process/save-lease protocol found. Agent shutdown currently stops Minecraft. | [VestaWrapper.java](../wrapper/src/main/java/com/vesta/wrapper/VestaWrapper.java), [MinecraftServerProcess.java](../wrapper/src/main/java/com/vesta/wrapper/runtime/MinecraftServerProcess.java) |
| F04 | Startup script adoption and existing services | partial | P3 | Installer/CLI create and control launch artifacts; seamless script inspection/adoption and robust detached-child adapters remain. | [install.ps1](../scripts/install.ps1) |
| F05 | Readiness and crash-loop policy | prototype | P3 | Process-alive state exists, but it is not world readiness; no durable bounded crash-restart controller found. | [MinecraftServerProcess.java](../wrapper/src/main/java/com/vesta/wrapper/runtime/MinecraftServerProcess.java) |
| F06 | Local identity, pairing and device credentials | prototype | P2 | Link codes and bearers exist; canonical device principals, local owner bootstrap and device certificates are missing. | [TokenService.java](../core/src/main/java/com/vesta/sync/auth/TokenService.java), [ControlIpcServer.java](../wrapper/src/main/java/com/vesta/wrapper/ipc/ControlIpcServer.java) |
| F07 | Minecraft identity and admission adapters | partial | P2 | Session challenge and whitelist reader exist; binding to device principals, bans and policy reconciliation remain. | [MinecraftSessionChallengeService.java](../core/src/main/java/com/vesta/sync/auth/MinecraftSessionChallengeService.java), [CapabilityEngine.java](../core/src/main/java/com/vesta/sync/auth/CapabilityEngine.java) |
| F08 | Roles, denies, scope and delegation | prototype | P2 | Named capabilities exist within numeric levels derived from OP/permission nodes, not independent Vesta role assignments. | [CapabilityEngine.java](../core/src/main/java/com/vesta/sync/auth/CapabilityEngine.java) |
| F09 | Operation confirmation and security audit | not started | P1 | Ordinary logs exist, but no durable exact-target confirmations or protected structured audit subsystem found. | [VestaHttpServer.java](../wrapper/src/main/java/com/vesta/wrapper/server/VestaHttpServer.java) |
| F10 | TLS, stable server identity and device authentication | partial | P2 | TLS material and certificate fingerprint API exist; mutual TLS device lifecycle and renewal-safe identity separation remain. | [TlsMaterial.java](../wrapper/src/main/java/com/vesta/wrapper/security/TlsMaterial.java), [VestaHttpServer.java](../wrapper/src/main/java/com/vesta/wrapper/server/VestaHttpServer.java) |
| F11 | Vault, password hashing and credential lifecycle | prototype | P2 | MVP hashes human passwords with salted SHA-256 and persists plaintext security.password; no protected vault found. | [TokenService.java](../core/src/main/java/com/vesta/sync/auth/TokenService.java), [SecuritySettingsService.java](../wrapper/src/main/java/com/vesta/wrapper/config/SecuritySettingsService.java) |
| F12 | Abuse, request and storage limits | partial | P2 | HTTP/auth token buckets exist; transfer/storage quotas and coordinated resource budgets are incomplete. | [TokenBucketRateLimiter.java](../wrapper/src/main/java/com/vesta/wrapper/security/TokenBucketRateLimiter.java), [VestaHttpServer.java](../wrapper/src/main/java/com/vesta/wrapper/server/VestaHttpServer.java) |
| F13 | Public/versioned API and event contract | partial | P1 | HTTP/WebSocket and legacy TCP messages exist; target OpenAPI, durable cursors, idempotency and consistent errors remain. | [VestaHttpServer.java](../wrapper/src/main/java/com/vesta/wrapper/server/VestaHttpServer.java), [VestaHttpServerCommunicationTest.java](../wrapper/src/test/java/com/vesta/wrapper/server/VestaHttpServerCommunicationTest.java) |
| F14 | 10,000-line live console buffer | verified | P3 | Bounded in-memory console ring is present; focused existing ConsoleBuffer tests passed. This does not verify supervisor continuity. | [ConsoleBuffer.java](../wrapper/src/main/java/com/vesta/wrapper/console/ConsoleBuffer.java), [ConsoleBufferTest.java](../wrapper/src/test/java/com/vesta/wrapper/console/ConsoleBufferTest.java) |
| F15 | Game-log catch-up and player reconciliation | partial | P3 | Live list-command parsing exists; persistent log offsets, rotation/gap reconciliation and reliable absence are missing. | [PlayersQuery.java](../wrapper/src/main/java/com/vesta/wrapper/runtime/PlayersQuery.java) |
| F16 | File confinement and protected roots | partial | P4 | Basic sanitization/real-path checks exist; creation through symlink parents and protected-root coverage need hardening. | [FileBrowserService.java](../wrapper/src/main/java/com/vesta/wrapper/files/FileBrowserService.java), [ContentInstaller.java](../wrapper/src/main/java/com/vesta/wrapper/content/ContentInstaller.java) |
| F17 | File editing, trash, archives and transfers | partial | P4 | Read/write/delete exist with temporary-file replacement. No complete trash, optimistic edits, archives or resumable transfer engine found. | [FileBrowserService.java](../wrapper/src/main/java/com/vesta/wrapper/files/FileBrowserService.java) |
| F18 | SFTP service and unified file policy | not started | P4 | No SFTP subsystem or dependency located. | No subsystem located in audited source inventory |
| F19 | Content sources and safe server updates | prototype | P4 | Live install/remove supports byte hashing; provider-resolution, expected-hash verification and restart activation are not complete. | [ContentInstaller.java](../wrapper/src/main/java/com/vesta/wrapper/content/ContentInstaller.java), [ServerState.java](../wrapper/src/main/java/com/vesta/wrapper/server/ServerState.java) |
| F20 | Separate hosted client-pack workspace | prototype | P5 | MVP can host platform/archive sources, but scan mode and generated packs read the live server tree. | [ServerState.java](../wrapper/src/main/java/com/vesta/wrapper/server/ServerState.java), [VestaPackBuilder.java](../core/src/main/java/com/vesta/sync/manifest/VestaPackBuilder.java) |
| F21 | Vesta Pack schema and provider references | partial | P5 | Vesta index builder includes revision/minimum revision and external references; launcher-compatible schema fixtures need expansion. | [VestaPackBuilder.java](../core/src/main/java/com/vesta/sync/manifest/VestaPackBuilder.java), [ServerModpackManifest.java](../core/src/main/java/com/vesta/sync/manifest/ServerModpackManifest.java) |
| F22 | Pack build, publication and scheduled release | prototype | P5 | Archive builder and configured revisions exist; durable publication transaction, automatic revision increment and scheduled activation remain. | [ServerState.java](../wrapper/src/main/java/com/vesta/wrapper/server/ServerState.java), [VestaPackBuilder.java](../core/src/main/java/com/vesta/sync/manifest/VestaPackBuilder.java) |
| F23 | Pack policy, minimum version and authorization | partial | P5 | Capabilities and revision fields exist; complete independent pack policy and minimum-version lifecycle remain. | [CapabilityEngine.java](../core/src/main/java/com/vesta/sync/auth/CapabilityEngine.java), [ServerModpackManifest.java](../core/src/main/java/com/vesta/sync/manifest/ServerModpackManifest.java) |
| F24 | Ignore policies and pack symlink safety | not started | P5 | Independent target ignore rules and per-revision symlink consent protocol not located. | No subsystem located in audited source inventory |
| F25 | Backup repositories and retention | not started | P6 | Installer copy/backup options are not a runtime restic repository/history subsystem. | [install.ps1](../scripts/install.ps1) |
| F26 | Online backup consistency and save lease | not started | P6 | No online-backup coordinator, crash-safe autosave lease or snapshot helper found. | No subsystem located in audited source inventory |
| F27 | Restore, inspection and recovery exports | not started | P6 | No verified manual restore workflow or encrypted complete recovery export found. | No subsystem located in audited source inventory |
| F28 | Durable operations, locks and recovery journals | not started | P1 | HTTP actions are synchronous/ephemeral; no durable operation store or resource-lock recovery found. | [VestaHttpServer.java](../wrapper/src/main/java/com/vesta/wrapper/server/VestaHttpServer.java) |
| F29 | Schedules and typed sequential workflows | not started | P7 | Session cleanup timers are not an owner scheduler; durable typed workflows/presets absent. | No subsystem located in audited source inventory |
| F30 | Notification inbox and optional delivery | not started | P7 | Push update events exist, but no durable per-account inbox/delivery subsystem found. | [ServerState.java](../wrapper/src/main/java/com/vesta/wrapper/server/ServerState.java) |
| F31 | Maintenance, whitelist overlay and hooks | not started | P7 | Whitelist read adapter exists; temporary maintenance restrictions and crash-safe reconciliation are absent. | [CapabilityEngine.java](../core/src/main/java/com/vesta/sync/auth/CapabilityEngine.java) |
| F32 | Sleep, offline status and wake-on-join | not started | P7 | No game-port sleep/status responder or wake coordinator located. | No subsystem located in audited source inventory |
| F33 | MOTD/settings placeholders and lifecycle presentation | prototype | P7 | Static descriptor/status exists; structured live/offline templates and state-aware game responses are absent. | [VestaHttpServer.java](../wrapper/src/main/java/com/vesta/wrapper/server/VestaHttpServer.java) |
| F34 | Typed moderation and online player management | partial | P7 | Online players and unrestricted console exist; separate safe moderation endpoints/history remain. | [PlayersQuery.java](../wrapper/src/main/java/com/vesta/wrapper/runtime/PlayersQuery.java), [VestaHttpServer.java](../wrapper/src/main/java/com/vesta/wrapper/server/VestaHttpServer.java) |
| F35 | Metrics, diagnostics and resource budgets | prototype | P7 | Basic process/status/list snapshots exist; TPS adapters, storage-limited retention and redacted bundles remain. | [MinecraftServerProcess.java](../wrapper/src/main/java/com/vesta/wrapper/runtime/MinecraftServerProcess.java), [PlayersQuery.java](../wrapper/src/main/java/com/vesta/wrapper/runtime/PlayersQuery.java) |
| F36 | Structured server configuration | prototype | P4 | Security TOML editor exists; server.properties/JVM/ban/loader schema-backed edits and apply modes are incomplete. | [SecuritySettingsService.java](../wrapper/src/main/java/com/vesta/wrapper/config/SecuritySettingsService.java) |
| F37 | SQLite state, repair and pending changes | not started | P1 | No SQLite dependency/state layer or durable pending-change reconciliation located. | No subsystem located in audited source inventory |
| F38 | OS isolation and optional sandbox profiles | prototype | P3 | Service scripts exist; verified separate game/control identities and sandbox/resource-limit profiles remain. | [install.ps1](../scripts/install.ps1) |
| F39 | Java runtime and compatibility matrix | partial | P3 | Custom Java/JVM arguments and loader release matrix exist; guided game-runtime provisioning and 1.7+ test matrix remain. | [MinecraftServerProcess.java](../wrapper/src/main/java/com/vesta/wrapper/runtime/MinecraftServerProcess.java), [install.ps1](../scripts/install.ps1) |
| F40 | Network discovery, NAT and optional proxy setup | partial | P8 | Server descriptor/address utilities exist; end-to-end reachability diagnostics, ACME and reviewed proxy setup remain. | [VestaHttpServer.java](../wrapper/src/main/java/com/vesta/wrapper/server/VestaHttpServer.java), [install.ps1](../scripts/install.ps1) |
| F41 | Signed agent/connector updates | not started | P8 | Distribution packaging exists, not signed rollback-protected updates and approved activation. | [install.ps1](../scripts/install.ps1) |
| F42 | Installer, CLI and nonintrusive uninstall | partial | P8 | Windows/Linux installers, registry/control CLI and service helpers exist; complete secure walkthrough/connector setup remain. | [install.ps1](../scripts/install.ps1) |
| F43 | Discord status/linking and integration presets | not started | P7 | No Discord bot, identity linkage or outbound delivery provider located. | No subsystem located in audited source inventory |
| F44 | Declarative extensions and opt-in telemetry | not started | P8 | No target schema-extension registry/telemetry consent implementation located. | No subsystem located in audited source inventory |
| F45 | Velocity, multi-server, relay and host protocol extensions | deferred | P9 | Future topology/features; initial scope is one independently secured Java server per agent. | No subsystem located in audited source inventory |

## 14. High-priority migration gaps

1. Authority: remove automatic Minecraft OP/LuckPerms escalation into Vesta roles.
   Import proposed roles only through a local owner-reviewed migration.
2. Local admin: current loopback-based shortcut is not OS authentication and is
   particularly dangerous behind a loopback reverse proxy. Replace with protected
   local IPC/credentials, not merely a forwarded-address check.
3. Passwords/vault: migrate plaintext TOML and fast salted hashes to protected
   credential storage. Re-pair/revoke legacy bearer authority where necessary;
   migration must not silently turn a pack password into an admin credential.
4. Lifetime: current agent shutdown calls Minecraft stop; current stop force-kills
   after ten seconds. Target independent supervision and explicit Kill differ.
5. Packs: stop deriving the hosted client pack from the live server scan. Import
   only into the separate workspace after explicit review, then publish once.
6. Files: harden symlink-parent/TOCTOU/protected-root paths and unify all transfer
   routes before treating the file API as suitable for hostile remote inputs.
7. Persistence: do not bolt backups/workflows onto synchronous handlers. Build
   operation journals, locks and audit before those features.
8. Protocol: deprecate legacy management/security paths through a documented
   compatibility window; disabling old paths must not remove local recovery.

These are architecture/migration findings, not a completed penetration test.
Validate exploitability and regression scenarios during P0/P2/P4.

## 15. Implementation phases and exit gates

### P0 — Baseline, threat model and shared contracts

Freeze requirement IDs and traceability, target API/domain schemas, pack fixtures,
platform support matrix and migration inventory. Inventory dependencies and
licences, capture existing tests, and define repeatable fake-game/adapter harnesses.
Resolve supervisor/OS-helper feasibility with bounded prototypes, not product code
changes disguised as planning. Gate: design review identifies authority and
lifetime boundaries and every initial subsystem maps to an acceptance test.

### P1 — Durable foundation

Add local SQLite migrations, operation lifecycle/locks/journals, confirmations,
structured audit, event cursors, API error/idempotency contracts and desired versus
effective settings. Keep recovery paths available during ordinary corruption.
Gate: interrupted write/migration/confirmation/queue tests yield a known state;
sensitive mutations cannot proceed without authorization and durable audit.

### P2 — Identity and remote security

Implement server-scoped principals/devices, local bootstrap/recovery, invitations,
role/deny/scope/expiry/delegation, mTLS/identity rotation, vault and Argon2id,
transport/proxy boundaries and abuse budgets. Remove game-permission escalation.
Gate: complete permission matrix, proxy/spoof/replay/revoke/cross-server tests and
legacy migration prove no unintended remote owner authority.

### P3 — Independent supervisor and process adoption

Implement protected local IPC, separate OS identities, generation tracking,
launch/readiness, graceful stop/explicit kill, approved restart, crash policies,
output ring/log catch-up, service/script adapters and autosave lease primitive.
Gate: repeated agent/supervisor/host failure drills on Windows and Linux show no
duplicate game process, false-ready state or silently abandoned cleanup.

### P4 — Unified files, configuration and content

Implement confined filesystem service, HTTPS/resumable/SFTP policy parity,
optimistic text edits, trash/archives, storage budgets, structured editors,
provider/hash/SSRF defenses and pending-JAR transactions.
Gate: archive/link/junction/race/disk-full/cancellation tests cannot escape roots;
managed executable updates never replace running code or restart automatically.

### P5 — Independent hosted pack model

Implement workspace/import migration, provider manifest adapters, immutable
artifact builder, signing/revisions, minimum-version decisions, manual and
scheduled release, ignore/symlink metadata and pack access policy.
Gate: launcher-schema fixtures pass; incomplete/competing publications preserve
one valid public revision; suggestions do not mutate the pack automatically.

### P6 — Backups and recovery

Bundle reviewed restic, add repositories/recovery keys, profiles, verification,
retention, save coordination/leases, optional snapshot/write-guard adapters,
manual restore and encrypted recovery exports.
Gate: induced agent loss during save-off cleans up; consistency is labelled;
disk-full/network loss cannot erase valid backups; restore drills verify data.

### P7 — Automation and community operations

Add typed sequential workflows, durable notification inbox/delivery, player
history/moderation, metrics/diagnostics, maintenance, hooks, MOTD/sleep and optional
Discord. Configuration presets are editable, not irreversible product choices.
Gate: missed/revoked/ambiguous events cannot replay destructive actions; offline
providers do not block work; sleeping responds to ping without waking.

### P8 — Distribution, onboarding and initial release

Complete secure Windows/Linux walkthrough/CLI/uninstall, optional connector
packaging, signed update distribution, exposure/proxy/ACME/reachability diagnostics,
documented support matrix and manual disaster-recovery rehearsal.
Gate: independent security review, full platform acceptance, storage/failure/load
tests and signed reproducible release artifacts pass. No launcher UI is required
to validate protocol behavior with test clients.

### P9 — Later proposals

Velocity topology/multiple managed servers, host-service principals, self-hostable
relay, mobile pairing, mutating Discord commands and isolated extension modules.
Write separate requirements before implementation. Their absence does not prevent
P8 completion.

Dependency order: P0 → P1 → P2 → P3. P4 follows P3. P5/P6 can progress in parallel
after their shared P1–P4 foundations. P7 depends on both. P8 is the cross-platform
release gate. Security is a gate in each phase, not a final optional phase.

## 16. Acceptance and threat-test matrix

- Identity: pairing expiry/replay, key rotation, unexpected server identity,
  revocation during active sessions, role expiry, deny composition, cross-server
  access and local recovery behind a proxy.
- Roles: every preset × capability × resource × allow/deny outcome; delegation
  rank cannot exceed capability scope; mods/OP cannot grant Vesta admin.
- Files: traversal, drive/UNC/ADS paths, case differences, junctions, linked parents,
  concurrently changed ancestors, archive expansion, protected paths and scope
  parity across HTTPS/SFTP/backups/trash/export.
- Lifecycle: slow stop, missing exit, force kill confirmation, reused PID,
  agent-only restart, supervisor crash, power-loss reconciliation and script loops.
- Pack: deterministic manifests, source disappearance, expected hash mismatch,
  missing overrides, conflicting draft changes, interrupted publish, scheduled
  release conflict, lowered display-version warning and minimum revision rules.
- Backup: flush timeout, unknown autosave baseline, agent crash during scan,
  lease expiry, plugin writes, snapshot privilege failure, full disk, interrupted
  prune/upload, recovery-key loss drill and failed restore safety backup.
- Workflows: DST/timezone changes, missed triggers, duplicate delivery, revoked
  principal/preset, unknown command outcome, cancellation during backup and
  external provider outage.
- Network: untrusted discovery/redirects, forwarded-header spoofing, mTLS/public
  bootstrap, certificate renewal/expiry, raw TCP legacy exposure, NAT/proxy
  conflict, wake flooding and no ping-triggered start.
- Resource/security: bounded input/output, malicious log lines, slow subscribers,
  token/hash/KDF budgets, audit capacity, secret redaction, signed update rollback,
  database corruption and unsupported adapter behavior.

Use test levels deliberately: pure unit; protocol/SQLite/IPC integration;
fake-game fault injection; real Windows/Linux service/file tests; representative
Minecraft/loader integration; opt-in end-to-end remote/proxy tests. The 1.7+
target needs tested per-feature coverage, not a blanket connector promise.

Performance targets and time estimates must come from measured hosts and chosen
support workloads. Start with bounded resource presets and observable budgets,
not invented universal player-count promises.

## 17. Definition of complete

P8 is complete only when every initial feature has target acceptance evidence,
secure setup/adoption is reproducible on Windows/Linux, no launcher connection is
needed for operations, backup/recovery drills pass, authority remains independent
of game plugins, documentation describes degraded capabilities honestly, and an
independent security review has no unresolved release-blocking findings.

The feature registry is a snapshot, not a live monitor. Reaudit source/test
evidence after each phase; promote a status only against its acceptance criteria.
Keep audit date/commit visible. Update the Markdown, JSON and HTML together.

## 18. Research and borrowed lessons

- [Pterodactyl Wings configuration](https://pterodactyl.io/wings/1.0/configuration.html):
  resource/isolation knobs need explicit security tradeoffs. Vesta borrows clear
  operational controls, not a mandatory separate web-panel/database architecture.
- [Crafty backup management](https://docs.craftycontrol.com/pages/user-guide/backup-manager/):
  backup profiles, exclusions, schedules and distinct restore choices are useful
  owner controls. Vesta keeps them opt-in and makes storage/consistency explicit.
- [PufferPanel Docker environment](https://docs.pufferpanel.com/en/3.x/environments/docker.html):
  workload containers provide meaningful isolation; simple shared-process access
  should not be marketed as equivalent. Docker remains optional for Vesta.
- [AMP 2.6.4 release notes](https://discourse.cubecoders.com/t/amp-phobos-2-6-4-release-notes/35690):
  delegation and reconciling externally edited files illustrate real panel needs.
  Vesta uses the owner's stricter lower-rank/local-admin-grant policy.
- [Paper update guidance](https://docs.papermc.io/paper/updating/) and
  [plugin trust guidance](https://docs.papermc.io/paper/adding-plugins/):
  running executable-file replacement and untrusted plugins are security/operational
  risks; managed staging and game/control-plane isolation address them.
- [JVM class loading](https://docs.oracle.com/javase/specs/jvms/se21/html/jvms-5.html):
  loading is dynamic; startup does not establish that a JAR will never be read again.
- [Restic repository design](https://restic.readthedocs.io/en/stable/100_references.html):
  use native authenticated encrypted storage rather than inventing backup crypto.
- [Let's Encrypt challenge types](https://letsencrypt.org/docs/challenge-types/):
  certificate automation has concrete reachability requirements, not just a TLS
  checkbox on any arbitrary API port.
- [OWASP API Security](https://owasp.org/API-Security/) and
  [The Update Framework](https://theupdateframework.io/):
  organize abuse/auth/resource regression tests and signed-update trust review.

Research informs the design; it does not imply that these tools already provide
Vesta's exact guarantees or that their code can be copied without licence review.

## 19. Deliverables and next implementation boundary

Delivered planning artifacts: accepted requirements, this architecture/phased
plan, dated status registry and a self-contained HTML roadmap containing the full
plan and requirements plus feature evidence and release gates. The HTML is a
planning document, not a working Minecraft panel, public site or deployed service.

No feature implementation, external messages, PR, deployment, firewall change or
installer execution is part of completing this plan. A later implementation task
should begin with P0/P1 against the agreed baseline rather than restart product
discovery or implement all subsystems in one unreviewable patch.


## 20. Current status against the phases (2026-10-10)

Audit at commit 94fb98a. The Java-to-Rust migration gate has not passed: the change-only review since a3dda06 is still pending. The feature registry (`SERVER_SOFTWARE_STATUS.json`) now carries the Rust re-audit for 15 features; the rest are carried from 2026-10-04 and marked for re-audit.

| Phase | Where it stands |
|---|---|
| P0 Baseline | Mostly complete. Requirements, inventory and parity checks exist. |
| P1 Durable foundation | Largely in Rust: migrations to v11, operations, audit, confirmations, file intents and recovery, all tested. |
| P2 Identity and remote security | Not met. No remote management login, device certificates or owner recovery. Management routes are refused by design. |
| P3 Supervisor and adoption | Not met. The agent still owns the game process; no separate supervisor or crash continuity. |
| P4 Files and content | Built as library code, not reachable remotely until P2. SFTP not started. |
| P5 Client-pack hosting | Mostly built. Scheduled release and a trusted publish path from the running server remain open. |
| P6 Backups and recovery | Not started. Outside the Java scope; future work. |
| P7 Automation | Not started, except pieces of moderation and player queries. |
| P8 Distribution | Partial installer. Signed updates not started. |

Recommended direction: P2 (remote management identity), then P3 (a separate supervisor), then the remote parts of P4 and P5.
