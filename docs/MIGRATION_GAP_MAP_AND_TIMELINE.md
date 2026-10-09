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
| Pack passwords and pack-only access | `security/PasswordKdfGate`, pack-password tests | Partial | Password metadata and verification exist. Pack-only sessions, tickets and routes are missing. |
| Operations and confirmations | `operations/` | Partial | Closed lifecycle driver exists. Confirmation coverage is unverified. |
| Process and console | `runtime/MinecraftServerProcess`, `console/ConsoleBuffer` | Partial | Owned direct-child control and console capture exist. No supervisor, no crash policy. |
| Player query | `runtime/PlayersQuery`, `PlayerListParser` | Partial | Literal `list` query exists. Runtime composition is not wired. |
| File browser | `files/FileBrowserService`, `ServerPathPolicy` | Partial | Read-only observations exist. Protected mutations are not implemented. |
| Content install | `content/ContentInstaller` | Missing | No Rust managed-content install or update. |
| Pack catalogue, manifest, archive | `core/manifest/ServerModpackManifest`, `LocalArchivePack` | Partial (Replaced format) | Rust-native catalogue schema, stored ZIP builder and content-addressed publication exist. No catalogue selection, no archive-mode manifest. |
| Pack revisions and update policy | `manifest/ModpackState` | Partial | `packs/releases.rs`: database-backed monotonic releases, optional/required minimum (`pack.force_update`), `current`/`update_available`/`update_required`, display-version acknowledgement. No status route, pruning or signing. |
| Sync and status | `core/sync` | Partial | Pure proposal types. No I/O, no status route. |
| HTTP, TCP, realtime | `server/VestaHttpServer`, `TcpSyncServer`, `VestaRealtimeSocket`, `SessionManager` | Missing | No listener in any crate. |
| TLS | `security/TlsMaterial` | Missing | No socket is opened. |
| Rate limiting | `security/TokenBucketRateLimiter` | Partial | `admission.rs` peer limiter. Equivalence is unverified. |
| Whitelist | `security/WhitelistService` | Partial | `whitelist.rs`: strict bounded parser (fails closed). File loading and pack-access policy composition missing. |
| Config loading | `config/ConfigLoader`, `WrapperConfig` | Partial | Passive `check-config` only. Runtime and transport are not started. |
| Discovery | `runtime/ServerFeatureDiscovery` | Partial | Draft protocol types. No transport. |
| Installer | `ContentInstaller`, `AgentRuntime` | Unknown | `vesta-server-install` exists. Capability not inspected. |
| Connector and plugin protocol | `ipc/ControlIpcServer`, `src/main/java/com/vesta/loader` | Missing (host side) | No Rust IPC endpoint. Plugin and mod remain Java. |

## 3. Requirement families with no recorded Rust counterpart

These prefix groups appear to have no Rust counterpart. Counts are from earlier grep work
and are unverified.

- connectivity-and-nat (26)
- backups (25)
- scheduling-and-notifications (16)
- lifecycle-maintenance-and-sleep-mode (18)
- sftp (11)
- console-and-player-actions (12), partly covered by `vesta-game-runtime` without requirement links
- minecraft-status-and-motd (9)

Some of these may be out of scope. That needs an owner decision (see section 5).

## 4. Java-only candidates for "not applicable" or "retained"

These are proposals for the owner, not approved dispositions.

- Retained plugin and mod code (17 rows): loader and platform entrypoints. The owner's
  rule keeps this Java.
- Test and test-helper rows (54 not_migrated): may be replaced by Rust tests rather than
  ported one-to-one.
- Features that may be out of scope: SFTP, backups, metrics, Discord, NAT.

## 5. Decisions needed

1. Are backups, SFTP, scheduling, NAT and metrics in scope for this migration?
2. Should the Java plugin and mod be moved out now (overriding the plan's comparison
   rule), or after the migration gate?
3. Is cleaning `target/` (about 8.7 GB, regenerable) approved?

## 6. Timeline (ordered waves, not dated)

Each wave ends with focused tests, a fresh full suite and an unsigned local commit.

0. **Done:** protected content-addressed pack publication (`2709401`); release ledger, whitelist parser,
   accounting report and checkpoint-doc archive (this wave).
1. **Accounting pass:** script-based recount of requirement links; classify the 179 rows by
   kind and boundary; record dispositions with evidence. No promotion without tests.
2. **Catalogue and revisions (remaining):** reference-aware artifact pruning, sync/status route,
   configuration dropping its now non-authoritative revision fields.
3. **Archive-mode manifest and snapshot:** pinned snapshot, refresh and serve for `.zip` and
   `.mrpack` artifacts.
4. **Protected file and content services:** validated relative paths, bounded reads,
   staged replacement, integration with operation effects.
5. **Host composition:** strict configuration, runtime startup and shutdown with rollback,
   honest discovery, bounded player query.
6. **Transports and sessions:** TLS, bounded HTTP and TCP, pack-only sessions and tickets,
   rate limits, whitelist; management routes stay unavailable until a canonical device
   boundary exists.
7. **Owner-scope decisions:** backups, SFTP, scheduling, NAT, metrics, per decision above.
8. **Verification gate:** settled-source fmt, check, test, clippy, Node and parity checks
   on Windows; a separately configured Sol performs the whole-rebuild review; parent fixes
   findings with regressions and reruns the full suites.
9. **Repository cleanup:** Java plugin and mod extraction and doc consolidation, after
   wave 8 unless the owner overrides.

Product and platform gates stay separate and are not part of this timeline: Linux and
macOS execution, MSRV, OS-local bootstrap and device authentication, durable installer
activation, real Java and loader matrix, services and distribution, and release.

## 7. Proposed repository and documentation cleanup (not executed)

- **Java plugin and mod:** the source is under `src/main`, in the same Gradle build as
  `core/`, `wrapper/` and `versions/`. A split needs a decision on `core/`. `demo-server/`
  holds a real world, logs and state. It is git-ignored and must be copied as-is, never
  erased.
- **Build output:** `target/` (about 8.7 GB) is regenerable; `cargo clean` is the proposed
  action, pending approval.
- **Docs:** done. 26 per-slice checkpoints, `IMPLEMENTATION_PROGRESS.md` and
  `RUST_OPERATIONS_KERNEL_PLAN.md` moved to `docs/archive/` with every reference updated.
  Root-level Java-era docs (`RUNNING.md`, `TESTING.md`, `SERVER_SYNC_SPEC.md`) wait for the
  Java split decision.
