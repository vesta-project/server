# Vesta Server Software - Requirements and Decision Log

Status: product baseline agreed; engineering plan and implementation audit dated
2026-10-04. Future changes are explicit amendments, not inferred new requirements.

This document captures the target product and decisions made while planning the
complete Vesta server software. It deliberately excludes implementation of the
launcher UI, while treating the launcher as the primary management panel and
defining the server-side behavior and protocol that panel requires.

## Planning deliverables

- Complete this living requirements document before freezing the architecture
  and phased implementation plan.
- After the Markdown plan is complete, generate a standalone interactive HTML
  roadmap that visualizes the entire target system, implementation phases,
  dependencies, security gates, and evidence-backed status of every subsystem in
  the current repository.
- HTML status must distinguish at least `not started`, `prototype`, `partial`,
  `implemented`, `verified`, and `blocked/deferred`, link claims to source files
  or tests where practical, and state the audit date/commit so stale estimates
  are not presented as live truth.

## Product intent

Vesta Server is a self-hosted, security-first Minecraft server management agent.
It should provide the capabilities users expect from a mature server panel while
keeping the launcher as the operator interface. The agent must continue running
the server, schedules, backups, health checks, crash recovery, notifications, and
other automation when no launcher is connected.

The initial audience is an individual managing their own server, followed by
community owners and delegated administrators. Commercial hosting is not the
primary product, but the protocol should preserve a path for hosting providers.
No Vesta cloud account or Vesta-operated service may be required for core use.

## Locked decisions

### Deployment and instance scope

- The first complete release uses one agent instance per Minecraft server
  process.
- Every agent has a stable `server_id`. Authentication, authorization, audit
  events, storage, and protocol resources are scoped to that ID so a future host
  supervisor can expose multiple separately secured servers.
- Windows and Linux are supported. Current development and testing is primarily
  on Windows.
- An existing Minecraft server directory can be selected without moving it into
  a Vesta-owned directory.

### Process modes

- `managed`: Vesta launches and supervises the Minecraft process and can provide
  complete lifecycle control, stdin console commands, stdout streaming, crash
  policy, and process metrics.
- `existing`: Vesta uses an existing server directory. It can optionally become
  the process supervisor, or use available adapters such as the connector, RCON,
  log tailing, a Windows service, or systemd. Capabilities must accurately report
  what is and is not available; no feature may pretend to work when the process
  cannot be controlled safely.
- Start-on-agent-start and restart-after-crash are separate, opt-in policies.
  Crash restart supports bounded exponential backoff, crash-loop detection, and
  suspension with a durable notification.
- Agent, Java, server software, modpack, mod, and plugin updates never restart
  the Minecraft server or agent automatically. A restart is a separately
  authorized operation, even when update checking or download is automated.

### Installation components and process continuity

- Owners install one Vesta product. Internally it contains a management agent
  and a small process supervisor; the installer manages both and normally does
  not ask users to configure their connection. The optional Minecraft connector
  mod/plugin is a third component installed only on a compatible server with
  owner consent. Vanilla operation remains supported.
- The agent owns network APIs, identity, policy, configuration, operations, and
  integrations. The supervisor owns the managed Minecraft process, stdin command
  delivery, output draining, the bounded live console buffer, and process exit
  events. It has no network listener and exposes permission-protected local IPC
  through Windows named pipes or Unix domain sockets.
- IPC grants the agent process control but gives the in-game connector only a
  restricted capability channel. Merely running on the same host or possessing
  connector credentials never grants supervisor command or management access.
- Minecraft continues running across an agent crash, approved agent update, or
  agent-only service stop. Start-on-agent-start checks the supervisor's existing
  process generation and never creates a duplicate launch. Full Vesta shutdown
  and Minecraft Stop are distinct, explicit operations.
- After reconnecting, the agent reconciles process generation/identity, exit
  events, operation journals, active maintenance, save-suspension cleanup, and
  Minecraft log offsets before it resumes automation. Missed commands or
  non-idempotent workflow steps are never blindly replayed.
- A supervisor update remains pending until Minecraft is stopped. The panel
  prompts for an approved restart: issue `stop`, observe actual process exit,
  activate the supervisor update, launch Minecraft through the new supervisor,
  and verify readiness. A failed stop or update leaves a visible pending/failed
  operation rather than silently force-killing or starting duplicate processes.
- OS shutdown integration attempts graceful Minecraft stop within the platform's
  available service shutdown deadline. Setup reports/configures that deadline
  where supported; abrupt power loss cannot be made graceful by software.
- If the supervisor crashes while Minecraft survives, the agent identifies the
  process using more than a PID (including start time and launch identity), tails
  game logs, and reports degraded control. It can use a configured connector,
  RCON, or existing service adapter where available; full stdin control may require
  an owner-approved Minecraft restart.
- Adopting a running existing server does not terminate it. Vesta initially uses
  available monitoring/control adapters and offers full supervision on the next
  approved restart. An existing Windows service or systemd unit may remain the
  process owner through an explicitly selected adapter; support and permissions
  are capability-detected, not assumed.
- The ordinary launch definition is an executable and argument array with an
  explicit working directory. It is passed directly to process creation without
  shell interpolation. Changing executable paths, JVM arguments that load code,
  or service targets is a trusted operational capability.
- Advanced locally registered launch presets can run an existing `.bat`, `.ps1`,
  or shell script for servers with custom startup needs. Remote users can select
  approved presets and validated parameters; registration and script authoring
  occur locally. The normal installer does not require any script knowledge.
- Existing-server onboarding prefers taking over startup seamlessly: inspect
  supported script patterns, detect the Java executable, arguments, working
  directory, and required setup, then offer an equivalent supervised launch on
  the next approved restart. Preserve the original script; never silently rewrite
  it, run it merely to discover settings, or discard required custom steps.
- Arbitrary scripts cannot always be safely converted. Offer an approved custom
  startup preset or an existing-service adapter instead of forcing migration.
  Supervision must identify the actual Minecraft child and surface limitations
  for detached children or scripts with their own restart loops. Owner-approved
  takeover must not create competing process owners or duplicate restarts.
- Readiness distinguishes `process_alive`, `starting`, `ready`, and `degraded`.
  It combines server-adapter console patterns, game-port probes, connector signals
  where available, and configurable timeouts. A live process or open port alone
  is insufficient proof that a world is ready to accept players.

### Identity, pairing, and authority

- The system is completely self-hostable and does not depend on Vesta accounts.
- A launcher installation has a device keypair. Successful pairing issues a
  server-scoped device credential; raw long-lived bearer secrets are not the
  target design.
- Device private keys are non-exportable through normal Vesta UI/API flows. Each
  device pairs independently and can be revoked independently.
- Minecraft-authenticated access is associated with a Minecraft UUID and one or
  more paired devices.
- Passcode-only access is associated with the paired device when no UUID has been
  proven.
- Administrative authority is granted only through the local Vesta wrapper
  console/CLI. Minecraft OP, LuckPerms, plugins, mods, or in-game commands cannot
  grant agent administration.
- In-game linking may prove a Minecraft UUID and grant only the role configured
  by the server owner, but it is not required for normal pack access and is not
  part of the administrative bootstrap path. Its remaining value is limited to
  optional identity linking and offline-mode/future integrations such as Discord.
- Local recovery commands can create a new owner pairing and optionally revoke
  all existing administrative access.
- The initial ownership flow uses a short-lived, single-use pairing code. Local
  launcher-guided onboarding and a headless installer/CLI flow should use the
  same underlying bootstrap protocol.
- Pairing and invitation codes are rate-limited, single-use, and expire after a
  configurable interval with a ten-minute default.
- Owner may revoke any device. Other remote roles may revoke only principals and
  devices strictly below their own delegation rank.

### Unified principal model

- One server-local Vesta principal is the canonical identity. Device keys,
  Minecraft UUIDs, Discord identities, pack-password grants, roles, and SFTP
  credentials attach claims/capabilities to that principal instead of creating
  separate account databases.
- Entering a pack password bootstraps a device-bound pack grant so the password
  is not repeatedly transmitted. Password rotation can revoke all grants derived
  from the previous password.
- Discord OAuth links a Discord identity to an existing/new local principal; it
  does not create an independent administrative authority.
- External SFTP clients require a protocol-specific SSH key or password, but the
  credential maps to the same Vesta principal and role.
- Offline recovery material remains separate by design so access can be restored
  after all online device credentials are lost.

### Authorization

- Access is explicitly granted by the server owner.
- The target model uses named, per-action capabilities and server-local roles,
  not only cumulative numeric levels.
- Authorization is checked for every request and resource. Loopback or LAN
  location alone does not grant remote API administration.
- Destructive operations use a two-step prepare/confirm protocol. The normal UX
  may still be a simple launcher "Are you sure?" dialog, but the server must bind
  confirmation to the exact operation, actor, target, and short expiry.
- Built-in roles are supplied for a safe initial setup, and owners can create
  custom roles from named capabilities.
- Invitations are role-specific, short-lived, and single-use. An authorized
  administrator selects the role before generating the invitation code.
- Remote delegation follows a strict role hierarchy and cannot grant equal or
  greater authority. Owner and Administrator grants are local wrapper-console
  operations only. Administrators may invite Maintainers and lower;
  Maintainers may invite Moderators and lower only when explicitly granted a
  delegation capability (their default role cannot manage roles). The same
  rule continues down the hierarchy. Custom roles carry an explicit delegation
  rank, but rank alone never permits granting capabilities or resource scopes
  the delegator does not possess or is forbidden to delegate.
- Built-in roles are Owner, Administrator, Maintainer, Moderator, Viewer, and
  Pack User. Pack User is a limited server principal with pack access and a safe
  status view, not general management access.
- The initial product assigns one primary role to a principal, plus optional
  per-principal capability overrides. The persistence and policy model uses role
  assignments rather than a single role column so multiple-role composition can
  be enabled later without migrating identity records.
- Effective policy supports inherited role allowances and explicit principal or
  scoped denies. Explicit deny wins over inherited allow; an individual allow
  cannot bypass a hard security invariant or grant beyond the delegator's scope.
- Role assignments and individual overrides may have an optional expiry. Expired
  authority stops applying immediately and produces a notification/audit event;
  permanent grants remain the ordinary default.
- File capabilities can carry normalized path scopes and separate list, read,
  create, overwrite, rename, mkdir, and delete rights. For example, a role may
  edit `config/**`, read `logs/**`, and have no world access.

### Built-in role defaults

- Owner has all server-local authority, including local recovery and granting
  Administrator, subject only to hard security invariants.
- Administrator has broad remote operational, configuration, identity,
  integration, and pack authority, but cannot remotely create Owner or
  Administrator, invoke local recovery, erase protected security history, or
  bypass hard security rules.
- Maintainer can manage lifecycle, raw console, files, content, backups,
  schedules/workflows, maintenance, and diagnostics by default. It cannot manage
  roles, credentials, security policy, owner recovery, or agent installation.
  These broad trusted-operator defaults are confirmed; owners may create narrower
  or more specialized custom roles by tweaking the capability presets.
- Moderator defaults to player visibility and typed moderation actions. Raw
  console and unrestricted file access are disabled unless explicitly granted.
- Viewer can see detailed server status, metrics, online player identities,
  activity history, and task progress without mutation rights. Logs and console
  output are separate opt-in capabilities because they may contain sensitive
  information.
- Pack User receives pack access and the limited status surface only.

### Trust boundaries and powerful operational permissions

- Permission to install executable mods/plugins, replace startup code, or alter
  code-loading JVM arguments is permission to execute code as the Minecraft OS
  identity. Raw console can also invoke powerful installed plugin commands.
  Vesta describes these capabilities as trusted-operator access, not as a
  contained or harmless form of file editing. Maintainer's current defaults
  imply that trust; a narrower custom role should be used for limited helpers.
- The recommended service installation separates the Minecraft OS identity
  from the management agent's identity and protects agent state, vault keys,
  supervisor control IPC, update files, and locally approved script definitions
  with OS permissions. Merely hiding paths in the panel is not isolation.
- Locally approved presets and launch scripts must not be editable through a
  lower-trust file capability. If their content changes, approval/hash validation
  is required before execution. Connector IPC cannot acquire agent-level powers.
- Portable same-user operation remains possible, but clearly reports its weaker
  isolation: a malicious plugin may access anything that OS user can access.
  No API permission system can contain arbitrary code running as its own identity.
- File scopes also govern archive creation/extraction, trash recovery, backup
  browsing/selective download, and exports; those routes cannot bypass a denied
  path. Full recovery exports and restoring backups remain owner-authorized.
- Typed moderation actions validate identifiers and each argument using the
  selected server adapter; newlines/control characters and command interpolation
  cannot turn a limited action into extra raw-console commands.

### Pack access versus management access

- Pack download authorization and server-management authorization are separate
  policy domains. Having permission to join or sync does not imply permission to
  administer the server.
- Pack access supports at least `public`, `password`, and `minecraft-policy`
  policies.
- `minecraft-policy` verifies the Minecraft UUID and evaluates the Minecraft
  server's real admission data rather than maintaining an unrelated Vesta list:
  whitelist state and `whitelist.json`, plus player bans where applicable.
- A player does not need to run an in-game link command before receiving a pack;
  a player unable to obtain the pack may not be able to join and run that command.
- Administrative roles are granted through local bootstrap/recovery or an
  explicit role-scoped device invitation.
- Offline-mode servers cannot use Mojang-backed `minecraft-policy`. They may use
  password-gated pack access, or public pack download with only a limited public
  status view.
- Microsoft/Minecraft authentication may still prove an online Minecraft UUID
  for device identity or explicitly granted management access when the Minecraft
  server itself runs in offline mode. That does not automatically make the UUID
  equivalent to an offline-mode player identity.
- Offline-mode owners explicitly accept that Minecraft player identity is not
  cryptographically trustworthy. Vesta does not invent a stronger mapping or
  treat a linked online UUID as the joining offline player. Maintenance and
  admission use the server's actual whitelist data as-is, with future external
  authentication adapters left as a later extension.
- If a UUID fails the server admission policy, including a banned player or a
  player absent from an enabled whitelist, it receives only the configured
  public/password fallback access rather than Minecraft-policy access.
- Limited Pack User/public status contains server branding, online state, player
  count without player names, MOTD, Minecraft/loader version, pack name/version,
  and required-update state. It excludes detailed metrics, logs, identities,
  paths, software inventory, security configuration, and management data.

### Pack revisions and update policy

- Pack state distinguishes the latest available revision from the minimum
  revision currently allowed to join.
- The sync/status contract accepts the client's installed revision and reports
  one of `current`, `update_available`, or `update_required`.
- The active status records the latest revision and minimum required revision so
  clients and operators can explain why an update is required.
- New installations always receive the one currently hosted latest revision. An
  existing installation below latest but at or above minimum receives
  `update_available`; one below minimum receives `update_required`. Hosting only
  the latest does not invalidate an already installed still-allowed revision.
- Owners may publish an optional pack, an optional update, or an immediately
  required update. There is no scheduled grace-period enforcement in the
  baseline because mismatched client/server mods frequently cannot interoperate.
- Every Build/Publish increments revision. During publication the owner chooses
  whether minimum revision remains unchanged (optional update) or advances to the
  new revision (immediately required update).
- Synchronization uses an internal automatically increasing integer revision.
  Pack presentation also has an owner-managed numeric/dotted display version.
  The owner increments that value manually; publishing a lower value is allowed
  only after an explicit warning because revision, not display ordering, remains
  the authoritative update signal.
- Only the current published pack manifest and its files are hosted. Previous
  pack states are retained through the configured backup system rather than a
  public version-history API.
- The editable pack workspace is a draft. Server-side mod changes never silently
  mutate it; Vesta calculates suggested additions, removals, and updates and marks
  the pack potentially outdated until an authorized user applies or dismisses
  them. Clients continue receiving the current published revision until a manual
  Build/Publish succeeds. The owner receives a persistent pending-publication or
  server/pack-mismatch notification while action is needed.
- Build and Publish are one durable action: validate and display the complete
  release diff, generate optional release notes, build/sign in staging, increment
  the monotonic revision, and atomically activate the new result.
- Hosted client-pack release is distinct from updating code in the Minecraft
  server process. A successful normal Build/Publish makes the complete new pack
  revision available immediately; it does not wait for Minecraft to restart.
- Owners may optionally schedule when a new pack revision becomes available.
  Scheduled release freezes the approved built artifact/diff, keeps the current
  pack public until activation, and revalidates authority and expected current
  revision before the switch. A draft edit or another intervening release cannot
  silently substitute different content or change the approved minimum revision.
  Only the active revision is publicly hosted; scheduled staging is private and
  storage-bounded. Failed/incomplete builds never replace the active pack.
- The release diff classifies additions, removals, version changes, stale
  suggestions, custom overrides, symbolic links, and server/pack mismatches.
- Rollback restores a selected backup into staging, validates it, and publishes
  the restored state as a new monotonic revision.
- Installing a modpack into the Minecraft server directory (not releasing its
  hosted client pack) uses the same transactional model as launcher instance
  updates: resolve and validate a complete plan, create the configured safety
  snapshot, stage downloads, verify hashes, journal application of the file set,
  persist the new revision, and retain rollback metadata.
  A multi-file server-directory change is not claimed to be one filesystem-atomic
  operation; locks and a recoverable journal protect intermediate states.
- Pack possession and revision claims are compatibility signals, not a security
  boundary. The agent does not attempt forgeable client attestation, and the
  connector is not required to prove that a non-Vesta client has the pack.

### Live server files and hosted modpack publication

- The Minecraft server directory is live. File-manager, external filesystem,
  mod, plugin, and configuration changes apply to that directory immediately;
  there is no unpublished server-files draft.
- File changes never trigger an automatic restart. The standard restart control
  remains available and Vesta exposes a low-pressure changed-since-start and
  restart-required status when it can determine one.
- The managed updater stages verified mod/plugin/server-JAR replacements and
  removals while Minecraft runs, then applies them after actual process exit on
  the next approved restart. Ordinary direct file management remains live, with
  executable-file risk warnings. This is not a server-file publication workflow.
  Java may load classes/resources after startup; loaded code is not a guarantee
  that replacing a running JAR is safe. Hosted client-pack releases remain
  immediate unless the owner explicitly schedules their release.
- A custom hosted modpack has its own Vesta-managed workspace outside the live
  Minecraft server directory. It is never generated by scanning or copying the
  server root. The workspace contains the pack manifest/state plus a dedicated
  overrides directory for custom configuration and files.
- The manifest remains schema-compatible with the launcher's existing
  `VestaIndex`: format version, name/version, revision/minimum revision,
  Minecraft/loader identity, dependencies, and file entries containing path,
  size, hashes, environment rules, and source metadata. Additions use a versioned
  backward-compatible contract rather than a second Vesta format.
- Platform content is represented primarily by immutable metadata references:
  provider, project ID, version/file ID, expected hash, filename, and required
  environment. Vesta does not duplicate Modrinth/CurseForge-hosted bytes inside
  the pack merely because the same artifact is installed on the server.
- Each published revision records an exact provider version/file ID and expected
  content hash. Editing the draft to select another provider version is normal,
  but a published revision never resolves a mutable `latest` selector or accepts
  different bytes under the same identity.
- When installing/updating a server mod whose trusted metadata declares a client
  or shared requirement, Vesta offers a suggested manifest change but does not
  apply it automatically. Server installation and pack editing/publication remain
  separate operations. Updating/removing a matching server artifact produces an
  outdated/stale pack suggestion; ambiguous metadata asks rather than guesses.
- Client-only provider mods can be added directly to the pack manifest and are
  downloaded by the launcher; no copy is placed in the server `mods` directory.
  Client-only custom/private files are explicit overrides.
- Custom uploaded content without an external immutable source may be placed in
  the pack overrides workspace through an explicit pack operation. It is not
  implicitly copied from the live server tree.
- Custom packs use the Vesta Pack manifest model aligned with the launcher's
  persisted instance/modpack state. The hosted representation is a small JSON
  manifest plus an optional compressed overrides archive, each hashed and bound
  to the same monotonic pack revision.
- The launcher downloads referenced Modrinth/CurseForge artifacts directly from
  their provider. Missing or withdrawn content fails with a clear affected-file
  error. A validated provider-byte cache is optional, opt-in, storage-bounded,
  and subject to provider redistribution terms.
- For manual portability, canonical `.vestapack` export wraps the manifest,
  overrides archive, metadata, and signatures into one container. Import may
  continue accepting `.vpack` as a compatibility alias. Network sync serves the
  manifest and overrides archive directly rather than expanding thousands of
  individual hosted files.
- One overrides archive is the baseline because custom content is expected to be
  small or absent. Archive splitting is deferred until measured pack sizes show a
  practical need.
- Importing an existing launcher instance or Vesta pack into the workspace is a
  destructive replace operation: preview, exact confirmation, and movement of
  the previous workspace into configurable pack trash precede replacement.
- Published manifest bytes and the overrides-archive hash are signed by the
  server identity and bound to server ID and revision. Paired launchers reject
  invalid signatures, hash mismatches, and revision rollback.
- Executable permission is not a remotely supplied pack-manifest capability.
  Pack extraction uses safe platform defaults and cannot mark downloaded content
  executable merely because a manifest requests it.
- The overrides workspace and imported Vesta packs may contain symbolic links.
  The versioned manifest records link path and target explicitly rather than
  silently treating a link as an ordinary file.
- Pack build/import scans links without following targets outside the workspace.
  Absolute, cyclic, special-file, or sandbox-escaping targets are rejected;
  contained relative links may be preserved for a sandbox-capable launcher.
- Connecting to a server pack and manually importing a `.vpack` or `.vestapack`
  containing links displays a specific warning and the affected paths before
  acceptance. The launcher must validate containment again at extraction time.
- Symlink acceptance is remembered only for the exact signed pack revision. A
  later revision containing links requires a new warning/acceptance so link
  targets cannot change without the user noticing.
- Pack metadata includes icon, description, author, optional changelog/release
  notes, recommended RAM, and optional support/homepage links. When release notes
  are omitted, Vesta proposes an editable summary generated from the release diff.
- Optional client entries are a supported Vesta manifest capability, including a
  default enabled state and user selection before installation/update. More
  complex dependency groups, conflicts, and mutually exclusive choices may be
  phased after the basic optional-file implementation.
- Pack builds occur only after an explicit authorized Build/Rebuild action;
  automatic, debounced, and scheduled rebuilds are outside the baseline.
- `.vesta-packignore` applies only to the dedicated overrides workspace;
  `.vesta-fileignore` applies to HTTPS/SFTP visibility in the live server root;
  and `.vesta-backupignore` applies only to backup inclusion. Each uses documented
  gitignore-style semantics.
- File-browser visibility defaults to hiding nothing in the Minecraft server
  tree except invariant protected Vesta state, credentials, keys, repositories,
  and other security-sensitive agent internals.
- A build stages and verifies a complete manifest/archive pair, then atomically
  replaces the current published pair and increments the revision. The previous
  pack remains downloadable during the build; partial or mixed output is never
  exposed. Only the current revision is hosted after publication, with older
  pack workspace states available only through configured backups.
- While building, Vesta-managed writes to the manifest and overrides workspace
  are rejected with a clear build-in-progress result. External mutation is
  detected and invalidates the build.

### Pack authorization

- Owner and Administrator can manage, build, and publish packs by default.
- Pack management uses dedicated capabilities so a custom Pack Maintainer role
  can edit the draft, manage provider references and overrides, build, publish,
  and inspect pack status without receiving live server-file or console access.
- The Pack Maintainer preset may publish optional or required updates and raise
  `minimum_revision` by default. Owners can remove those capabilities or split
  edit/build/publish/force-update authority through custom role configuration.
- Pack replacement/import uses ordinary pack-draft management authority plus the
  destructive-operation confirmation; it does not require a mandatory special
  role capability. Owners may still separate it through a custom deny/override.

### Server operations

The complete baseline includes:

- Existing-server adoption and new-server installation.
- Start, stop, restart, force kill, crash detection, and restart policy.
- Live console, durable structured logs, and log search/download.
- CPU, memory, disk, network, process, player, and tick-health monitoring where
  the selected process/connector mode supports them.
- A constrained file manager and structured editors for known configuration.
- Mod, plugin, modpack, server-JAR, Java runtime, and safe update workflows.
- Player list, whitelist, OP, kick, ban, pardon, kill, and lightning-strike
  operations.
- Backups, retention, integrity verification, restore, and recovery.
- A durable scheduler owned by the agent.
- Durable notifications that launchers can fetch after being offline.
- Optional email, Discord, and generic webhook notification sinks.
- Signed agent updates with automatic checks, explicit installation approval by
  default, staged activation, and rollback.
- Update checks may run at startup and periodically, but update installation and
  any required restart remain explicit owner-authorized operations.

Dedicated advanced world editing is not a baseline goal. World files and world
configuration remain covered by files, configuration, backup, restore, import,
and export operations.

The Java Edition compatibility target includes Vanilla, Paper/Purpur,
Fabric/Quilt, Forge/NeoForge, and Sponge. Velocity process support is planned
after the single-server baseline. Bedrock is out of scope while Vesta itself is
Java-only.

Minecraft Java 1.7 and newer is the minimum protocol/version target. Vesta should
support as many later versions as practical through versioned server adapters and
capability detection rather than assuming every command, log format, loader, or
configuration key is universal. Pre-1.7 legacy status protocols are deferred.

### Lifecycle, maintenance, and sleep mode

- Graceful stop first sends Minecraft's normal `stop` command and enters a
  visible `stopping` state. The launcher Stop control may become a separately
  authorized Force Kill control while shutdown is pending; the timeout and
  unattended workflow behavior are configurable.
- Crash restart has a configurable crash-loop breaker. The recommended preset is
  three failures within ten minutes, after which Vesta leaves Minecraft stopped,
  marks it unhealthy, and emits a critical notification.
- Maintenance mode is a first-class state with a configurable reason/MOTD and an
  admission overlay for owners, administrators, and explicitly allowed players.
  It may reject new joins immediately, wait for non-maintenance players to leave,
  or remove them after a configured deadline before maintenance work starts.
- Linked Minecraft UUIDs for Owners and Administrators are included in the
  recommended maintenance admission preset, with an additional explicit list of
  allowed maintenance players. Owners may configure a narrower policy.
- The panel/API exposes the preserved normal whitelist and active temporary
  maintenance list as two distinct resources. Editing one never silently edits
  the other, and the UI explains which list applies after maintenance ends.
- With a trusted connector, the admission overlay is enforced in-process without
  rewriting the normal Minecraft whitelist. Without a connector, Vesta snapshots
  the exact whitelist contents and enabled state, transactionally installs a
  temporary maintenance whitelist, reloads it, and journals restoration. Normal
  start after interrupted maintenance first restores or explicitly reconciles
  that saved state.
- Entering and leaving maintenance are workflow-backed transitions. Owners may
  compose permitted pre-maintenance and post-maintenance actions, notifications,
  delays, scripts from local presets, backup, player countdown, kick policy, and
  the main maintenance operation without remotely uploading arbitrary code.
- A failed pre-maintenance action aborts entry by default and reports the failed
  step. An Owner may explicitly continue or configure failure behavior for
  actions that are safe to ignore.
- Optional sleep mode gracefully stops Minecraft after a configurable period
  with zero players, while the Vesta agent remains online. The public API and
  launcher distinguish `sleeping`, `waking`, `starting`, `maintenance`,
  `crash_loop`, `recovery_required`, and ordinary `offline` states.
- While sleeping or otherwise intentionally offline, a lightweight Java Edition
  status responder may temporarily own the Minecraft TCP port. It returns a
  configurable protocol-compatible status/MOTD and can trigger a queued start on
  a login connection. Server-list status pings never wake Minecraft.
- A login connection that triggers wake receives a configurable disconnect
  response such as "Server is starting; reconnect shortly." Vesta does not claim
  that the original login session can be transparently transferred to a process
  that has not started.
- Concurrent or repeated login attempts coalesce onto one durable start operation
  and are rate-limited per source address. They cannot create parallel launches
  or bypass a start cooldown.
- Before Minecraft starts, the status responder closes and releases the game
  port; readiness requires successful process bind and status probing. When the
  actual server is online it owns the port and its normal status response unless
  a future always-on proxy mode is explicitly enabled.
- Wake behavior reports compatibility limitations for CGNAT tunnels, TCP
  forwarding services, modded handshakes, multiple game ports, and status bots.
  Status monitoring remains available without waking the process. Any compatible
  login attempt may request wake; authorization is still enforced by Minecraft
  after startup because the temporary responder cannot securely authenticate a
  UUID.
- Vesta does not place an always-on proxy in front of a running server in the
  baseline. The temporary responder cannot transparently transfer the triggering
  login connection and does not intercept live traffic.
- Automatic sleep performs a configurable in-game warning/countdown, is cancelled
  if a player joins, and is deferred while incompatible backup, update,
  maintenance, restore, or file operations are active.
- Once the crash-loop breaker activates, network wake attempts return the
  configured unhealthy status/message but cannot start Minecraft until an
  authorized user clears the condition.
- Without a connector, server-type/version-specific console parsers maintain the
  observed online-player set from join, leave, kick, and shutdown events. Vesta
  reconciles that set after startup and whenever evidence is incomplete so a
  missed line cannot incorrectly trigger sleep.

### Minecraft status and MOTD

- Status configuration distinguishes an online/live MOTD from offline-state
  templates. Templates exist for sleeping, waking, starting, maintenance,
  stopping, crash loop, recovery required, and ordinary offline states.
- Templates support documented safe placeholders such as server name, current
  state, player counts, Minecraft/loader version, maintenance reason, queued
  operation, elapsed state time, estimated readiness when known, and a concise
  public-safe failure code. Secret paths, exception text, and administrative
  detail are never available as public placeholders.
- The editor provides a two-line visual preview, Minecraft color/style controls,
  placeholder insertion and preview values, protocol-limit validation, and an
  authorized raw-text mode.
- Owners may configure a normal icon and state-specific icons for offline,
  sleeping, starting, maintenance, and unhealthy/failure responses, with a
  generated fallback when a selected asset is invalid.
- Offline-state status defaults to a truthful `0/0` player display. Maximum
  players and displayed version text may be configured per state, but Vesta does
  not fabricate named or nonzero online players.
- The structured server-settings editor manages the ordinary live MOTD in
  `server.properties`, reports its reload/restart behavior, and preserves raw
  editing access for authorized users.
- Vesta uses a safe server-specific reload only when the implementation proves
  one is supported. It does not invoke broad plugin reload commands merely to
  refresh an MOTD; otherwise the setting is marked Minecraft-restart-required.
- When a compatible connector is installed, it may implement dynamic online
  status-ping/MOTD responses without rewriting `server.properties`. Without a
  connector, the actual running server owns its live status response and Vesta
  does not claim it can override that response dynamically.
- The launcher/app status uses the same canonical lifecycle state and template
  inputs as the game-port responder, while presenting authorized diagnostic
  details separately from public MOTD text.

### Files and content

- The remote file view covers the Minecraft server root but permanently hides
  agent internal state, credentials, private keys, update state, and backup
  storage.
- Deletes and overwrites are recoverable by default through a quota- and
  retention-bounded trash system. Operations preflight required disk space and
  never silently consume the server's remaining capacity.
- Trash uses both an absolute and percentage ceiling, with the lower limit
  winning. The initial recommended defaults are 10 GB, 10 percent of the volume,
  and seven-day retention; all are configurable.
- Supported content origins include Modrinth, CurseForge, launcher uploads,
  local files, and explicitly authorized arbitrary URLs.
- Hash verification is mandatory for arbitrary URL content. URL fetching also
  requires SSRF defenses, scheme/redirect restrictions, size limits, and clear
  unverified-origin labeling; a checksum proves byte identity, not trustworthiness.
- Hashless arbitrary URL downloads are refused.
- Mod and plugin changes are explicit manual operations. Replaced artifacts use
  recoverable file transactions/trash rather than creating a full server backup
  for every individual change.
- Modpack updates recommend or create a safety snapshot according to backup
  policy because they can change a large coordinated file set.
- Text reads return a content revision/hash. Edits use optimistic concurrency
  (`If-Match`-style) and reject stale writes with enough metadata to compare and
  deliberately retry, preventing one administrator from silently overwriting a
  newer edit.
- Large HTTPS/launcher uploads are resumable, chunked, and verified per chunk and
  as a completed object. Interrupted upload staging is quota-bound and expires
  automatically.
- Upload, download, extraction, archive, staging, and concurrent-operation limits
  start from secure presets calculated against available storage and host
  resources, with explicit owner-configurable ceilings and hard safety reserves.

### Backups

- Storage targets include local disk, mounted paths/NAS, and S3-compatible object
  storage.
- The portable online-backup baseline coordinates Minecraft writes with
  version-supported save suspension and acknowledged flush (normally `save-off`
  followed by `save-all flush`), keeps autosave disabled while the backup engine
  reads the server tree, and restores its previous state on completion, failure,
  or cancellation using the crash-safety contract below. This trades minutes of
  world-save latency for a stable flushed Minecraft world without requiring
  temporary space equal to the server size.
- `save-off` does not freeze every file in the server directory: logs and some
  plugins/mods may continue writing independent data. Without a filesystem
  snapshot or plugin-specific quiesce adapter, Vesta describes the world as
  save-consistent and the remaining live files as best-effort rather than
  claiming the entire directory is transactionally frozen.
- Online backup may issue configurable advance and start notifications to the
  server console, players, launcher inbox, and external notification routes.
- The recommended pre-backup player warning is 60 seconds. Its duration,
  messages, destinations, countdown cadence, and whether an authorized manual
  backup may skip it are configurable.
- For the duration of a backup scan, Vesta rejects conflicting HTTPS file API,
  SFTP, pack-build, content-update, and other Vesta-managed mutations. Direct OS
  access and independent plugin/mod/log writes cannot be safely blocked without
  stopping the process; Vesta detects observable source mutations and records
  them in the backup result.
- Source-mutation handling is configurable per backup profile: finish with a
  warning, fail when protected/important paths change, or fail on any detected
  mutation. The safe preset fails on world-data mutation and warns for known
  volatile logs/cache.
- Mutation detection is best-effort, not proof that independent writes never
  occurred. A point-in-time filesystem view still requires writer coordination
  for plugin/application consistency; otherwise its result is crash-consistent.
- An advanced optional write guard may temporarily deny the managed Minecraft OS
  identity new writes to configured paths and restore permissions from a durable
  journal afterward. It is disabled by default because existing open handles may
  remain writable and denied writes can break plugins/mods or lose their pending
  data. Vesta tests the guard, excludes required writable paths, warns players,
  and never labels this mechanism alone as a guaranteed filesystem freeze.
- The write guard is configured per path and leaves Minecraft/agent logs and
  required runtime/temp paths writable in its recommended preset. If original
  permissions cannot be restored exactly, Vesta enters recovery-required state
  and blocks Minecraft launch until locally repaired or explicitly reconciled.
- Backup inclusion supports a separate gitignore-style `.vesta-backupignore`.
  It is independent from hosted-pack and file-browser visibility policy.
- Vesta separately detects optional point-in-time filesystem facilities such as
  Windows VSS, Btrfs, ZFS, or LVM. Here, "filesystem snapshot" means a temporary
  frozen source view, not a dated Vesta backup. When available and enabled, Vesta
  may hold `save-off` only through flush and snapshot creation, restore `save-on`,
  and let the backup engine read the temporary snapshot while play continues.
- Filesystem-snapshot acceleration is configurable and never required. Vesta
  owns no new filesystem or volume format and falls back to the portable
  save-suspension flow when support or privileges are unavailable.
- Temporary filesystem snapshots are read-only backup sources and are deleted
  after use. They are never automatically restored and do not alter the owner's
  dated Vesta backup history.
- Stopped-server backup is the universal safe fallback.
- Backup manifests include checksums and sufficient metadata for verification
  before restore.
- Backup creation and automated backup schedules are opt-in. The agent may
  recommend a safety backup before risky changes but does not silently enable a
  recurring backup policy.
- Incremental, deduplicated snapshots are the target design, with configurable
  retention and storage quotas.
- With restic, all backup repositories are encrypted and authenticated, including
  local disk and NAS; it has no unencrypted repository mode. Setup generates a
  repository secret, stores it in the protected agent vault, and guides the owner
  to preserve a recovery copy. This does not require another everyday login.
  An owner-chosen repository passphrase is an advanced alternative. A separately
  authorized plaintext file export is not an unencrypted restic backup.
- Restore supports full in-place replacement, selective paths, and restoration
  into a separate inspection directory.
- No failure, crash loop, failed update, failed health check, or startup error
  automatically restores a backup. Restoration always requires an authorized
  owner decision and exact-operation confirmation.
- Full restore always stops the Minecraft process. Before destructive restore,
  Vesta creates a pre-restore safety snapshot when storage permits; proceeding
  without one requires an explicit owner override.
- Vesta orchestrates a bundled, signed mature backup engine such as restic for
  content-defined deduplication, authenticated encryption, repository integrity,
  and cross-platform storage support instead of implementing new backup
  cryptography.
- Backup history, maximum count, maximum repository size, retention periods, and
  dated restore points are independently configurable regardless of whether a
  temporary filesystem snapshot was used to create them.
- Repository-size limits are storage budgets with capacity preflight and a
  working-space reserve, not a promise that every incremental backup or prune
  can fit under an exact hard limit. Cleanup never silently deletes unexpired
  restore points to satisfy that budget.

> **Owner amendment (2026-10-12), superseding the restic and encryption clauses
> of backups-019 and backups-023:** Vesta does not bundle third-party backup
> software. It uses a built-in Rust engine: content-addressed (SHA-256),
> deduplicated, compressed snapshots with integrity checks, stored in a local
> directory or an S3-compatible bucket. Repositories are not encrypted; owners
> who need confidentiality use encrypted storage or a private bucket. The baseline
> bullets above stay frozen for traceability. Read them with this amendment.

### Backup crash-safety contract

- Online backup requires an adapter capable of reliable save commands and flush
  acknowledgement. Exact commands vary by Minecraft/server version; unsupported
  variants use a compatible adapter or the stopped-server fallback.
- A normal finally/cancellation handler is insufficient after an agent crash.
  Before save suspension, the supervisor records a bounded save lease tied to
  that exact Minecraft process generation. Expiry or lost agent ownership aborts
  the scan and restores the previous autosave state independently of the agent.
- Restoring autosave cannot accidentally enable it when the owner deliberately
  had it disabled, or send cleanup commands to a newly started process. An
  adapter reports the prior state where possible; otherwise online backup
  requires an explicitly configured/acknowledged autosave baseline.
- If the selected process adapter cannot provide this cleanup guarantee, Vesta
  explains the limitation and offers stopped-server backup instead. After host
  restart, journals are reconciled before launching or resuming operations.
- VSS or other snapshot privileges are isolated to a narrowly scoped local
  helper; enabling snapshot support does not make the public agent an OS admin.

### Scheduling and notifications

- Schedules execute locally without a launcher connection.
- The scheduler is a durable typed workflow engine, not only a cron/action list.
  Workflows compose built-in steps and locally installed script presets in an
  owner-defined order.
- Steps have typed inputs and outputs. Workflows may add delays, pass outputs to
  later steps, branch on success/failure or validated output conditions, and set
  retries, timeouts, and failure behavior. Example: create backup; on success,
  clean trash; then emit a notification.
- Scheduled actions, workflow state, outputs, retries, and failures are durable
  and auditable across agent restarts.
- Notifications are stored locally and track read/acknowledged state per Vesta
  principal, with delivery cursors so any of that principal's devices can fetch
  events missed while offline without creating separate notification accounts.
- External delivery integrations are optional and must not be required for core
  operation.
- Built-in steps include backup, backup verification, start/stop/restart,
  console command, delay, update check/install, trash cleanup, notification, and
  local script preset. Notification is a real workflow action with selected
  destinations and severity.
- Arbitrary remote script authoring is not exposed. Scripts/hooks are installed
  locally as server-side presets; launcher users with permission may select a
  preset and allowed parameters when creating a schedule.
- Script presets run under the server's unprivileged identity with fixed working
  directories, environment allowlists, timeouts, output limits, parameter
  validation, and an auditable content hash.
- Preset definitions can be reloaded without restarting the agent after schema,
  path, ownership, and security validation.
- Initial workflows are sequential with conditional branches and no loops or
  parallel branches.
- Triggers include manual, calendar/cron, fixed interval, agent startup, server
  start/stop/crash, first-player-join, last-player-leave/player-count-zero,
  update available, backup completion/failure, and resource thresholds.
- Backup schedules and retention are opt-in and configurable. Presets may include
  minimal daily retention and daily/weekly/monthly policies.
- Minecraft log rotation/cleanup is independently configurable by age, total
  size, and free-space policy.
- Each workflow has an execution principal. Current role, resource scope,
  credential/grant expiry, and preset approval are checked before each mutating
  step, not only at schedule creation. Revoked authority suspends affected work
  and creates an audit event/notification; schedules cannot preserve old access.
- Missed schedule occurrences have an explicit policy: skip and notify, run the
  latest eligible occurrence, or bounded catch-up. Dangerous/non-idempotent work
  is never inferred from historical console lines or blindly repeated. The
  confirmed default is skip-and-notify, configurable per workflow. Reconciliation
  of notifications, process state, and already-started operations is separate
  and still occurs after recovery.

### Durable operations and concurrency

- Every long-running or materially mutating action is represented by a durable
  operation with an identifier, actor, requested inputs, lifecycle state,
  timestamps, progress, safe-to-cancel state, result/error, and audit correlation.
- Operations include backups, verification, restore, updates, Vesta Pack builds,
  archive work, large file transactions, imports/exports, lifecycle transitions,
  diagnostics, and maintenance workflows.
- A persistent task queue survives launcher disconnection and agent restart.
  Authorized clients can subscribe to progress or reconnect and retrieve it.
- Named resource locks serialize conflicting work. Conflicts normally queue in
  request order; harmless redundant lifecycle requests may be coalesced or
  rejected with the existing operation identifier. Local recovery work has a
  controlled priority path.
- Restore, agent activation, pack publication, and other destructive transitions
  are exclusive against operations that could observe or alter inconsistent
  state. Read-only work may continue concurrently when its inputs are stable.
- Cancellation occurs only at declared safe points. If an operation cannot be
  safely cancelled, the API explains why and completes or rolls back the current
  transaction instead of abruptly terminating it.
- Stop and force-kill requests may receive queue priority, but never silently
  interrupt consistency-sensitive work. If a backup or comparable operation is
  active, the client must explicitly confirm cancellation of that exact
  operation before the kill proceeds; cleanup and operation state remain
  auditable.
- After process or machine failure, idempotent/safe work may resume or retry.
  Interrupted destructive work uses its journal to roll back or enters an
  explicit `recovery_required` state; it is never blindly replayed.
- Completed operation summaries default to 90-day retention. Retention duration,
  bounded task output, and total task-history storage are configurable; security
  facts required by audit policy remain independently retained.

### Console and player actions

- Raw Minecraft stdin executes with console authority and has no native remote
  user identity. Vesta therefore authorizes commands before writing to stdin.
- Vesta does not require owners to design a Minecraft command-policy language.
  Moderator-oriented operations use dedicated typed actions and safe built-in
  presets rather than unrestricted console strings.
- Administrators/Maintainers with explicit raw-console capability may use full
  console. A Moderator has no raw-console entry by default.
- Vesta does not provide `execute_as_player`. Platform inconsistencies and
  online-player requirements make it unreliable; dedicated moderation actions
  and explicit full-console permission are the supported boundaries.
- The player-management baseline includes online players, kick, temporary and
  permanent ban with reason, pardon, whitelist add/remove, OP/de-OP and OP
  level, kill, lightning strike, and auditable history.
- IP bans, mute, teleport, and inventory inspection are plugin/connector
  extensions unless later promoted to core actions.
- Console and log output is not heuristically redacted. Access is restricted to
  explicit log/console capabilities and all access is audited.
- Live console history is an in-memory 10,000-line ring buffer only. Vesta does
  not duplicate persistent console history because Minecraft already writes its
  own logs.
- The supervisor continuously drains stdout/stderr even without a viewer, so a
  full process pipe cannot stall Minecraft. The bounded buffer may include early
  JVM failures not present in game logs; this is not another persistent log store.
- Catch-up reads Minecraft's existing logs using file identity/offset cursors,
  with rotation/truncation handling and bounded scans of retained archives.
  Missing/deleted logs produce a visible history gap, not fabricated events or
  replay of commands. Connector/status reconciliation corrects uncertain player
  state before sleep or player-dependent automation resumes.
- Vesta retains its own bounded operational diagnostics and required security
  audit records; these describe Vesta actions, not a duplicate Minecraft log.
- Player join/leave history and aggregate activity are retained as metrics under
  the configured metrics retention policy.

### Isolation

- Sandboxing is optional but recommended.
- Docker is not the only isolation option and is not required on Windows.
- Baseline isolation uses a dedicated unprivileged OS identity, strict filesystem
  ACLs, process-tree ownership, resource limits, and network policy.
  Secure service setup uses distinct identities for the game and control plane;
  containers and stronger sandboxing remain optional additions.
- Hardened Linux profiles may add systemd sandboxing, namespaces/cgroups,
  seccomp, AppArmor/SELinux, or rootless containers.
- Hardened Windows profiles may add a restricted service identity, directory
  ACLs, Job Objects, and Windows Firewall rules.

### Java runtime

- One Java runtime is configured for the single server instance. Setup detects
  or installs a compatible Java version, and the owner may override it with a
  custom executable path.
- Runtime compatibility is validated before server installation, update, or
  launch.
- Setup may download and manage one signed compatible runtime distribution, with
  a user-supplied Java executable as an alternative.
- "One runtime" means the configured game runtime. If Vesta itself needs a
  different runtime, it is bundled/managed independently by the one installer;
  old Minecraft Java requirements must not force an obsolete agent runtime.
- Minecraft 1.7+ is a protocol/process compatibility target, not a promise that
  every optional connector, metric, loader, or online-backup command works on
  every version. The tested adapter matrix and capability discovery say exactly
  what is supported; unsupported features have an honest fallback.

### Metrics retention

- Raw operational samples are retained for 24 hours at approximately ten-second
  resolution.
- Five-minute aggregates are retained for 30 days.
- Daily summaries are retained for one year.
- Retention and total metrics storage have configurable ceilings and cleanup is
  performed locally.

### Resource controls and diagnostics

- Configuration may set Java memory, process priority, CPU affinity where
  supported, and disk warning/critical thresholds. Strong cgroup/Job Object
  limits are available only on platforms and isolation profiles that support
  them.
- The agent can refuse storage-expanding operations below a configurable critical
  free-space threshold while preserving owner recovery and cleanup operations.
- Critical-disk actions are configurable. Notification/refusal is the safe
  default; an owner may explicitly configure a workflow such as backup cleanup
  or server stop.
- A configurable low-disk policy may delete expired staging/temp files and
  expired trash, then prune only backup snapshots already eligible under the
  active retention policy. It never silently removes unexpired backups or live
  server data. Cleanup order, thresholds, and automatic execution are visible
  and configurable.
- An authorized user can generate and download a diagnostic bundle containing
  agent logs, Minecraft logs/crash reports, version/environment data, recent
  metrics, and configuration with known credentials and protected secrets
  removed.
- There is no automatic third-party log-upload integration in the baseline;
  diagnostic and log export is manual.
- Setup recommends Java memory from available host RAM and detected server type,
  while allowing owner override.

### Connector role

- The wrapper remains fully useful with a vanilla server and no connector.
- Loader mods/plugins are optional capability adapters for accurate player
  events, TPS/MSPT and tick health, coordinated world flushes, join enforcement,
  native whitelist/ban operations, and other in-process signals.
- The connector is not an administrative authority. Because it shares a process
  with third-party server code, a compromised mod/plugin environment must not be
  able to grant wrapper roles, access files, rotate credentials, or approve
  updates.
- The no-connector baseline relies on documented server commands, console/log
  parsing, status probes, and file inspection. Parsers are versioned adapters and
  surface uncertainty instead of inventing player or health state.

### Connectivity and NAT

- Direct authenticated HTTPS remains the mandatory self-hosted transport.
- The endpoint accepts DNS hostnames such as `play.example.net` as well as IPv4
  and IPv6 literals. IPv4 and IPv6 have first-class bind, advertise, firewall,
  certificate, and diagnostic handling.
- Entering a Minecraft address can discover the Vesta API through a
  `_vesta._tcp` DNS SRV record. A configurable HTTPS
  `/.well-known/vesta-server` descriptor is a secondary discovery path, and
  complete manual API URL entry is always supported.
- A compatible connector may advertise the same endpoint/server-ID hint in an
  extended Minecraft status response while the server is live; Vesta's offline
  status responder emits it while sleeping/offline. Discovery hints are untrusted
  until certificate/server identity and pairing are verified.
- Vesta serves no browser-based management panel. HTTPS exists for the launcher,
  public status, pack distribution, and third-party protocol clients; local
  bootstrap/recovery remains available through the wrapper CLI.
- One configurable HTTPS listener normally serves public status, pack, and
  authenticated management routes with per-route authorization. Optional SFTP
  remains a separate protocol/port.
- Initial operation may use LAN access, explicit port forwarding, a user-managed
  VPN/overlay, or a reverse proxy.
- A bring-your-own, self-hosted end-to-end encrypted relay is a later CGNAT
  feature rather than a dependency of the direct-connect release. It requires a
  publicly reachable owner-operated host; no Vesta-operated relay is mandatory.
- Relay transport, if implemented, must not be able to decrypt management
  traffic or impersonate either endpoint.
- The first networking implementation uses direct HTTPS plus diagnostics for
  LAN reachability, firewall rules, public addressing, and port forwarding.
  Automatic UPnP/NAT-PMP mapping is never enabled without explicit owner consent.
- Setup may create host firewall rules after explicit confirmation and verifies
  local bind/listener/firewall state. End-to-end public reachability is tested by
  a paired launcher on another network or an optional stateless external probe;
  core operation never depends on a Vesta-operated checking service.
- The agent advertises one canonical public endpoint. Launchers may manually
  retain LAN/VPN alternatives, but automatic multi-address priority/failover is
  deferred until a concrete need justifies its complexity.
- Optional LAN mDNS discovery may advertise a safe server ID/name and endpoint,
  but is a low-priority, onboarding-configurable convenience distinct from normal
  DNS hostname entry and reveals no credentials.
- A trusted reverse-proxy mode supports owner-operated HTTP proxies that
  terminate public TLS. Device-key proof remains application-layer authenticated,
  proxy source addresses are explicitly trusted, forwarded client-address headers
  are accepted only from those sources, and the origin hop is authenticated.
- Direct and TCP-passthrough deployments retain end-to-end mutual TLS. Reverse
  proxy termination does not silently downgrade into bearer-token-only access.
- Access modes are composable, not mutually exclusive presets. A reverse-proxy
  deployment can keep loopback/LAN management available while also exposing its
  configured public routes through the proxy.
- Onboarding can optionally detect a local Nginx/Caddy/Traefik installation,
  generate a reviewed Vesta site/upstream configuration, back up any touched
  configuration, validate it with the proxy's native checker, and reload only
  after success. It never silently installs or rewrites an unrelated proxy.
- Optional ACME certificate issuance/renewal supports owners with a domain. A
  locally generated pinned certificate remains the universal default, and ACME
  failure cannot remove CLI recovery or silently replace server identity.
- ACME HTTP-01 requires reachable port 80 and TLS-ALPN-01 requires port 443;
  neither automatically works merely because Vesta's chosen API port is open.
  Onboarding detects conflicts/exposure and recommends proxy-managed certificates
  or an owner-supplied DNS-validated certificate when direct validation cannot
  work. No DNS-provider secret is required for normal local/pinned setup.
- Direct deployments initially automate HTTP-01/TLS-ALPN ACME validation.
  Reverse-proxy deployments normally let the proxy own ACME. DNS-01 uses an
  owner-supplied ACME client/certificate path initially; provider-specific DNS
  credential integrations are deferred.
- Route exposure is independently configurable. Remote management may be
  disabled or limited to loopback/LAN while public status and authorized pack
  downloads continue. Roles still govern access whenever management is exposed.
- Disabling remote management immediately rejects new remote requests and closes
  existing remote management event/session channels; loopback CLI recovery is
  unaffected.
- Changing the advertised hostname may preserve pairings when continuity of the
  same server identity is proven. Launchers still require low-priority endpoint
  approval before sending management requests to the new address.
- Limited Minecraft-style public status remains available even when pack access
  is password-gated or Minecraft-policy-gated.
- Management and SFTP support configurable IP/CIDR allow and deny rules as an
  additional condition after identity authentication.
- Minecraft `banned-ips.json` remains a game-admission source, not the canonical
  management firewall. Owners may explicitly enable inheritance of its denied
  addresses into pack/management/SFTP network policy; independent Vesta rules
  remain available to avoid accidental owner lockout or shared-NAT surprises.

### Management authentication

- Management HTTPS uses mutual TLS. The agent presents its pinned server
  identity and each paired device has a server-scoped client certificate bound
  to its principal/device record.
- Public-CA certificates are supported for server transport identity without
  replacing per-device client authentication.
- The stable Vesta identity key is distinct from a renewable HTTPS certificate.
  Public-CA certificate renewal with valid hostname/chain and the same verified
  Vesta identity does not require pairing again. Locally pinned transport keys
  and the stable identity key have explicit authenticated rotation/continuity
  flows. An unexpected identity change or unproven pinned-key rotation blocks
  management until verified locally/out-of-band; failed TLS validation is not
  silently ignored. Public unauthenticated status may still be
  displayed with a prominent unverified-identity warning.
- Public status, discovery, and pairing must remain reachable without an existing
  client certificate. Listener/proxy configuration distinguishes these routes
  while every management route requires verified device authentication; an absent
  client certificate cannot downgrade management into unauthenticated access.
- Optional TOTP or a local security PIN provides configurable step-up
  authentication for selected sensitive operations. It is disabled by default.
- Local recovery requires OS-level authorization: elevation on Windows and root
  or the dedicated Vesta service identity on Linux.

### SFTP

- The agent provides an optional built-in SFTP service rooted in the Minecraft
  server directory for use with standard tools such as WinSCP and FileZilla.
- SFTP is disabled until explicitly enabled during owner onboarding or later
  configuration.
- SFTP and HTTPS file management share the same canonical file permissions,
  browser-ignore rules, protected internal paths, quotas, trash behavior, and
  audit events.
- Authentication prefers per-principal SSH public keys. Password credentials,
  if enabled, are separately generated/revocable and use the same role-derived
  file capabilities; management or pack passwords are never reused directly.
- A Vesta administrator uses the same local Vesta principal and role for SFTP,
  but registers a dedicated SSH public key or receives a separately revocable
  SFTP password. SFTP does not create an unrelated authorization database.
- The canonical SFTP username is the stable Vesta principal UUID. A linked
  Minecraft UUID may be accepted as an alias. Credential expiry is configurable;
  SSH keys and generated passwords can have different lifetimes.
- SFTP distinguishes list, read/download, create/upload, overwrite, rename,
  mkdir, and delete permissions. It prevents path/symlink escape, limits sessions
  and transfer rates, and logs authentication and file operations.
- SFTP writes use standard live-file semantics. During overwrite, the original
  may be moved into recoverable trash first, but the new target is visible as it
  is uploaded; interrupted transfers can leave a partial file and a recoverable
  previous version.
- SFTP and HTTPS file management support ZIP/TAR/GZIP archive creation and
  extraction with traversal, symlink, file-count, compression-ratio, expanded
  size, timeout, and disk-space protections.
- Remote ownership changes are blocked. Safe permission-bit changes are
  configurable on supported Unix filesystems and have no fabricated equivalent
  on Windows.
- The server SSH host key is generated locally, displayed during setup, and can
  be pinned by clients. SFTP listens on a separately configurable port.

### Secrets, credentials, and abuse controls

- Vesta minimizes credential sprawl by keeping one canonical principal and role
  assignment. Protocol credentials are authentication methods on that principal,
  not separate users, and each method can be listed, expired, rotated, or revoked
  independently from one place.
- Device private keys remain on the launcher device. The server stores only the
  public key/certificate and revocation metadata. Human-entered passwords and
  recovery passphrases are stored only as Argon2id verifiers when decryption is
  not required.
- Secrets that must be recovered in plaintext, such as Discord tokens, webhook
  credentials, SMTP credentials, and backup repository keys, live in an
  encrypted agent vault whose wrapping key is protected by the operating system.
  Windows uses service-identity/DPAPI protection; Linux uses service-only file
  permissions with an optional system keyring or TPM-backed key where available.
- Secrets never appear in normal configuration export, file browsing, metrics,
  notifications, task output, or diagnostic bundles. Replacement is supported
  without revealing the current value.
- Public status and pack endpoints have configurable per-IP and per-principal
  bandwidth, request-rate, concurrent-transfer, archive, and daily-transfer
  limits. Secure presets are enabled by default, including limits for anonymous
  clients.
- Management HTTPS and SFTP apply configurable request/body/transfer limits,
  connection and session caps, authentication backoff, temporary IP throttling,
  and per-principal storage/transfer quotas. Local recovery cannot be blocked by
  remote throttling or network deny rules.

### Structured configuration

- The management API provides schema-backed editors for `server.properties`,
  EULA state, JVM/memory settings, whitelist, bans, and recognized loader/server
  configuration while preserving raw text editing for authorized users.
- Vesta validates structured edits, writes atomically, preserves formatting when
  practical, and reports whether a reload or restart may be needed.
- Locally installed declarative schema extensions may add structured editors for
  server implementations and plugin/mod configuration. Raw text editing remains
  available, and schemas do not gain arbitrary agent code execution.
- Provisioning or managing MySQL/PostgreSQL databases is out of scope. Vesta may
  protect database credentials found in server files but does not own external
  database lifecycle.

### Data and recovery export

- Owners can export normal configuration for portability and an encrypted full
  recovery package containing roles, device registrations, schedules,
  integrations, SFTP registrations, notification policy, and audit metadata.
- Recovery export is distinct from the redacted diagnostic bundle. Secret-bearing
  recovery packages require owner authorization, strong encryption, and a
  separately preserved recovery key/passphrase.

> **Owner amendment (2026-10-12), superseding the encryption clauses above for now:**
> recovery exports contain no passwords or other credential material (no password
> verifiers, one-time codes, confirmations or tokens) and are therefore written
> unencrypted. Encryption and other export options may be added to the backup system
> later. The baseline bullets stay frozen for traceability.

> **Owner amendment (2026-10-13), extending the export options above:**
> credential-free recovery exports remain unencrypted by default, with optional
> passphrase encryption for local and owner-authorized remote exports. S3 settings
> and credential files are not part of the control-data export. Backup repository
> encryption is unchanged; this option protects only the exported recovery file.

### Agent state, repair, and pending changes

- Local durable state uses SQLite in WAL mode with foreign keys, versioned
  migrations, startup integrity checks, and a protected pre-migration snapshot.
  Database maintenance and snapshots participate in the operation queue.
- The SQLite state directory stays on a supported local filesystem, not a
  network share used for backups. Live database exports use SQLite's supported
  backup mechanism, not an inconsistent copy of the database/WAL files.
- Desired configuration and effective runtime state are tracked separately.
  Each changed setting declares whether it applies live, on agent restart, on
  Minecraft reload, or on Minecraft restart.
- The API exposes a compact pending-change summary, affected settings, required
  action, and validation errors. A launcher can render this persistently, such as
  in a status control at the bottom of its navigation sidebar.
- Roles, limits, notification routes, and schedules normally apply live.
  Listener/TLS/service changes require an agent restart; JVM, loader, and most
  Minecraft configuration changes report the appropriate reload/restart need.
- Breaking state corruption prevents Minecraft launch by default and Vesta does
  not expose normal remote management against uncertain authorization data and
  enters a local recovery-required mode. The owner may configure a narrowly
  defined availability policy for non-security-critical failures, but corrupted
  identity, authorization, path, or launch-integrity state always fails closed.
- Repair, restore, reset, or locally approved continuation requires an
  OS-authorized local confirmation and produces a permanent audit event.
- Automatic service-level restart of the Vesta agent after an agent crash is an
  owner-configurable watchdog toggle. It is independent of the Minecraft
  start-on-agent-start and crash-restart policies.

### Software update policy

- Update channels are `stable`, `beta`, and `nightly`; `stable` is the default.
- The agent checks for updates at startup and approximately every 24 hours.
- Checks and optional staging may be automated, but installation, agent restart,
  and Minecraft restart always require explicit authorization.
- Updates are signed, rollback-protected, staged, health-checked after activation,
  and retain the previous working agent version for recovery.
- Recovery of an interrupted, uncommitted file transaction is distinct from
  restoring a backup after a completed but unhealthy update. Vesta reconciles
  journals to a known transaction state; it never silently restores worlds or
  chooses an older pack/server state after a health check fails. Agent-binary
  fallback must preserve state compatibility and report exactly what happened.

### Installation experience

- The default installation path is an interactive Windows PowerShell or Linux
  shell walkthrough. It supports choosing a new/existing server directory,
  process mode, Java runtime, service/portable operation, ports/firewall, TLS,
  initial owner pairing, pack policy, optional connector, start/restart policy,
  backups, update channel, and hardening profile.
- The normal walkthrough asks only for a new/existing server, its detected or
  chosen implementation/version, intended access (local/LAN/internet), initial
  owner pairing, and opt-in features such as backups or a supported connector.
  Recommended settings are displayed together; advanced options expand only
  when requested. Owners are not required to understand IPC, TLS internals,
  script definitions, resource-limit primitives, or backup cryptography.
- Network onboarding offers composable Local, LAN, Direct internet, Reverse
  proxy, and VPN/overlay choices rather than forcing one exclusive topology. It
  configures only the selected listeners/routes and explains the exposure.
- Optional external port/reachability testing discloses that the public address
  is sent to a remote probe. Owners may instead complete an end-to-end test from
  a paired launcher on another network.
- System-service operation is recommended and starts the agent with the machine.
  Portable/manual operation and integration with an existing Windows service or
  systemd unit are also supported.
- Advanced/headless installations support documented non-interactive flags or an
  answer file using the same validation and backup acknowledgements.
- Uninstallation is a guided set of explicit choices. Removing the Vesta service
  does not implicitly delete the Minecraft server, Vesta state, downloaded Java,
  backups, or recovery material; each removable data class is selected and
  described separately.

### Audit retention

- Audit records are tamper-evident, locally durable, and exportable as structured
  JSON.
- Local tamper evidence is not protection against an OS administrator replacing
  the whole history. Optional external checkpoints/export can strengthen that
  guarantee without becoming a core service dependency.
- Each event records the principal, authentication method/device, source address,
  operation, target, outcome, timestamp, and correlation/operation identifier.
  Sensitive changes include safe before/after hashes or redacted summaries, not
  raw file contents, credentials, tokens, or command output containing secrets.
- General audit events have 180-day default retention. Security, ownership,
  credential, and recovery events are retained permanently unless removed by a
  local owner recovery operation with an audit export/warning.
- Protected audit growth has storage monitoring, reserved capacity, and explicit
  local export/archival guidance. Vesta never silently prunes protected history
  or continues unaudited sensitive mutations when durable recording has failed.

### Discord integration

- Discord is an optional outbound integration for notifications, status presence,
  and read-only slash commands such as uptime, online count, and safe server
  status.
- Optional Discord OAuth identity linking may associate a Discord user or guild
  role with a Vesta principal/role. Local Vesta authorization remains canonical,
  and Discord cannot grant Owner or Administrator authority.
- Guild, channel, user, and role allowlists constrain bot output. Bot and webhook
  tokens are protected agent secrets and never exposed through normal file APIs.
- Discord channels, visibility, slash commands, status presentation, event
  routes, Vesta role mappings, and whether supported actions are enabled are all
  owner-configurable. Presets provide a useful setup without hard-coding a
  community's access policy.

## Configuration philosophy

- Vesta ships secure, useful presets and an interactive setup path so an owner
  does not need to design policies from scratch.
- Presets are schema-backed configuration, not product lock-ins. Owners can
  customize roles, capabilities, pack rules, schedules, retention, notifications,
  Discord behavior, process policy, storage, network endpoints, and adapters.
- Presets can be reset, exported, imported, and diffed against current settings.
- Security invariants remain enforced even when surrounding policy is
  configurable: path confinement, protected agent secrets, authenticated local
  owner recovery, cryptographic verification, request limits, and auditable
  authorization cannot be disabled through ordinary remote configuration.

### Proxies and hosting extensibility

- Velocity can be treated as a supported Java server process without requiring
  multi-server orchestration in the first release.
- Proxy topology, forwarding secrets, backend network isolation, dependency
  ordering, and community-wide orchestration are later capabilities.
- The management protocol is public and versioned: an OpenAPI-described HTTPS
  request API plus a versioned WebSocket or Server-Sent Events stream for task,
  console, status, metric, player, and notification events.
- Compatibility negotiation, stable error codes, idempotency keys for mutating
  requests, pagination, event cursors, and deprecation windows are part of the
  protocol contract so third-party panels can be reliable clients.
- Hosting-provider service principals are a niche optional deployment feature,
  disabled by default. If enabled, they receive explicitly configured scopes and
  server access and cannot silently obtain or delegate local Owner authority.

### Extension and telemetry boundary

- The initial complete release supports declarative configuration schemas,
  connector capability adapters, outbound webhooks, and locally installed script
  presets. These cover likely integrations without loading arbitrary third-party
  code into the privileged agent process.
- A general in-process plugin API is deferred until concrete extension needs and
  a secure isolation/signing model exist. Future extensions must declare their
  capabilities, resources, compatibility, and trust source.
- Product telemetry is completely disabled by default and core operation never
  depends on it. Owners may explicitly opt into a documented anonymous diagnostic
  dataset, preview exactly what is sent, and revoke consent at any time.

## Security direction

The existing MVP security layer is a prototype and is not the final baseline.
The complete design must include at least:

- Argon2id for Vesta-owned human password verification. Mature backup engines
  retain their reviewed native key derivation rather than changing their format.
- Device public-key authentication and short-lived sessions.
- Per-device and per-principal revocation.
- Strict TLS identity/pinning and certificate rotation.
- Fine-grained authorization with deny-by-default policy.
- Request/body/concurrency/storage limits and abuse controls.
- Safe path resolution, protected internal roots, atomic writes, and operation
  transactions.
- Append-only audit events for every authentication, authorization, console,
  file, content, backup, schedule, settings, update, and power operation.
- Secret redaction, protected local storage, and platform-specific permissions.
- Signed update metadata and rollback protection.
- Threat-model and security regression tests before public release.

## Research influences

- Pterodactyl/Wings: isolated workloads, resource limits, server lifecycle,
  backups, file access, and a distinct node control plane.
- Crafty Controller: server-scoped roles, backup policy, scheduling, task chains,
  player operations, and differentiated permissions.
- AMP: host-level versus instance-level permissions, mature delegation, and a
  sleep mode where the panel listens on the Minecraft port while the game server
  is stopped. Field reports also show compatibility problems with some tunnels,
  modded setups, and alternate ports, which Vesta must diagnose and disclose.
- PufferPanel: explicit distinction between convenient shared-host execution and
  meaningful workload isolation.
- NIST Zero Trust: no implicit trust based on network location; authenticate and
  authorize access to each resource.
- OWASP API Security: object/function authorization, authentication, resource
  consumption, SSRF, configuration, inventory, and unsafe upstream consumption.
- The Update Framework: signed, compromise-resilient software update metadata.
- Restic/Windows VSS: restic can read from a Windows filesystem shadow copy, but
  VSS alone is only crash-consistent for applications without a VSS writer.
  Vesta must coordinate Minecraft's save suspension/flush around snapshot
  creation to establish the intended application-consistent point in time.

## Open requirements

### Product choices

The final outstanding managed-JAR staging proposal was accepted. No mandatory
product-choice question remains for the initial release; the decisions above are
the baseline. Optional future features remain explicitly deferred, not blockers.

### Engineering decisions for the final implementation plan

These are implementation work, not another mandatory owner questionnaire.

- Final capability matrix, delegation checks, and secure service identities/ACLs.
- Concrete versioned pack schema against launcher state, tested migration rules,
  and exact scheduler preset schema.
- Tested Minecraft/version/loader capability matrix, connector adapters, and
  crash-safe connector-free whitelist reconciliation.
- Cross-platform supervisor lifetime, degraded adoption, save-lease recovery,
  and OS shutdown integration tests.
- Maintained SFTP/cryptography libraries, proxy/device-auth verification, stable
  identity and transport-key rotation, and sensible measured resource presets.
- Storage preflight, safe backup/source exclusions, recovery-key walkthrough,
  and crash/cancellation/fault-injection test matrix.
- Evidence-backed repository audit, migration from MVP interfaces, acceptance
  tests, and phased implementation/security gates before building the HTML.
- CGNAT relay, multi-server orchestration, mutating Discord commands, and a
  general extension API remain later work; their precise designs are not needed
  to complete the initial release requirements.

## Review corrections and primary sources (2026-10-04)

- [Paper's update guidance](https://docs.papermc.io/paper/updating/) warns against
  replacing running JARs and documents restart-based plugin update staging. This
  motivates the agreed managed-update correction above, while retaining live
  file access and owner-approved restarts.
- [Paper's plugin guidance](https://docs.papermc.io/paper/adding-plugins/) states
  that plugins can access the machine. Vesta therefore treats content-install
  permission as code execution in the game OS identity and relies on real OS
  isolation, not just a hidden file view, to protect the control plane.
- [Restic's repository design](https://restic.readthedocs.io/en/stable/100_references.html)
  uses authenticated encrypted storage. Optional unencrypted restic repositories
  were an incorrect requirement and have been replaced by automatic protected
  key generation plus owner recovery export.
  Superseded on 2026-10-12: the owner chose a built-in unencrypted engine over
  bundled restic (see the Backups amendment).
- [Let's Encrypt's validation requirements](https://letsencrypt.org/docs/challenge-types/)
  distinguish port-80 HTTP validation, port-443 TLS validation, and DNS validation.
  Onboarding must not imply that opening an arbitrary management port enables
  automatic public certificate issuance.
- Crash-safe autosave cleanup, authority rechecking for durable workflows,
  bounded log catch-up, separated OS identities, and local-disk SQLite state are
  architectural safeguards added during this review, not claims of features
  already implemented in the MVP.
