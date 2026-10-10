# Rust owned direct-child process increment

Date: **2026-10-08**, native **Windows 11 / Rust and Cargo 1.96.0**.
Status: **ordinary-library direct-child component integrated; scoped source follow-ups and fresh
workspace verification complete**. This is not final whole-rebuild review or migration completion.

This is the approved [runtime migration slice](RUST_WRAPPER_MIGRATION_PLAN.md), over the
previous [console component](RUST_CONSOLE_CAPTURE_CHECKPOINT.md). The useful ordinary-library
boundary is `vesta_game_runtime::process`, described by [its API contract](../operations/game-process-api.md).
It is not a remote route, authenticated management worker, independent supervisor or Java API port.
Java comparison code/builds remain. All 222 inventory hashes and 370 requirements/status/history
are preserved; this checkpoint does not promote a parity row or a whole requirement.

## Implemented boundary

- Privately owned literal executable/argv/cwd definitions, explicit graceful-stop support,
  portable UTF-8 byte/count bounds, controls/Unicode line-separator rejection, metadata
  preflight again at start and conservative Windows native quoting bounds. Arguments remain
  individual `Command::args` entries; no implicit shell/interpolation/environment rewriting.
  `.bat`/`.cmd` refuse. Empty arguments remain valid literal values. Debug/errors never print
  executable paths, argv or console-command values.
- Opaque random 128-bit generation lookup IDs, not PIDs, local counters or administration.
  Every command/stop/kill/wait binds the exact retained generation. Existing live/stopping/
  pipe-draining ownership refuses replacement. `AlreadyOwned` is an inert refusal rather
  than Java's convenience already-running no-op; there is no durable restart API.
- Three owned I/O threads, prepared before process spawn. Stdout/stderr drain through the
  bounded console regardless of viewers. Storage failure marks output failure and continues
  drain/discard; native read failure remains failure. Stdin admission is a nonblocking queue
  bounded to eight lines plus the writer's current line, each at most 8,192 bytes plus newline.
  Admission is not proof of delivery or command effect.
- Graceful stop admits only fixed `stop`, only when supported. Refusal is inert; no invented
  command, signal or script. Timeout leaves actual live ownership and Stopping, with no implicit
  kill/restart. Explicit force kill targets the retained direct-child handle, never a supplied PID.
  Failed kill retains Stopping but does not falsely mark a graceful command as admitted.
- Final exit summaries require actual OS exit **and every I/O worker returned/joined**. Actual
  zero/nonzero exit codes and I/O faults are distinct. Exit without closed inherited pipes remains
  Draining; restart is refused until genuine drainage/joins. Retained last-exit facts are historical
  during a later running generation; they do not describe that generation as exited.
- Post-spawn attachment failure is `StartupUncertain`, never a clean no-effect spawn failure.
  The exact child/generation, workers and undelivered handles remain owned. A failed send restores
  its handle into the child; unattempted handles never leave it. Incomplete output is flagged,
  even if recovery later reaches a zero exit. Required trusted durable-effect recovery remains
  separate work; this is not a persisted operation receipt.

Mutable controller ownership serializes effects. Zero-duration stop/wait plus polling is available
for a responsive trusted host loop; the component is not a concurrent application actor. Drop is
**unbounded synchronous last-resort direct-child kill/reap/join**, including repeated native wait
failure. It is not graceful application shutdown, confirmed management kill, hard cancellation,
agent-crash survival or a promise that descendants are terminated. Normal application composition
must retain the owner after timeouts and inspect explicit generation-bound outcomes.

`getrandom = 0.4.3` is the only runtime dependency and `tempfile = 3.27.0` is test-only. Both were
already pinned/cached in the workspace. Cargo.lock adds only those edges to the local runtime
package: no new external package or repin. An explicitly named inert fixture executable under
`tests/fixtures/game.rs` is a test target, not a stub application/supervisor. Private fault hooks
remain cfg(test); there is no public test-support/authority injection feature.

## Test and review scope

Space Bunny supplied read-only Java behavior/test-family accounting and an unexecuted public
integration draft. Parent inspected and replaced the flawed oversized draft with compact public
tests/common helpers, implemented production ownership and independent adversarial/fault/native
ownership witnesses, and owns every executed command. Source-only helper output is not a pass.

The selected test family covers:

- Individual argument/count/UTF-8 budgets, exact total UTF-8/native-quoted bounds and one over;
  controls/Unicode separators, absolute paths, script refusal, real preflight recheck and native
  spawn failure with no published generation. Metadata preflight is not path confinement.
- Byte-for-byte native argv oracle with empty/quoted/trailing-backslash/Unicode/shell-metacharacter
  and `-jar`/`@argfile` tokens, correct native cwd and a no-shell-side-effect marker.
  These tokens are delivered literally to the inert executable, **not real Java shape execution**.
- Real graceful exit 0, crash exit 23, fixed-command delivery, idempotent repeated pending stop,
  slow-stop explicit release, ignored-stop timeout plus live command-response and exactly one start,
  unsupported stop refusal and separately requested direct-child kill.
- Same-owner exit/replacement then stale command/stop/kill/wait refusals, with replacement's
  positive command delivery. Excessive timeout refusal and sensitive diagnostics.
- Actual stdout **and stderr** beyond pipe capacity (4 MiB each) before a later stop, with no
  snapshots/viewer during the bulk-output interval (startup handshakes occur beforehand).
  An actual 1 MiB undelimited line truncates to 16,384 bytes and the following line is captured.
- Native nonreading stdin, bounded queue refusal, pending writes returning/faulting and finalizing
  after explicit kill. This is not yet a deterministic handshake inside a blocked native write,
  nor a living child that closes its stdin handle. The `closed-stdin` fixture mode never reads;
  it does not close stdin and its name is not evidence of disconnection.
- Controlled kill refusal on a live inert unit-test subprocess, retained ownership then exact
  graceful `stop` delivery and real exit 0. This injects the native-error result; it does not
  induce an actual Windows TerminateProcess failure.
- Failed **second** pipe dispatch after successful stdin transfer, retained returned stdout and
  unattempted stderr, StartupUncertain/current generation/replacement refusal, graceful recovery
  and truthful output-failure summary. Missing first pipe retains both unattempted outputs.
- Poisoned console with actual 2 MiB per stream plus a later fixture-recorded stop: discard-drain
  is required for child progress. A separate reader-position witness reaches real EOF after a
  first-line storage fault, beyond the first read buffer.
- Deterministic RNG error/zero/recent-collision bounds, and second/third prepared-thread refusal
  with actual already-created worker return counts before the preparation call returns.
  These are private seam tests, not induced native RNG/thread-allocation failures.
- Observation-error injection against an actual live child: exact generation, no observed exit
  and all three retained worker handles are asserted before/after refusal, followed by replacement
  refusal and real graceful recovery. The public observation path still calls native `try_wait` only.
- A positively entered stdin-worker gate prevents consumption while exactly eight lines fill the
  queue. Failed graceful-stop admission leaves both admission/phase flags unchanged. After explicit
  release, all eight lines are positively observed before retry; exactly one final `stop` and actual
  exit 0 demonstrate recovery. This does not handshake inside a blocked native pipe write.
- Actual direct-child exit with a test descendant holding inherited stdout/stderr: Draining and
  restart refusal persist until explicit descendant release/worker return. The fixture descendant
  has a ten-second self-termination fallback and a release-before-owner-Drop unwind guard.
  This proves retention, **not process-tree termination/containment**.
- Windows-only panic unwind: a live child positively holds a share-denied native file lease;
  after owner destruction the same lease can actually be reopened while cwd remains alive.
  This demonstrates actual direct-child lifetime termination, not independently reaping/joining,
  Drop during a blocked native write, independent supervisor continuity or descendant cleanup.
  The reap/join guarantee is also supported by the reviewed ownership source and exit/drain tests.

Independent GPT/Sol source review found two error-path defects, both corrected by parent:
termination phase wrongly suppressing unadmitted graceful stop after failed kill, and attachment
failure dropping untransferred pipe handles. The above concrete fault witnesses were added.
Sol's source-only follow-ups accepted both production fixes and the later observation/queue
fault witnesses. The final assertion-strength note is corrected: the observation test directly
counts all three retained worker handles before and after the error, rather than merely asserting
that some worker remains. No remaining verified finding was reported within this scoped follow-up;
this is not executed evidence and does not satisfy the required final whole-rebuild review.

## Execution history

All selected commands use the root workspace manifest and `-p vesta-game-runtime`:

```sh
cargo fmt -p vesta-game-runtime
CARGO_INCREMENTAL=0 cargo check -p vesta-game-runtime --all-targets --offline --locked
CARGO_INCREMENTAL=0 cargo test -p vesta-game-runtime --offline --locked
CARGO_INCREMENTAL=0 cargo clippy -p vesta-game-runtime --all-targets --offline --locked -- -D warnings
```

- Initial all-target check failed with E0503 borrow errors in the unexecuted Bunny integration
  draft. No tests/Clippy ran. Independent inspection also found contradictory timeout/exit
  assertions, unsupported-stop semantics, startup-file races and waits without stop delivery.
  Parent replaced the batch rather than weakening production behavior.
- Corrected pre-review run: check and **39 runtime tests passed**; strict Clippy failed on two
  test-only double-ended `.last()` calls. That sequence is not a strict-Clippy pass.
- Post-review-fix run: check and **50 runtime tests passed** (22 unit, 28 integration; the unit
  count includes one gated inert-subprocess harness entry), zero failures/ignored. All real
  fault/inherited-pipe/Windows-unwind witnesses executed. Strict Clippy still refused the
  interim `.filter().next_back()` spelling; it was replaced with `.rfind()` without suppression.
  This is also not a strict-Clippy pass.
- Corrected selected run after `.rfind()` replacement: formatting/check, **50 runtime tests**
  and strict all-target Clippy passed. This predates the observation/full-queue witnesses and
  worker-count assertion, so it is not final-source verification.
- First later full-workspace attempt: formatting/check and **388 Rust tests passed**, zero
  failures/ignored, including the two new fault witnesses. Strict Clippy then failed on an unused
  `retained_workers_for_test` helper: the run encountered it between its introduction and its
  assertion call sites while source edits were still in flight. No warning suppression was added;
  both before/after assertions now use it. The sequence failed with exit 101; Node/accounting/diff/
  protected-digest steps chained after Clippy did **not** run. This is not an integrated pass.
- Fresh settled-source full-workspace rerun: **passed**, exit 0, on native Windows 11 /
  Rust and Cargo 1.96.0. `cargo fmt --all -- --check`, offline locked workspace/all-target check,
  offline locked workspace tests (**388 passed**, zero failed/ignored), and strict offline locked
  workspace/all-target Clippy all passed. `vesta-game-runtime` contributes **52 entries** (24 unit,
  28 integration), including one gated inert-subprocess harness entry rather than a standalone
  behavioral assertion. That is 35 package entries added over the prior 17-entry console package.
  All selected native/fault witnesses, including the all-three-handle and gated-queue cases, ran.
- In the same successful sequence, **159 Node tests passed**, zero failed/skipped; parity structure
  and requirement-ledger checks passed; `git diff --check` passed. All **222 Java byte hashes** and
  **370 original requirement IDs/text pairs** remain intact. Inventory dispositions stay **24
  migrated, 17 retained Java plugin, 181 not migrated, 0 verified**; no parity JSON was changed.
  The protected launcher manifest's SHA-256 remains
  `4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`.

The full sequence, run against the root server manifest with `CARGO_INCREMENTAL=0` for Cargo
check/test/Clippy, was:

```sh
cargo fmt --all
cargo fmt --all -- --check
cargo check --workspace --all-targets --offline --locked
cargo test --workspace --offline --locked
cargo clippy --workspace --all-targets --offline --locked -- -D warnings
node --test scripts/requirements-ledger.test.mjs scripts/server-feature-protocol.test.mjs scripts/rust-wrapper-parity.test.mjs
node scripts/rust-wrapper-parity.mjs
node scripts/requirements-ledger.mjs --check
git diff --check
sha256sum ../launcher-server-mvp/vesta-launcher/src-tauri/Cargo.toml
```

Three further settled-source `cargo test -p vesta-game-runtime --offline --locked` repetitions
also passed: **52 entries each**, zero failed/ignored, followed by format and diff checks. These
repeated runs exercise timing-sensitive native fixtures again; they are not 156 distinct tests
or a claim of exhaustive concurrency coverage.

Dependency/fixture execution here is native Windows evidence only. Real Minecraft, providers,
Mojang, Java launch shapes, other platforms, declared MSRV and release readiness were not tested.

## Preserved gaps and requirement accounting

Relevant frozen requirement families include `installation-components-and-process-continuity-010`
(literal launch), `-002` (owned stdin/drain/buffer/exit portions), `-004` (generation/no duplicate
portions), `-013` (surfacing detached-child limitations), and `lifecycle-maintenance-and-sleep-mode-001`
(graceful stop/stopping/timeout/separate kill portions). This component covers bounded portions;
none is promoted, and canonical authorization/confirmation for kill remains unfinished.

Generic literal launch intentionally does not recreate Java jar/argfile/native class shapes,
wire names, relative-path resolution, legacy validation bypass or JDK character counts. A trusted
backend software-plan adapter must still preflight primary JAR/argfiles/artifacts, Java policy,
installation receipts/provenance and complete launch composition. Literal token tests do not
substitute for that behavior or real inert Java launch-shape execution.

Production durable lifecycle bindings/workers/effect markers, confirmations/recovery, authenticated
canonical transport/command and console-read policy, protected IPC/independent supervisor/crash
continuity, process-tree/Job Objects/isolation, readiness/crash-loop breaker/sleep/service adapters,
installer/backend host composition and mandatory final whole-rebuild review remain unfinished.
MSRV 1.85, Linux/macOS/other architectures and real Minecraft/provider execution remain unverified.
No EULA/world/service/firewall/router/signing-setting effects, protected-launcher edits, push, PR
or publication are authorized/performed here. Local unsigned checkpoint follows only verified work.
