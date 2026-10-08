# Rust server-installer extraction — bounded migration checkpoint

Date: **2026-10-08**. Status: **integrated; scoped review, cleanup and settled Windows verification passed**.

## Approved boundary and preservation

The existing reviewed Rust installer moved into `crates/vesta-server-install`; Piston retains
only a narrow `piston-lib::game::server_install` compatibility facade and adapter tests. This
reuses Rust behavior, not Java classes, the whole launcher engine, a client-side install route,
managed-JRE acquisition, remote authority or an independent supervisor. Java wrapper/core,
tests, Gradle/Stonecutter builds and Minecraft plugin/mod remain intact.

Starting committed references:

- Server: `7ae6794be9c581e059ae69c6944cdc3377984a0a`.
- Original installer/companion: `e5bc7e00b6f88a303bf6430b4c07ca30d36e4a14`,
  `crates/piston-lib/src/game/server_install/` at that revision.
- Server fetch and `pull --ff-only origin main`: **Already up to date**. Companion origin
  refreshed; its work branch was not behind origin/main at this start. No merge changed
  protected owner work.
- Protected dirty companion `vesta-launcher/src-tauri/Cargo.toml` stayed outside edit,
  formatting, staging and checkpoint scope. Required SHA-256:
  `4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`.

## Interfaces and behavior

Exactly four external seams were separated:

1. Six-method neutral progress trait and silent reporter; Piston borrows/forwards all callbacks
   and live cancel/pause/dry-run checks. No captured control flags or notification/action API.
2. Small private Java-major parser preserving legacy `1.` and leading ASCII digits, without
   managed Java acquisition/import.
3. Explicit private/redacted owned `TransportConfig::direct()` or validated `::proxy(String)`.
   Direct mode disables ambient proxies; invalid proxies fail closed with fixed errors, never
   direct fallback. Piston snapshots its configured proxy once. No custom client/origin/policy
   UUID injection or serialization. Earlier validation is an intentional fail-before-effects
   timing change.
4. Remove only the redundant modpack ancestor-helper call. The stronger local non-following
   loop, including all Windows reparse-point refusal, remains; native path-race gaps remain.

Private plan/recipe constructors, fresh private per-installer policy UUID, compiled sources,
DNS/redirect/budgets/deadlines/retries, exact upstream pins and observed SHA-256 remain intact.
Schema-1 golden hash remains
`3a8e8d96353035b0cd4542ec4cb671c1165d19197d3bdf3d48298bb3fecbd7ed`;
schema-2 domains, receipt-last commit markers, no-clobber hard-link publication and refusal to
adopt/retry/launch partial outcomes are unchanged. Six automatic providers remain
Vanilla/Paper/Folia/Purpur/Fabric/Quilt. Forge/NeoForge and real processor execution are unavailable.

The selected-Java probe retains owned-child/output/deadline/injection-environment behavior.
Prepared execution/workspace primitives remain private staged groundwork, not enabled providers.
No activation, EULA acceptance, world/config mutation, automatic stop/restart/restore, service,
listener or administrative authority was added. Clients still signal desired changes; trusted
backend application code—not clients—supplies executable/source/transport policy.

See [the host API](RUST_SERVER_INSTALL_API.md) and
[dependency intake](DEPENDENCY_DECISIONS.md#extracted-server-installer-intake-2026-10-08).
The extracted crate keeps edition 2021 and declares Rust **1.88** for preserved sysinfo 0.38.4.
The expanded graph is not certified for the foundation's 1.85 declaration. Both MSRVs,
non-Windows targets, full provenance/licence/notice/vulnerability intake remain unverified.

## Source/test accounting and cleanup

Original inventory: **20 files / 11,222 lines**, 11 production and nine test files. All production
and every test family were copied/adapted; the core additionally has config/progress modules and
its own manifest. Parent inspected exact helper deltas. Obsolete 19 Piston implementation/test
files were removed only after core tests and the facade compiled/passed; removal targets were
verified unchanged against companion HEAD. The facade is not a second implementation.

| Former Piston relative test source | Current server test source |
| --- | --- |
| `tests.rs` | `crates/vesta-server-install/src/tests.rs` |
| `tests/installation.rs` | `crates/vesta-server-install/src/tests/installation.rs` |
| `tests/runtime.rs` | `crates/vesta-server-install/src/tests/runtime.rs` |
| `tests/transfer.rs` | `crates/vesta-server-install/src/tests/transfer.rs` |
| `tests/plans.rs` | `crates/vesta-server-install/src/tests/plans.rs` |
| `tests/providers.rs` | `crates/vesta-server-install/src/tests/providers.rs` |
| `tests/execution.rs` | `crates/vesta-server-install/src/tests/execution.rs` |
| `tests/execution_workspace.rs` | `crates/vesta-server-install/src/tests/execution_workspace.rs` |
| `tests/loaders.rs` | `crates/vesta-server-install/src/tests/loaders.rs` |

Original test-source attributes: **160**; two parameterized macros expand to **166** before
platform filtering, **164** on Windows. These source counts are not execution proof by themselves.
Three config tests and one genuinely constructed second-installer approval regression bring the
core to **168** Windows cases. Piston adds two adapter cases; its full post-relocation suite is
**210**, versus the prior **372**: `372 - 164 + 2 = 210`. Combined coverage is preserved plus six
new cases, not a claim that the old Piston count stayed unchanged. Counts include inert child
entrypoint/harness cases and do not individually certify requirements.

Exact child selectors/guards now name `tests::runtime::*` / `tests::execution::*`; assertions,
fixtures, macro cases, platform filtering and deadlines were not weakened. Fixtures are owned
native self-test-executable processes, a self-terminating pipe holder, controlled local HTTP/TCP,
inert artifact bytes, temp directories and existing owned Windows junction/native-handle cases.
No real provider/Java installer/Minecraft/Mojang requests ran. New approval regression copies
only fixture HTTP/Java/space overrides while preserving the second constructor's fresh UUID, so
even a regressed refusal cannot escape to a real provider.

Strict-lint cleanup removes Copy clones, names identical closure/observer types, uses equivalent
method comparisons/byte iterators/borrows, and groups the private URL/software pair in `Source`.
Production publication does not enumerate; test enumeration retains the same after-link fault
indices. No blanket lint/unsafe relaxation: native allowances are function-scoped; the public
outcome alone retains an enum-local large-variant exception to preserve its unboxed receipt API;
one owned child-spawn statement retains a documented zombie-process exception because waiting
inside that child would destroy the inherited-pipe fault (outer completion waiting remains).

Six existing requirement rows relocate **14** live test pointers to the core. IDs/text/phases,
statuses and historical evidence are unchanged; the checker requires real, restricted Rust test
paths instead of accepting missing old copies. Historical dated checkpoint claims remain intact
and their original sources remain available at the companion reference above. The parity JSON,
all **222** Java hashes and **370** frozen normative requirements are not promoted by extraction:
**24 migrated, 17 retained Java plugin, 181 not migrated, 0 verified**.

## Helpers and scoped review

Space Bunny's inventory attempt and later selector-check retry both failed with free-tier HTTP
429; neither completed work. The failure was disclosed, not counted as evidence. Approved Luna
helpers handled mechanical inventory/copies/test adapters and bounded lint/fixture work. Parent
owned interfaces, security-sensitive seams, manifests/locks, independent inspections, corrections,
cleanup and executed commands. One Luna lock report initially inspected the wrong historical
repository; parent corrected it and checked the actual server lock. No Anthropic/default fallback
or nested workflow was used.

Scoped independent Sol source/delta review found no production extraction regression. Its
suggested second-constructor regression was added, then parent/Sol checked that even the negative
failure path stays on local inert fixtures. Follow-up review accepted the cleanup and corrected
fixture; **no remaining finding within that bounded scope**. This is source review, not independent
execution and **not the mandatory final whole-rebuild review**, which remains `not_run`.

## Executed attempts and final evidence

Environment: native **Windows 11 Home 10.0.26100**, Rust/Cargo **1.96.0**, Node **22.14.0**, Git Bash.
Cargo uses `CARGO_INCREMENTAL=0`, offline cached dependencies and locked final runs.

- `b4vrmmox8`: core-only offline library check passed; no tests were inferred from it.
- `bz4c3mgi3`: formatting and **167 core tests** passed; strict all-target Clippy failed on
  copied-source issues. Fixed as described above; this failed chain is not a final pass.
- `bdrn23uqb`: adapter all-target check and **210 Piston tests** passed. Piston emitted its
  pre-existing unrelated unused `tokio::task` import warning; no unrelated source was changed.
- `budygm3tc`: expanded all-target check, **556 server Rust tests**, zero failed/ignored, and
  strict all-target Clippy passed. Node was **158 passed / 1 failed** because old test pointers
  became missing after duplicate cleanup; subsequent checker/ledger/diff steps were skipped.
  Pointer relocation fixes that failure without dropping evidence or upgrading statuses. A late
  fixture correction also means this attempt is not used as final settled-source evidence.
- `bd0yp011m`: fresh settled server/launcher/accounting/preservation chain **passed**, output
  inspected by parent: **556 server Rust tests** (including **168 installer cases**), **210 Piston
  tests** and **159 Node tests**, zero failed/ignored/skipped. Server format/all-target check/strict
  all-target Clippy, exact adapter formatting, Piston all-target check, parity/ledger/diff checks and
  the protected digest passed. Parent independently counted the server/Piston result boundaries
  and confirmed the new approval, both adapter, inherited-pipe and schema-1 hash witnesses executed.
  Piston's pre-existing unrelated unused-import warning remains; no strict Piston-wide Clippy or
  full Tauri/UI build is claimed.
- `bv83exa6a`: final documentation/accounting pass **passed**: **159 Node tests**, parity/ledger,
  format/diff checks, exact checkpoint scope, unchanged companion registry selections, original
  server-lock delta accounting and unchanged protected digest. No hidden platform/whole-product
  gate is inferred from those checks.

Final chain includes server format/check/workspace tests/strict all-target Clippy; exact facade
rustfmt check; Piston all-target check/full tests; all three Node suites; parity and ledger
checkers; diff checks; protected digest; and an independent comparison proving the ledger changed
only the 14 pointers, with frozen parity hashes unchanged. These are bounded native Windows
results, not unexecuted platform/MSRV/release or whole-migration gates.

## Reproducibility and remaining gates

The relative Piston dependency needs sibling checkouts named `launcher-server-mvp` and
`server-connections-mvp`, including the full server workspace metadata/lockfile. Paired unsigned
checkpoint references identify this local increment. See companion
`docs/SERVER_INSTALL_ADAPTER.md` for commands and CI layout. No absolute dependency, moving Git
reference, invented published crate or publication was introduced. Existing isolated launcher-only
CI/release checkouts need explicitly revision-pinned sibling wiring before any publication; no
remote CI run or access to local-only revisions is claimed.

Current authority/operations/confirmation/recovery/worker lifetime, artifact/Java launch composition,
activation/adoption/update/world-backup/onboarding, OS bootstrap/device/mTLS/session/vault,
protected files/packs, agent executable, independent supervisor/IPC/native containment/readiness,
platform/MSRV/release intake and final whole-rebuild review remain unfinished. This chunk does not
complete the Java-to-Rust rebuild or authorize feature/requirement/platform promotions.

No protected-manifest edit, push, PR, publication, real provider/game execution, Mojang request,
EULA acceptance, world/demo reset, service/firewall/router/public-exposure/signing-setting change,
automatic world restore or arbitrary privileged command was performed. Only verified local
unsigned checkpoints are authorized after final checks.
