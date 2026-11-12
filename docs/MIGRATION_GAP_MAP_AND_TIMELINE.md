# Java-to-Rust migration: gap map and timeline

Status: **migration gate passed for the agreed scope; no inventory row is verified.** The owner-approved gate is the complete whole-rebuild review at `a3dda06` plus delta reviews. The delta review at `1250533` found three P2s, all now fixed, and a fix-delta review of `1250533..cbaf1ab` at Sol found no new P1, P2 or P3 findings. Known limits: 179 inventory rows are `not_migrated`, several Java assertion families have no line-by-line Rust equivalent recorded, and 41 rows have no counterpart pointer. Passing the gate does not mean every Java assertion is translated or that any row is verified.

Sources: `docs/RUST_WRAPPER_PARITY.json`, `docs/SERVER_SOFTWARE_REQUIREMENTS.md`,
`docs/archive/RUST_WRAPPER_MIGRATION_PLAN.md`, `docs/archive/RUST_INTEGRATED_MIGRATION_PASS.md`, and a
read-only review of the Rust crates and Java sources. Rust status below comes from module
docs and code search, not from re-running tests. Treat it as a map to verify, not a
verdict.

## 1. Inventory counts

Inventory: 222 Java entries. 26 `migrated`, 17 `retained_java_plugin`, 179 `not_migrated`.

| Kind | host boundary | shared host/plugin | plugin boundary |
|---|---|---|---|
| production | 75 | 50 | 0 |
| test | 32 | 17 | 0 |
| test_helper | 4 | 1 | 0 |

The 179 `not_migrated` rows are 125 production, 49 test, and 5 test helper. The 17
`plugin_boundary` rows are all `retained_java_plugin`.

**Requirement links:** `node scripts/migration-accounting-report.mjs` reports 19 of 370 frozen
requirements referenced by any inventory row and 351 unreferenced (0 unknown references).
Unreferenced does not mean unimplemented; most rows have not been requirement-mapped yet.

## 2. Capability gaps

Rust status: Implemented, Partial, Missing, Replaced (deliberate safer or different
design), Plugin-side (retained Java), or Unknown.

| Area | Java evidence | Rust status | Gap |
|---|---|---|---|
| Identity: Minecraft proof and canonical policy | `identity/`, `auth/AuthService` | Partial | Device enrollment and pairing are missing. Production local authority is unavailable. |
| Pack passwords and pack-only access | `security/PasswordKdfGate`, `auth/BearerTokenStore`, `RealtimeTicketService`, `server/SessionManager` | Partial | `pack_access.rs`: Minecraft-policy grants from the server's real `server.properties`/whitelist/bans (offline mode fails closed), password grants on the active revision, hash-only bounded sessions and single-use tickets rechecked on every use; composed in `Host`. Pack-only HTTPS routes exist in `crates/vesta-transport/src/routes.rs` (`route` at line 47). |
| Operations and confirmations | `operations/` | Partial | Closed lifecycle driver exists. Confirmation coverage is unverified. |
| Process and console | `runtime/MinecraftServerProcess`, `console/ConsoleBuffer` | Partial | Owned direct-child control and console capture exist. No supervisor, no crash policy. |
| Player query | `runtime/PlayersQuery`, `PlayerListParser` | Partial | Literal `list` query is composed through the operation driver (`QueryDriver`). An authenticated management transport for player queries is missing. |
| File browser | `files/FileBrowserService`, `ServerPathPolicy` | Partial | List/read plus `files/mutation.rs`: create/overwrite/delete/restore, scoped capabilities, bounded recoverable trash (10 GiB / 10 % / 7 days), disk-space preflight and audited transactions. One-level mkdir and file rename with If-Match hashes, and resumable uploads (bounded on-disk staging in protected state, reservations, fresh authority checks), are implemented as library APIs only; no CLI or network entry point (management surface refused until the device boundary exists). Expired uploads are purged at host startup and during upload creation (`crates/vesta-control/src/host.rs:273`, `crates/vesta-control/src/files/upload.rs:137`). Durable file-intent/outcome journaling is implemented (`crates/vesta-control/src/files/journal.rs`, schema v11); unbounded intent retention remains a capacity limitation being fixed. Non-ASCII file/resource names remain unsupported although Java accepted them. |
| Content install | `content/ContentInstaller` | Partial | Library APIs only; no CLI or network entry point. Recoverable file mutations cover upload/remove; provider-sourced Modrinth/CurseForge installs resolve trusted metadata and verify bytes before writing. Uploads and installs are capped at 16 MiB (`MAX_WRITE_BYTES`, `files/mutation.rs:35`). Broader arbitrary URL installation is not implied. |
| Pack catalogue, manifest, archive | `core/manifest/ServerModpackManifest`, `LocalArchivePack` | Partial (Replaced format) | Rust-native catalogue schema, stored ZIP builder and content-addressed publication exist. Implemented: strict backend `[pack.source]` selects provider/project/version reference, confined local archive, or scanned content through explicit `Host::prepare_pack` / authorized `PackReleases::publish_prepared`; configured archive preparation pins bytes through the protected reader and protected publication/authenticated artifact serving are implemented. Selected directories share one root-relative namespace, skip/report absent selections, reject overlaps/case aliases, and retain exclusions, unresolved dispositions and global budgets. No live auto-scan, provider fetching or archive extraction. Exact legacy archive-manifest parity remains an open question; whole-family independent parity review remains pending. `vesta-agent serve` has no trusted pack-publication entry point. |
| Pack revisions and update policy | `manifest/ModpackState` | Partial | `packs/releases.rs`: database-backed monotonic releases, optional/required minimum (`pack.force_update`), `current`/`update_available`/`update_required`, display-version acknowledgement. Reference-aware artifact pruning and its quarantine/recovery path are implemented (`crates/vesta-control/src/packs/quarantine.rs`). No signing. |
| Sync and status | `core/sync` | Partial | Pure proposal types. No I/O in the core. `GET /api/v2/pack/status` exists (`crates/vesta-transport/src/routes.rs:211`). |
| HTTP, TCP, realtime | `server/VestaHttpServer`, `TcpSyncServer`, `VestaRealtimeSocket`, `SessionManager` | Partial | `vesta-transport`: bounded HTTPS (hyper + tokio-rustls) serves `POST /api/v2/discovery`, pack status, password/Minecraft pack sign-in, tickets and disk-streamed artifact downloads; authenticated pack realtime uses WebSocket. Management remains refused and legacy v1 removed; `vesta-agent serve`. TCP sync is superseded/forbidden by the disposition below. Realtime heartbeat rereads the database-hosted release and compares revision and minimum revision, detecting cross-process release changes within one heartbeat; the test commits a database row directly, not via a second process. Download throughput floor and global/per-session/per-peer caps are implemented (`crates/vesta-transport/src/lib.rs`, `routes.rs`). `vesta-agent serve` still has no realtime publish entry point. |
| TLS | `security/TlsMaterial` | Partial | Configured PEM via the protected reader, TLS 1.2+, rotation that retains the last good certificate on invalid replacement, and SHA-256 fingerprint response header. No generation or plaintext fallback; mTLS management is not implemented. |
| Rate limiting | `security/TokenBucketRateLimiter` | Partial | `admission.rs` peer limiter. Equivalence is unverified. |
| Whitelist and admission data | `security/WhitelistService` | Partial | `whitelist.rs`, `minecraft_admission.rs` (`server.properties`, `banned-players.json`), evaluated by `pack_access.rs` and composed/reloaded by `host.rs`; offline-player association and authenticated management remain open. No file watching. |
| Config loading and host composition | `config/ConfigLoader`, `WrapperConfig`, `runtime/AgentRuntime` | Partial | `host.rs`: `Host::open` composes database, identity, pack passwords, Minecraft challenges, whitelist, files, pack preparation, releases and the operation runtime; no owner bootstrap, no game start, rollback on failure, observation-only shutdown. CLI entrypoint exists (`vesta-agent serve`, `check-config`; `apps/vesta-agent/src/main.rs`). |
| Discovery | `runtime/ServerFeatureDiscovery` | Partial | `Host::discover` reports composed mechanisms honestly. Served by `POST /api/v2/discovery` (`crates/vesta-transport/src/routes.rs:185`). |
| Installer | `ContentInstaller`, `AgentRuntime` | Partial | `crates/vesta-server-install` implements an installer library for transfer, staging, bundle assembly and runtime probing; product activation/adoption remain a separate gate and are not claimed as complete. |
| Connector and plugin protocol | `ipc/ControlIpcServer`, `src/main/java/com/vesta/loader` | Replaced / forbidden legacy authority | Control IPC has no pack/status messages; do not add a Rust endpoint (see Control IPC disposition below). Retained plugin/mod remain Java; they still need a v2 HTTPS client and detection update in the plugin repository. |

## 3. Requirement families with no recorded Rust counterpart

These requirement families have no Rust counterpart. Counts are unreferenced requirement IDs from
`scripts/migration-accounting-report.mjs`.

- connectivity-and-nat (26): not in Java
- backups (25): not in Java (only a capability name and pre-migration DB backups)
- scheduling-and-notifications (16): not in Java (capability names only)
- lifecycle-maintenance-and-sleep-mode (18): Java only queried players around sleep; no maintenance mode
- sftp (11): not in Java
- console-and-player-actions (12), partly covered by `vesta-game-runtime` without requirement links
- minecraft-status-and-motd (9): not in Java

Owner decision (2026-10-09): only behavior the Java implementation had is migration scope.
Families Java never implemented (SFTP, NAT, Discord, MOTD, backups, scheduling, metrics) are
future product work, not migration gaps; their frozen requirements stay unchanged.

## 4. Java-only candidates for "not applicable" or "retained"

These are proposals for the owner, not approved dispositions.

- Retained plugin and mod code (17 rows): loader and platform entrypoints. The owner's
  rule keeps this Java.
- Test and test-helper rows (54 not_migrated): may be replaced by Rust tests rather than
  ported one-to-one.
- Features Java never implemented (SFTP, backups, metrics, Discord, NAT, MOTD, scheduling):
  out of migration scope per the owner decision above.

## 5. Owner decisions (2026-10-09)

1. Migration scope is what Java implemented; other requirement families are future product work.
2. The Java plugin and mod will be moved to a separate repository; timing is not critical. It is
   scheduled after the verification gate, because the parity checker hashes Java files in place
   and the Java code is the comparison baseline until then.
3. `target/` is kept: cleaning it only forces a full rebuild.

## 6. Timeline (ordered waves, not dated)

Each wave ends with focused tests, a fresh full suite and an unsigned local commit.

0. **Historical checkpoint evidence (not a current-HEAD gap list):** protected content-addressed pack publication (`2709401`); release ledger, whitelist parser,
   accounting report and checkpoint-doc archive (`dbbefab`); reference-aware artifact pruning and
   protected whitelist loading, host composition and discovery (`52773b0`); pack-access policy,
   sessions/tickets and Minecraft admission data (recorded as the next commit at that checkpoint).
   At current HEAD these capabilities exist, including implemented prune quarantine/recovery (`crates/vesta-control/src/packs/quarantine.rs`). The separate file-intent retention limitation remains under fix.
1. **Accounting pass:** script-based recount of requirement links; classify the 179 rows by
   kind and boundary; record dispositions with evidence. No promotion without tests.
2. **Catalogue and revisions:** configured whole-pack acquisition mode selection and selected-directory
   scanning within one root namespace are **implemented** (2026-10-10). `Host::prepare_pack` is explicit;
   `PackReleases::publish_prepared` preserves authorization and ledger revision checks for every mode.
   Platform-reference releases contain native catalogue JSON, not fetched provider archives. A forward-only
   state schema v10 migration preserves existing releases while admitting the `catalogue` artifact format.
   Remaining: configuration still needs to drop its non-authoritative revision fields; sync/status and
   release-aware pruning are implemented.
3. **Archive mode:** configured pinned archive preparation, protected publication and authenticated artifact serving are implemented. Exact legacy archive-manifest parity remains an open question; archive refresh semantics are not claimed.
4. **Protected file and content services:** Implemented as library APIs only; review history is
   recorded below, and migration status remains not passed. This includes mkdir/rename, resumable
   chunked uploads staged on disk in protected state, provider-sourced Modrinth/CurseForge content
   installation, and durable file-intent/outcome journaling (`crates/vesta-control/src/files/journal.rs`,
   schema v11). No CLI or network entry point (management surface refused until the device boundary
   exists). Expired uploads are purged at host startup and during upload creation. Uploads and installs
   are capped at 16 MiB (`MAX_WRITE_BYTES`). Unbounded intent retention remains a capacity limitation
   being fixed.
5. **Host composition (remaining):** the CLI entrypoint exists (`vesta-agent serve`, `check-config`);
   `serve` has no realtime publish entry point. Player-query runtime composition is wired through
   `QueryDriver`; an authenticated management transport for player queries and console exposure
   through authenticated channels remain.
6. **Transports:** Implemented, verified pending final review: pack WebSocket realtime, disk-streamed
   artifact downloads, TLS rotation and the SHA-256 fingerprint header, bounded HTTPS discovery,
   pack status and pack-only authentication over the existing session store, with rate limits.
   TCP sync is superseded/forbidden by the disposition below, not implemented. Management routes
   stay unavailable until a canonical device boundary exists. Realtime heartbeat rereads the hosted
   database release and compares revision/minimum revision for cross-process `pack_update` detection;
   its test commits a database row directly, not through a second process. `vesta-agent serve` still
   has no realtime publish entry point. Download throughput floor and global/per-session/per-peer
   caps are implemented.
7. **Out of migration scope:** backups, SFTP, scheduling, NAT, metrics, Discord, MOTD (future work).
8. **Verification gate:** settled-source fmt, check, test, clippy, Node and parity checks
   on Windows. Review history: the complete whole-rebuild Sol review at `a3dda06` did not pass
   (3 P2 findings plus accounting); the owner-approved gate requires that review plus delta
   reviews. Delta review `1250533` confirmed those 3 P2s resolved and found a new P2 (unbounded
   file-intent retention) plus accounting; a fix-delta review is pending. Migration status has
   not passed.
9. **Repository cleanup:** move the Java plugin/mod (and the Java core/wrapper baseline) into a
   separate repository outside the server repo, then drop Gradle files and Java-era root docs here.
10. **Remaining plugin-repository gap:** update the retained plugin's server detection and add a v2
    HTTPS client; the current embedded legacy v1 HTTP behavior is not Rust compatibility.

Product and platform gates stay separate and are not part of this timeline: Linux and
macOS execution, MSRV, OS-local bootstrap and device authentication, durable installer
activation, real Java and loader matrix, services and distribution, and release.

## TCP sync disposition (2026-10-10)

**Decision (b): superseded by pack-only HTTPS + WebSocket; do not add a Rust TCP
sync listener.** Static searches of the retained mod/plugin (`src/main/java`, not
separate `plugin/` or `mod/` directories), `core/`, and the read-only launcher at
`V:/launcher-server-mvp/vesta-launcher` found no production client of the Java pack
TCP protocol. This disposition supersedes the TCP-sync gap in section 2 and the
TCP-sync work item in wave 6; it does not certify the other transport work.

Evidence:
- `src/main/java/com/vesta/loader/VestaSyncManager.java:39-64` selects loopback
  control IPC or standalone HTTP; `ipc/ControlIpcClient.java:67-90` uses
  `ControlCodec`, **not** `TcpMessageCodec`. `server/ModHttpServer.java:103-107`
  registers HTTP info/manifest/sync/file routes. Control IPC remains a separate gap.
- `core/src/main/java/com/vesta/sync/protocol/TcpMessageCodec.java:21-31` defines
  the message registry, not a client. Searches for protocol imports, message
  construction and socket use found wrapper server handlers and tests, including
  `wrapper/src/test/java/com/vesta/wrapper/server/TcpPushUpdateAvailableTest.java:79-92`,
  rather than a retained mod/plugin consumer.
- Launcher paths below are relative to the external launcher root:
  `src/utils/modpacks.ts:152-165` invokes URL metadata retrieval;
  `src-tauri/src/commands/modpacks.rs:1843-1865` downloads over HTTP, while
  `:2302-2306` clears Vesta linkage without canonical server identity.
  `src-tauri/src/commands/sync.rs:24-49,75-81` polls Modrinth/CurseForge versions.
  Searches of launcher-owned frontend/backend source found neither TCP-sync framing
  or messages nor server-pack WebSocket consumption. Launcher integration with v2
  remains future client work; its external `piston-lib` dependency was not inspected.

Replacement mapping (semantic replacement, **not** legacy wire compatibility):

| Java message | Rust replacement |
|---|---|
| `Handshake` | HTTPS `/api/v2/discovery`, Minecraft challenge/complete (normal identity flow) or alternative pack-password sign-in, then purpose-bound tickets; v2 API boundary instead of TCP version negotiation. |
| `HandshakeResponse` | HTTPS discovery/auth responses and WebSocket `hello`; pack sessions grant no management authority. |
| `Heartbeat` | WebSocket heartbeat plus Ping/Pong, bounded idle/session expiry (`crates/vesta-transport/src/realtime.rs:349-381`). |
| `SyncCheck` | `GET /api/v2/pack/status?installed=N`, using monotonic installed revision rather than Java installed-hash/version DTOs. |
| `SyncCheckResponse` | `PackReleases` revision/minimum/status response (`crates/vesta-transport/src/routes.rs:211`); catalogue/artifact-based installation replaces server-generated per-file action lists. |
| `FileRequest` | Authenticated `GET /api/v2/pack/artifact`, optionally with a single-use download ticket; no arbitrary file path. |
| `FileResponse` | Pinned artifact stream with size/SHA-256 verification and digest ETag (`routes.rs:312` artifact handler, ETag at `routes.rs:349-353`), not a legacy per-file URL response. |
| `PushUpdateAvailable` | Release-watch WebSocket `pack_update` (`realtime.rs:335` `connected`, send at `:364`), authorized by a separate realtime ticket. Heartbeat rereads the database-hosted release and compares revision/minimum revision, detecting cross-process changes within one heartbeat. The test commits a database row directly, not via a second process; `vesta-agent serve` has no realtime publish entry point. |
| `SyncComplete` | Client persists its installed revision, then rechecks status/reconnects with that revision; no server acknowledgement endpoint. Java registered the DTO but no handler (`wrapper/src/main/java/com/vesta/wrapper/server/MessageRouter.java:40-44,85-88`). |
| `Disconnect` | WebSocket Close / HTTPS connection termination; no TCP session to remove (`realtime.rs:385`, close frame at `:402`). |

Framing remains a reusable foundation: Java's four-byte big-endian UTF-8 body
(`core/.../protocol/TcpMessageCodec.java:42-80`) is covered by
`crates/vesta-protocol/src/framing.rs:128-178`, deliberately rejecting empty,
truncated, oversized or invalid-UTF-8 frames. No listener or JSON message registry
is implied. This documentation-only decision ran no Rust tests or clippy (0 tests);
no dependencies, launcher files or transport code changed. External/uninspected
clients could require revisiting the decision.

## Control IPC disposition (2026-10-10)

**Decision: stop after the inventory; do not implement a Rust ControlCodec message
registry, local IPC listener, IPC secret, or agent configuration switch.** The useful
pack/status subset is empty. The retained plugin does not receive pack metadata,
revisions, update status, or push notifications through this channel. Existing Rust
pack-only HTTPS and WebSocket cover those product functions; the old IPC instead
supports connector detection and legacy permission/link-code authority. This
supersedes the host-side IPC gap in section 2, not the retained plugin's compatibility
work or any other migration gate.

### Complete message inventory

The six message constructors are defined in
`core/src/main/java/com/vesta/sync/ipc/ControlCodec.java:37-82`. Framing is four-byte
big-endian length plus UTF-8 Gson JSON, with the Java reader admitting only
`1..=1048576` body bytes (`:20-34`). It is not the separate TCP pack-sync codec.
Classes below: **(a)** useful pack/status, **(b)** management/authority (forbidden),
**(c)** obsolete connector plumbing. **There are no class (a) messages.**

| Message / fields | Retained plugin behavior and evidence | Classification / disposition |
|---|---|---|
| `heartbeat {ts}` | Sends periodically from `src/main/java/com/vesta/loader/ipc/ControlIpcClient.java:76-87`; no pack payload. Wrapper acknowledges it (`wrapper/src/main/java/com/vesta/wrapper/ipc/ControlIpcServer.java:129-135`) and tracks connector presence (`:48-51`). | **(c)** Legacy connector liveness, not pack/status; omit. |
| `heartbeat_ack {ts}` | Only receive handling is an ignored acknowledgement (`ControlIpcClient.java:101-103`); it updates no plugin state. Constructor: `ControlCodec.java:44-48`. | **(c)** Companion to obsolete heartbeat; omit. |
| `permission_check {uuid, permission, request_id?}` | Parses UUID/node, invokes permission callback, and echoes `request_id` (`ControlIpcClient.java:92-100`). Callback special-cases `vesta.ping`, otherwise uses `PermissionBridge.check` (`VestaSyncManager.java:71-78,85-92`); bridge delegates to a platform permission backend (`PermissionBridge.java:6-8,20-25`). Wrapper initiates correlated checks (`ControlIpcServer.java:54-75`). | **(b)** Arbitrary permission/authority bridge, not pack status; refuse by omission. |
| `permission_result {allowed, reason?, request_id?}` | Builds `allowed` response with no `reason` and echoes request ID (`ControlIpcClient.java:95-100`); plugin does not consume results. Wrapper completes the permission future (`ControlIpcServer.java:145-153`). `core/src/main/java/com/vesta/sync/auth/CapabilityEngine.java:145-149` raises effective access level on ALLOW. | **(b)** Legacy authority elevation; refuse by omission, even if some nodes historically enabled pack access. |
| `link_code {code, uuid}` | Sends the in-game code and UUID (`ControlIpcClient.java:52-60`; `VestaSyncManager.java:110-118`). Wrapper trusts it via `LinkCodeService.noteExternal` (`ControlIpcServer.java:137-143`); `/api/v1/auth/link` redeems a linked bearer (`docs/JAVA_NETWORK_SURFACE.md:38,62-64`). Linked UUIDs inherit configured people, OP and permission-node levels (`CapabilityEngine.java:135-149`). | **(b)** Connector-possession identity/authority shortcut; do not reproduce, including as a nominally pack-only compatibility feature. Minecraft proof remains the normal pack identity flow. |
| `error {message}` | Constructor exists (`ControlCodec.java:77-82`), but the plugin receive loop has no error branch (`ControlIpcClient.java:90-103`) and wrapper dispatch has no error handler (`ControlIpcServer.java:131-156`). | **(c)** Unused message helper; omit. |

Power and console are **not** ControlCodec message types. They are legacy HTTP and
WebSocket management surfaces listed in `docs/JAVA_NETWORK_SURFACE.md:32-34,55-58`;
permission/link-code authority can indirectly authorize them. Bukkit explicitly
answers `vesta.op` from OP status and allows `vesta.*` nodes for OPs
(`src/main/java/com/vesta/loader/platform/BukkitEntrypoint.java:47-59`; Paper has
the same OP shortcut at `platform/PaperEntrypoint.java:49-65`). Elevation depends
on configured grants; a permission ALLOW or linked bearer does **not automatically**
mean level 4. The problem is trusting the unauthenticated connector to assert a
UUID or permission result which can acquire those grants. No power, console,
OP-level, UUID-assertion, loopback, or connector-possession grant is introduced.

### Authentication and compatibility boundary

Java has **no IPC authentication handshake, token, shared secret file, or named
pipe**. It binds TCP to `127.0.0.1`, replaces the active peer on a new connection,
and immediately accepts frames (`ControlIpcServer.java:91-108,120-156`); the client
connects directly and starts heartbeats (`ControlIpcClient.java:67-90`). Loopback
location is not authenticated identity. There is therefore no legacy secret to
reproduce or retain hash-only, and adding one would not create a pack-data need.

The retained plugin's mode selection is **not pack status**. It probes either the
control port or the default HTTP port using a plain socket connection
(`VestaSyncManager.java:141-151`), then selects control-only wrapper mode or starts
its own embedded HTTP server (`:39-64`). A subsequent IPC connection stops the
embedded server and sets a sticky `wrapperMode` (`:99-108,131-133`). Neither TCP
connection success nor this local flag verifies Rust API compatibility.

In particular, “standalone HTTP fallback” is an **embedded plaintext legacy v1
server**, not a client of Rust HTTPS (`src/main/java/com/vesta/loader/server/ModHttpServer.java:94-107`;
`core/src/main/java/com/vesta/sync/VestaSync.java:6-11`). Leaving IPC absent does not adapt that server or prevent an
HTTP-port probe from entering wrapper mode with an endlessly retrying control
client (`ControlIpcClient.java:105-115`). Retained-plugin mode detection and v2
integration need separate client work; do not add an empty IPC endpoint just to
satisfy its probe or suppress retries. This disposition does not claim the retained
plugin works unchanged under the Rust agent.

Rust replacements already present (semantic, not Java-wire compatibility):

- `crates/vesta-transport/src/routes.rs:47,185,211,312`: route dispatch, HTTPS
  discovery, published pack revision/minimum/status; pack-auth and artifact endpoints.
- `crates/vesta-transport/src/realtime.rs:335-383`: subscribe-before-snapshot
  release watch (`connected`), WebSocket `hello` and `pack_update` (send at `:364`).
  Heartbeat rereads the database-hosted release and compares revision/minimum revision,
  detecting cross-process changes within one heartbeat (the test commits a DB row directly,
  not via a second process). `vesta-agent serve` has no realtime publish entry point. The
  plugin has no corresponding ControlCodec receive handler to preserve.
- `routes.rs:37`: `fixed_refusal`; management unavailable, all legacy v1 endpoints removed.

Java test evidence: `core/src/test/java/com/vesta/sync/ipc/ControlCodecTest.java:15-24`
round-trips a permission request and checks its type/node, not golden bytes or
partial-read semantics. `wrapper/src/test/java/com/vesta/wrapper/ipc/ControlIpcContractTest.java:47-82`
connects an ordinary unauthenticated socket, sends heartbeats and answers a correlated
permission request to obtain ALLOW. It does not establish authenticated pack/status
IPC functionality.

The reusable framing module stays unchanged. No new ControlCodec golden fixtures,
IPC malformed-message/refusal tests, release-push tests, connection-cap tests or
shutdown tests are added because there is no new codec registry/listener to test.
Validation on Windows for this documentation-only disposition:

- `cargo fmt --all -- --check`: passed; no Rust files formatted or edited.
- `cargo clippy --workspace --all-targets --offline --locked -- -D warnings`: passed.
- `cargo test -p vesta-protocol -p vesta-transport -p vesta-agent --offline --locked -- --test-threads=1`:
  **139 passed, 0 failed** (protocol 83; transport 49, pending further changes; agent 7; 0 doctests).
- Both clippy and test used `CARGO_TARGET_DIR=target/sol-transport` and
  `CARGO_INCREMENTAL=0`. These are existing-code regression checks, not certification
  of an IPC implementation; **0 new tests**. Concurrent content/files-agent changes
  were present and left untouched. Java sources/tests are reference evidence only,
  not executed in this disposition. No dependencies, commits, stash or clean.

### Implemented whole-pack configuration and selected-directory scans

The optional schema-1 `[pack.source]` table is closed and selects exactly one mode. Existing documents
without it retain the legacy `scan-roots` preparation APIs; the new whole-pack preparation API refuses
ambiguous multi-root legacy configurations. Configured local source roots also participate in native
state-overlap validation. Opening the Host observes roots but never scans or pins archive contents.

```toml
[pack.source]
mode = "scanned"
root = "game"
directories = ["mods", "config/client", "resourcepacks"]
```

Omitting `directories` scans the permitted root. A supplied list must be nonempty and bounded;
`RelativePath` and catalogue spelling policies reject unsafe names, duplicates, ancestor/descendant
overlaps and directory-case aliases. All names remain root-relative. Missing selections are skipped and
reported locally by `missing_directories()`; empty, protected and hidden selections are not mislabeled
as missing. Links, non-directory selections and other observation failures refuse. File counts, scan
work and observed-byte budgets remain global, including unresolved private files.

Alternative tables (use one, not several):

```toml
[pack.source]
mode = "archive"
root = "game"
path = "packs/release.mrpack"
```

```toml
[pack.source]
mode = "platform"
provider = "modrinth" # or "curseforge"
project-id = "project-id"
version-id = "version-id"
```

Archive paths are confined relative ZIP/MRPACK paths pinned through the same protected reader as other
pack bytes; no archive is extracted or parsed. Platform fields are IDs only: no URL field or URL-like ID
is accepted and no provider is contacted. Native platform-reference artifacts use `.catalogue` and
`application/vnd.vesta.catalogue+json`; public exposure still passes the existing pack access/download
boundary. These are trusted backend library APIs, not a new CLI/remote management publication route.
Filesystem checks remain point-in-time same-account hygiene rather than race-free tree confinement.

## 7. Repository and documentation cleanup

- **Java plugin and mod:** the source is under `src/main`, in the same Gradle build as
  `core/`, `wrapper/` and `versions/`. A split needs a decision on `core/`. `demo-server/`
  holds a real world, logs and state. It is git-ignored and must be copied as-is, never
  erased.
- **Build output:** `target/` is kept (owner decision).
- **Docs:** done. 26 per-slice checkpoints, `IMPLEMENTATION_PROGRESS.md` and
  `RUST_OPERATIONS_KERNEL_PLAN.md` moved to `docs/archive/` with every reference updated.
  Root-level Java-era docs (`RUNNING.md`, `TESTING.md`, `SERVER_SYNC_SPEC.md`) wait for the
  Java split decision.
