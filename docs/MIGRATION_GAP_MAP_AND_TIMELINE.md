# Java-to-Rust migration: gap map and timeline

Status: **not complete.** The migration gate has not passed: 179 inventory rows are
`not_migrated`, no row is `verified`, and the independent whole-rebuild review by a
separately configured Sol has not run. Baseline: branch `feat/server-software-foundation`,
latest commit `2709401` (pack publication).

Sources: `docs/RUST_WRAPPER_PARITY.json`, `docs/SERVER_SOFTWARE_REQUIREMENTS.md`,
`docs/RUST_WRAPPER_MIGRATION_PLAN.md`, `docs/RUST_INTEGRATED_MIGRATION_PASS.md`, and a
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
| Pack passwords and pack-only access | `security/PasswordKdfGate`, `auth/BearerTokenStore`, `RealtimeTicketService`, `server/SessionManager` | Partial | `pack_access.rs`: Minecraft-policy grants from the server's real `server.properties`/whitelist/bans (offline mode fails closed), password grants on the active revision, hash-only bounded sessions and single-use tickets rechecked on every use; composed in `Host`. No routes or transport. |
| Operations and confirmations | `operations/` | Partial | Closed lifecycle driver exists. Confirmation coverage is unverified. |
| Process and console | `runtime/MinecraftServerProcess`, `console/ConsoleBuffer` | Partial | Owned direct-child control and console capture exist. No supervisor, no crash policy. |
| Player query | `runtime/PlayersQuery`, `PlayerListParser` | Partial | Literal `list` query exists. Runtime composition is not wired. |
| File browser | `files/FileBrowserService`, `ServerPathPolicy` | Partial | List/read plus `files/mutation.rs`: create/overwrite/delete/restore with If-Match hashes, scoped capabilities, bounded recoverable trash (10 GiB / 10 % / 7 days), disk-space preflight and audited transactions. No mkdir/rename, no resumable uploads, no operation journal. |
| Content install | `content/ContentInstaller` | Partial | Java upload/remove is covered by recoverable file mutations. Provider-sourced installs (Modrinth/CurseForge/hashed URLs) remain. |
| Pack catalogue, manifest, archive | `core/manifest/ServerModpackManifest`, `LocalArchivePack` | Partial (Replaced format) | Rust-native catalogue schema, stored ZIP builder and content-addressed publication exist. No catalogue selection, no archive-mode manifest. |
| Pack revisions and update policy | `manifest/ModpackState` | Partial | `packs/releases.rs`: database-backed monotonic releases, optional/required minimum (`pack.force_update`), `current`/`update_available`/`update_required`, display-version acknowledgement. Reference-aware artifact pruning (`prune`) exists. No status route or signing. |
| Sync and status | `core/sync` | Partial | Pure proposal types. No I/O, no status route. |
| HTTP, TCP, realtime | `server/VestaHttpServer`, `TcpSyncServer`, `VestaRealtimeSocket`, `SessionManager` | Partial | `vesta-transport`: bounded HTTPS (hyper + tokio-rustls) serving discovery, pack status, password/Minecraft pack sign-in, tickets and artifact download; management refused, legacy v1 removed; `vesta-agent serve`. No TCP sync protocol or WebSocket. |
| TLS | `security/TlsMaterial` | Partial | Configured PEM only via the protected reader, TLS 1.2+, no generation or plaintext fallback. Certificate generation/rotation and mTLS management are not implemented. |
| Rate limiting | `security/TokenBucketRateLimiter` | Partial | `admission.rs` peer limiter. Equivalence is unverified. |
| Whitelist and admission data | `security/WhitelistService` | Partial | `whitelist.rs`, `minecraft_admission.rs` (`server.properties`, `banned-players.json`), composed into pack-access policy with explicit reload. No file watching. |
| Config loading and host composition | `config/ConfigLoader`, `WrapperConfig`, `runtime/AgentRuntime` | Partial | `host.rs`: `Host::open` composes database, identity, pack passwords, Minecraft challenges, whitelist, files, pack preparation, releases and the operation runtime; no owner bootstrap, no game start, rollback on failure, observation-only shutdown. No transport; CLI still only `check-config`. |
| Discovery | `runtime/ServerFeatureDiscovery` | Partial | `Host::discover` reports composed mechanisms honestly. No transport serves it. |
| Installer | `ContentInstaller`, `AgentRuntime` | Unknown | `vesta-server-install` exists. Capability not inspected. |
| Connector and plugin protocol | `ipc/ControlIpcServer`, `src/main/java/com/vesta/loader` | Missing (host side) | No Rust IPC endpoint. Plugin and mod remain Java. |

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

0. **Done:** protected content-addressed pack publication (`2709401`); release ledger, whitelist parser,
   accounting report and checkpoint-doc archive (`dbbefab`); reference-aware artifact pruning and
   protected whitelist loading, host composition and discovery (`52773b0`); pack-access policy,
   sessions/tickets and Minecraft admission data (next commit).
1. **Accounting pass:** script-based recount of requirement links; classify the 179 rows by
   kind and boundary; record dispositions with evidence. No promotion without tests.
2. **Catalogue and revisions (remaining):** sync/status route, configuration dropping its now
   non-authoritative revision fields.
3. **Archive-mode manifest and snapshot:** pinned snapshot, refresh and serve for `.zip` and
   `.mrpack` artifacts.
4. **Protected file and content services (remaining):** mkdir/rename, resumable chunked uploads,
   content install from Modrinth/CurseForge/URLs (Java `ContentInstaller`), and optional
   operation-journal integration for file effects.
5. **Host composition (remaining):** host-run CLI entrypoint once a transport exists; player
   query and console exposure through authenticated channels.
6. **Transports (remaining):** WebSocket realtime, TCP sync and disk-streamed downloads. Done: TLS, bounded HTTP and TCP serving discovery, pack status/sync/download and
   pack-only authentication over the existing session store, with rate limits; management
   routes stay unavailable until a canonical device boundary exists. Needs new reviewed
   networking/TLS dependencies.
7. **Out of migration scope:** backups, SFTP, scheduling, NAT, metrics, Discord, MOTD (future work).
8. **Verification gate:** settled-source fmt, check, test, clippy, Node and parity checks
   on Windows; a separately configured Sol performs the whole-rebuild review; parent fixes
   findings with regressions and reruns the full suites.
9. **Repository cleanup:** move the Java plugin/mod (and the Java core/wrapper baseline) into a
   separate repository outside the server repo, then drop Gradle files and Java-era root docs here.

Product and platform gates stay separate and are not part of this timeline: Linux and
macOS execution, MSRV, OS-local bootstrap and device authentication, durable installer
activation, real Java and loader matrix, services and distribution, and release.

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
