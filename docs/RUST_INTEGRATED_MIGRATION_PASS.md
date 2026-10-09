# Coordinated Rust migration pass

Started: **2026-10-08**. Updated: **2026-10-09**. Status: **bounded game-operation slice locally checkpointed and Windows-tested; migration and final review incomplete**.

The pause/failed-attempt sections below are historical. The final executed results and local
checkpoint are recorded near the end; they do not certify the remaining wrapper rebuild.

The owner requested a larger parallel implementation and integration pass rather than a series
of tiny checkpoints. Scope is useful behavior already implemented in the retained Java
wrapper/core, with idiomatic Rust equivalents and explicit dispositions for unsafe legacy
routes. This is not authorization to complete every future product requirement by stubbing it.
Java sources/tests/builds and the Minecraft plugin/mod remain available for comparison.

## Integration rules

- Parent settles shared interfaces and owns Cargo manifests/lockfiles, integration, executed
  verification, accounting and cleanup. Sol owns serious backend implementation and independent
  review; Luna owns bounded mechanical inventories/test work against settled interfaces.
- Space Bunny is unavailable according to the owner and is not retried during this pass.
  No Anthropic/default-model agents or nested orchestration are used.
- Writers have disjoint explicit file ownership. No blanket source translation, cfg(test)
  authority exposure, fake production handlers or skipped/ignored assertions to force a pass.
- Integrate one candidate, then run the full native Windows workspace, companion installer,
  Node accounting and preservation checks; fix failures and rerun before a local checkpoint.
  Small compile checks may occur during implementation but are not final test evidence.
- Final independent whole-rebuild review and complete behavior accounting are separate gates.
  No migration, requirement, MSRV, platform or release status is promoted by intent or count.

## Preserved constraints

Microsoft/Minecraft identity is the normal identity flow; management requires current
server-local canonical authority. OP, connector possession, loopback, passwords, Minecraft
receipts, local installer consent and process ownership do not confer administration.
Production OS-local authority remains unavailable until actually implemented.

Installation stays on the trusted server backend. Clients signal desired changes, not URLs,
digests, executable/transport policy or paths. Preparation may occur while the game runs;
activation requires observed actual exit, without automatic stop/restart or world restore.

Tests use real owned temporary SQLite/filesystem fixtures and inert owned local processes or
local HTTP/TCP fixtures only. No real provider/Minecraft/Mojang execution or request, EULA
acceptance, world/demo reset, service/firewall/router/exposure/signing-setting change, push,
PR, publication or workflow dispatch is authorized.

The protected launcher `vesta-launcher/src-tauri/Cargo.toml` remains outside edits, formatting,
staging and commits. Required raw SHA-256:
`4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`.

## Starting checkpoints and evidence

- Server: `ea4796fd05be7dc682ea67728ecd535ed0501fc7`.
- Companion: `9a5be4281bd9d610050cc5eba6b8c9081e34db9e`.
- Prior settled Windows/Rust 1.96 suites: 556 server Rust, 210 Piston and 159 Node tests.
  These are **prior** results, not verification of this pass.
- Both origins refreshed; server `pull --ff-only origin main` reported already up to date;
  companion has no missing origin/main commits. Protected work was not merged or rewritten.
- All 222 Java inventory entries, 370 frozen requirements and their historical evidence remain
  required. Paired revision-pinned CI/release wiring and unexecuted platforms/MSRVs remain open.

## Pause checkpoint — 2026-10-08

The owner reported only **3% of the five-hour session budget** remained. Four implementation
agents were stopped to avoid unreviewed work continuing past the pause. This is an intentionally
incomplete, uncommitted working-tree checkpoint, not a validated implementation.

### Work present at pause

Modified tracked server files:

- `crates/vesta-control/src/operations/domain.rs`
- `crates/vesta-control/src/operations/input.rs`
- `crates/vesta-control/src/operations/mod.rs`
- `crates/vesta-control/src/operations/repository.rs`
- `crates/vesta-game-runtime/src/process/mod.rs`

Untracked server files:

- `crates/vesta-control/src/operations/bindings.rs`
- `crates/vesta-control/src/operations/confirmation_repository.rs`
- `crates/vesta-control/src/operations/replay.rs`
- this checkpoint document

No files were staged or committed for this pass. Do not treat the partial agent implementations as
reviewed; inspect every diff before resuming. The launcher still has only the pre-existing dirty
protected manifest, excluded from all work. Its raw SHA-256 was rechecked and remains
`4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`.

### Verification outcome

A parent-run offline command was attempted:

`CARGO_INCREMENTAL=0 cargo check --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --all-targets --offline --locked`

It **failed (exit 101)** because implementation was interrupted mid-integration. Output included
missing `confirmation`, `game`, and `runner` modules; missing replay tests; `vesta-game-runtime`
not yet added as a `vesta-control` dependency; unresolved/test-gated operation types; queue methods
and repository APIs not yet integrated; and an unresolved replay/kernel seam. Runtime guarded-I/O
helpers also emitted dead-code warnings. This is expected incomplete work, not a regression verdict.
No tests, strict Clippy, full final verification, independent source review, accounting updates, or
checkpoint commit were performed for this pass. The prior 556/210/159 green test results belong to
the earlier installer extraction only.

### Resume sequence

1. Re-read this document and inspect the entire working tree/diffs; retain the two prior local
   checkpoint commits as the clean baseline and preserve the protected launcher file.
2. Re-establish the closed interfaces between production operation kernel, confirmations,
   authorized replay and the concrete owned game runtime. Do not copy provisional agent designs
   blindly or widen authority to make compilation pass.
3. Complete integration with explicit module/API ownership, add only the necessary reviewed state
   migration (accepted confirmation receipt must be distinct from a merely consumed/refused token),
   and preserve current transaction/audit atomicity and owner/worker/OS-lock lifetime.
4. Correct all compile/lint/test failures without weakening existing tests. Then run fresh server
   workspace format/check/test/strict Clippy, companion Piston check/tests, Node/parity/requirements
   checks, diff checks and protected digest verification on Windows.
5. Obtain the required independent Sol whole-rebuild review only after a coherent candidate exists;
   resolve findings and rerun final tests. Update the behavior inventory and dated accounting with
   exact verified scope. Commit unsigned only after the candidate is reviewed and green.

Current-Java inventory assessment found 181 of 222 Java rows still marked `not_migrated` (126
production, 50 tests and 5 helpers); 24 mapped as migrated, 17 retained plugin, and zero verified.
The proposed operations/replay/runtime work is only one slice. Current-Java file/content, pack,
configuration, server transports and host composition remain unaccounted/unimplemented in Rust;
future product/security/platform gates remain separate and must not be fabricated to call the rebuild
done. The migration is **not complete**.

## Resumed integration — 2026-10-09

The owner resumed the approved work. Parent inspected all paused tracked diffs and the three new
implementation files before restarting the four explicitly scoped writers. Both origins were
refreshed again; neither work branch has missing origin/main commits, so no merge was required.
The protected manifest raw digest remains unchanged. Space Bunny is not retried.

The next integrated candidate is the real closed game lifecycle operation path: production
admission/ownership/run/effect/finish, single-use confirmations, bounded task replay and an owned
native game driver. It is not the whole current-Java rebuild. Shared interfaces are fixed before
further writes; no caller-selected capability/lock/source revision or public handler/authority
registry is permitted.

Parent added the `vesta-control -> vesta-game-runtime` local dependency edge, a state runtime lease,
and schema-v6's nullable `confirmations.accepted_at` with no backfill. A consumed/refused legacy
token is not an accepted receipt. Confirmation token burn and accepted or refused decision/audit
must share one admitted writer; infrastructure failure rolls back. Token TTL limits acceptance;
execution additionally rechecks the exact accepted binding, current policy and actual generation.

`StateDatabase::retain_runtime()` keeps closure from discarding a live runtime's state. While any
lease lives, explicit close returns `state_runtime_busy` and leaves admissions open so the owner
can finish recording outcomes. The opaque lease/core remains owned through claims, native guards,
workers and actual joins. Parent's offline state all-target compilation passed; this is compile
readiness only, not executed new tests or final verification.

The interrupted alternate direct-I/O implementation is rejected: native stop authorization must
run on the existing owned bounded stdin writer immediately before native write, with a typed
acknowledgement and exact claim/database/OS-lock retention even after timeout. Start/kill gate
immediately before their native effects after inert preflight. Task replay checks current
`tasks.view` in the same snapshot and returns strictly validated fixed historical lifecycle facts,
not raw input/result/progress text or a fabricated historical summary. It does not require the
mutation capability merely to observe sanitized tasks.

New focused state retention/receipt-migration tests are assigned to a bounded Luna helper. The
parent owns manifests, schema, executed verification, integration, preservation accounting and
cleanup. The four Sol source authors retain disjoint kernel, confirmation, replay and native
runtime/driver ownership. No full pass, independent review or new commit is claimed yet.

### Interim integration checks and corrections

Parent's first resumed workspace/all-target offline locked check failed (101): runtime tests were
still being authored and the lifecycle object-shape helper was being integrated. The next check
passed (0), with unused staged retention/progress reachability warnings still to resolve; this is
not strict-Clippy or final-source verification.

Parent executed the complete state suite on Windows: **66 tests passed**, none failed/ignored.
This includes runtime-lease closure/retention races and the real v5-to-v6 receipt migration/backup
fixtures. The earlier expiry migration test now constructs a genuine v4 schema by removing both
v5 indexes **and** v6's column offline before replaying all migrations; no false version-only
fixture or weakened preservation/index assertions is used.

Parent executed Node contracts/accounting: **159 passed**, none failed/skipped. The checker
preserves all **222 Java inventory entries** and **370 frozen requirement bullets**. These checks
are preservation evidence, not operation parity or completed product requirements.

Two independent scoped Sol reviews cover confirmations/replay/state and native operation
ownership; they are not the final whole-rebuild review. Confirmation review verified no concrete
production defect but identified public-orchestration and event-insert-fault coverage gaps, now
assigned to the source author. Owner review identified a stale-observation stop-timeout defect:
losing native observation must persist uncertainty, disable the ordinary stop deadline, retain
claim/locks until actual child/I/O exit, and then record recovery rather than a fabricated timeout.
That fix and deterministic regression are in progress. Known-effect completion after revocation
is private factual bookkeeping with fixed denial metadata, never permission for a new effect.

The stop policy is explicit: an unanswered native callback retains the operation lock. Only a
returned successful stop request plus reliable live-child observation permits bounded
`game_stop_timeout`; the task then fails but exact game/IO/OS/database ownership remains. This
allows a later authorized, confirmed force-kill without releasing an unreturned worker or
pretending a draining generation has finished. See [the API](RUST_GAME_OPERATIONS_API.md).

### Stable-source verification and review follow-up

The source authors completed the closed kernel, confirmations, replay and concrete driver with
isolated formatting and froze their owned files. No author-run Cargo checks or tests are counted
as evidence. The confirmation/replay/state Sol reviewer closed both coverage findings by inspecting
seven real public runtime gateway tests, including late event insertion failure after success
auditing (burn, receipt, queue, audit and event all roll back). This is source coverage, not an
executed pass, and neither scoped reviewer is the final whole-rebuild review.

The native observation defect is corrected with sticky uncertainty, unavailable confirmation
revision, disabled stop deadline and exact ownership through actual child/I/O exit. The driver
applies this to observation and Start/Stop/ForceKill preflight polls. Authored regressions cover
real native write before acknowledgement, pre-effect versus post-ack cancellation, and a pending
guard held beyond the configured stop timeout with exact token/lock/database/OS ownership intact.

Parent's first format check failed (exit 1) on the newly added RuntimeLease Debug chain wrapping;
only formatting was corrected. A subsequent workspace format/all-target check passed, but strict
Clippy failed (exit 101) on an unreaped inherited-pipe fixture child (`zombie_processes`). The
chained full test command was therefore **not reached**. No strict-Clippy or full-server test pass
is claimed from those attempts.

The orphan descendant was removed, not warning-suppressed. Replacement fixtures retain real owned
stdin callbacks: one witnesses actual child exit with pending acknowledgement; another observes
the first real stop-write receipt and enters `Active::Exit`, while a second owned fixed-refusal
callback keeps native I/O Draining beyond the stop deadline. It never sends a second stop. RAII
releases the gate before blocking runtime destruction, and all native workers are actually joined.
A production-used pure deadline predicate also has a phase/uncertainty/deadline matrix regression.
These prove generic owned-I/O draining, not inherited stdout/stderr descendant behavior. The
existing native runtime tests are unchanged; stronger process-tree coverage remains a separate gap.

After that fixture correction, parent reran workspace format, all-target check and strict all-target
Clippy successfully. The chained entire-workspace Windows test suite then executed and reported
**299 control tests: 297 passed, 2 failed, none ignored**.
Cargo aborted before later workspace binaries. The failures were the post-ACK cancellation test
(`owner_stopping`) and the real observation-uncertainty regression. Tracing the cancellation test
identified a production shutdown race, not a missing authority grant: shutdown can set the exact
owner stopping flag between the dispatch check and claim, and that ordinary refusal was incorrectly
reported as a driver failure. Only this exact `OwnerStopping` rejection is treated as clean stop;
all other errors and any existing failure remain intact. The latter exposed a production
namespace panic: the driver constructed a `StateError` from a native `game_observe_failed` code.
The correction uses a typed native `OperationError::Runtime`, not a weakened database-error
namespace or a misleading database code. A focused classification regression is added as well.

The final fixture source rereview also found a panic-cleanup hang before the runtime wrapper was
constructed. Fixture-only ownership now carries both child-release and callback-release protection
together with the Driver from construction through every assertion, wait and thread-spawn handoff,
and transfers it to the runtime wrapper. Production observation-only Drop is unchanged.

Parent then applied the settled corrections: a typed `OperationError::Runtime(RuntimeError)` with a
`From` conversion and a private classification regression, so no native code is ever forged into
the database error namespace; sticky driver uncertainty stored as the typed native error, converted
to its fixed classification only where an audit or retained-uncertainty fact is recorded; and clean
classification of exactly the `OwnerStopping` claim refusal caused by an ordinary shutdown racing
the dispatch check. Every other claim error, and any prior failure, is preserved. No authority,
effect gate or prior failure was relaxed to fix this. The author's isolated formatting had skipped
child modules, so the workspace format check found one unformatted test file; parent ran the real
workspace formatter, then format, all-target check and strict all-target Clippy passed.

The two previously failing tests then passed, and the fresh complete workspace Windows test suite
passed: **635 tests, 0 failed, 0 ignored** across 36 test-result summaries
(`cargo test --workspace --offline --locked -- --test-threads=1`). Workspace format passed;
the immediately preceding post-fix all-target check and strict Clippy targeted `vesta-control`
and its dependencies. The other packages had passed the earlier whole-workspace checks, but
that earlier command is not a freshly executed final whole-workspace lint result.

### Scoped independent review status

The confirmation/replay/state Sol review closed both of its findings with no further concrete
defect. The native ownership Sol review verified and closed the sticky-observation defect, the
queue-admission timeout gap and the Stop-admission propagation gap, then found and drove the
fixture-cleanup hang and the orphan-descendant ownership problem. It could **not** complete its
rereview of the very last patches — the typed native error conversion, the clean `OwnerStopping`
classification and the fixture ownership transfer — because the Sol agent credentials hit a usage
limit (HTTP 429) and the agent is no longer reachable. Those final edits were applied and verified
by the parent directly. The native reviewer also left two **non-blocking gaps** it did not close:
no inherited stdout/stderr descendant witness (the replacement proves generic owned-I/O draining
only), and the Stop-admission `ObserveFailed` path is exercised through the shared failure
transition rather than by forcing that exact admission error. Neither is inferred covered. A
fresh independent review of the final source is outstanding.

### Local checkpoint

After the full green pass, parent created the authorized unsigned local checkpoint
`2f2e3804cc4e03b51f26e2b07d9b49e05381abde` on `feat/server-software-foundation`, containing only the
operation kernel, confirmation, replay, driver, state lease/migration and documentation changes.
Both working trees are otherwise clean; the companion retains only its pre-existing protected
manifest change. Nothing was pushed, published or merged.

The final Node/preservation rerun passed **159 tests**, none failed/cancelled/skipped/todo, and
both inventory/requirement checkers passed. Complete requirement ledger JSON is unchanged from
the starting server HEAD. The protected launcher manifest raw digest remains exact; only its
pre-existing dirty status remains in the companion tree. Parent's companion all-target check and
**210 Piston tests** also passed during this continuation, with only the pre-existing unused
`tokio::task` import warning. The Cargo lockfile change is one local runtime dependency edge, not
a registry repin. These bounded results do not promote migration or platform/MSRV gates.

While the control fixture correction was isolated, parent executed the four other frozen crates:
`CARGO_INCREMENTAL=0 cargo test --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --exclude vesta-control --offline --locked -- --test-threads=1`.
It passed (exit 0): **322 tests**, none failed or ignored (protocol 29, state 66, game runtime 59,
installer 168). This includes actual new guarded-native tests and the refactored state fixtures.
At that intermediate point the control suite and final integrated checks were pending; the
subsequent 635-test workspace pass and scoped post-fix lint results above supersede that status.

### Continuation verification — 2026-10-09

Against source checkpoint `2f2e380` and documentation HEAD `ed99439`, parent freshly ran:

```text
cargo fmt --manifest-path V:/server-connections-mvp/Cargo.toml --all -- --check
CARGO_INCREMENTAL=0 cargo check --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --all-targets --offline --locked
CARGO_INCREMENTAL=0 cargo clippy --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --all-targets --offline --locked -- -D warnings
```

All three passed on native Windows (combined command exit 0). This is fresh whole-workspace
compile/lint evidence, separate from the earlier 635-test run; tests were not rerun by this
command. A separate fresh Node run (`node --test scripts/*.test.mjs`) passed **159 tests**,
none failed/cancelled/skipped/todo; the parity and requirement ledger `--check` commands passed,
retaining all **222 Java files** and **370 frozen requirements**. Server source was clean and
the protected companion manifest digest remained exact.
A separate fresh serial baseline workspace test command also passed **635 tests**, zero
failed/ignored, before the launch increment. The focused independent Sol rereview then found a
real propagation defect: sticky observation failure introduced inside dispatch could retain
ownership/recovery correctly yet disappear from the driver's returned result after successful
later polls. The parent corrected the return-path merge without loosening ownership, and added
late-dispatch and earlier-error-preservation regressions. First compilation of those tests failed
on two mistaken test-only `runner_lock` calls; corrected them to the existing
`acquire_runner_lock` API. The fresh focused driver suite passed **14 tests**, zero failed/ignored.
The late-dispatch regression injects the existing private failure transition after real Start;
it does not force a native polling syscall failure. Sol's correction/launch scoped rereview is
complete: the propagation defect and one new unsupported-stop fixture-readiness race are closed,
with no remaining concrete scoped defect found. Parent's fresh settled-source whole-workspace
format/check/tests/strict Clippy then passed **658 tests**, zero failed/ignored. Fresh companion
check/**210 tests** and Node/**159 tests** plus both accounting checkers passed. The protected
manifest digest remains exact. See [the launch checkpoint](RUST_LAUNCH_CONFIGURATION_CHECKPOINT.md)
for failed attempts, exact commands and limitations; final whole-rebuild review remains unfinished.

The owner now requests fewer helpers, with bounded Luna or explicitly requested/verified Haiku
work and serious backend/review remaining with Sol. No requested Haiku 5.5 registration was
verified or silently substituted; this continuation uses one Sol reviewer and bounded Luna work.

### Behavior accounting boundary

The operation candidate supplies useful equivalents across Java kernel/submission/authority,
confirmation/replay and lifecycle/runner families, without copying their class/handler registry
architecture. Rust evidence is concentrated in `vesta-control/src/operations` (closed typed
bindings, transaction-taking private repositories, owned driver and focused tests), with native
process effects in `vesta-game-runtime/src/process` and lifecycle/receipt durability in
`vesta-state`. Generic Java handler interfaces are not new Rust production requirements.

The operation checkpoint left the inventory at **181 not_migrated, 24 migrated, 17 retained
Java plugin, 0 verified**. No row or whole requirement was advanced by that partial slice.
The subsequent bounded launch-definition increment maps only `LaunchSpec`/`LaunchSpecTest`,
with explicit Rust-native behavior dispositions: **179 not_migrated, 26 migrated, 17 retained
Java plugin, 0 verified**. See [the launch checkpoint](RUST_LAUNCH_CONFIGURATION_CHECKPOINT.md)
for the scoped review, exact execution and unresolved behavior; no whole requirement is promoted. In particular,
`OperationService`, `OperationSubmissionService`, `OperationRunner` and their test families still
need complete behavior-by-behavior disposition, including non-game domain composition and staged
purge/progress functions. This record supplies bounded progress/evidence, not a claim that every
method or future binding has a production counterpart. Protected files/content, packs/sync/ZIP,
configuration/application, safe transports and shared host/plugin ownership remain open, alongside
the separate OS-authentication, supervisor, installer activation, platform/MSRV and release gates.

## File observations / pure catalogue reconciliation continuation — 2026-10-09

Actual implementation following the trusted launch checkpoint adds canonical protected
file observations in `vesta-control::files`, private validated Rust-native catalogue/sync
values in `vesta-protocol::pack`, and ten real SQLite assertion tests over existing
state/audit/settings/events. Existing index projection and Java/plugin wire/source
contracts are retained. File writes/deletes, network source execution, archive extraction
and administrative transport are not introduced. No Java-style handler/service registry
or injected permission callback is added to bypass durable application authority.

The first settled-source integrated run passed **707 Rust tests**, zero failed/ignored,
workspace format/all-target check/strict Clippy, **159 Node tests**/both accounting checkers
and **210 companion tests**. Scoped Sol review then identified missing configured-secret
ancestor alias loss and existence-probe gaps; parent source/regression corrections are
recorded with the exact final verification and failed attempts in
[RUST_FILE_PACK_CHECKPOINT.md](RUST_FILE_PACK_CHECKPOINT.md). Fresh final corrected-source
verification passed **710 Rust tests**, zero failed/ignored, workspace format/all-target
check/strict Clippy and fresh Node/accounting/preservation checks. The earlier pre-correction
pass is not final-source evidence, and the scoped review is not the mandatory whole-rebuild review.

Inventory accounting remains **26 migrated / 17 retained Java plugin / 179 not_migrated /
0 verified**, with all frozen Java hashes and 370 requirement IDs/text/status/history
unchanged. Full file/content families, configured pack host/archive services,
configuration/application, safe sessions/transports and complete foundation family
accounting remain unfinished. The protected launcher manifest's inherited worktree
modification remains untouched at the recorded raw digest. No push/publication, real
game/provider/installer execution, EULA acceptance, world/demo erasure or OS/global
security changes occurred. Cross-platform source is not non-Windows/MSRV test evidence.

## Trusted host descriptions / passive CLI / bounded transport primitives — 2026-10-09

Following `f8a70c8`, strict full-host descriptions and a private protected local reader,
one-line player parsing, monotonic bounded peer admission and strict length-prefixed
UTF-8 framing are implemented. `apps/vesta-agent` is the sixth workspace member; its
only substantive command is passive `check-config`, with no database preparation,
owner generation, listener or game start. No Java-shaped configuration builder or
incidental permissive JDK decoding is restored. Configuration, IP, player text and
frame possession confer no administrative authority.

Fresh settled-source native Windows verification passed **816 workspace Rust tests
across 41 result summaries**, zero failed/ignored, workspace format/all-target check/
strict Clippy, **159 Node tests**/both accounting checkers, and **210 companion tests**/
all-target check. The existing companion unused `tokio::task` warning remains. The
prior 811-test run predates five review-suggested regressions and is not final evidence.
Scoped Sol review, parent fixes (including recursive partial-write stack safety),
failed-attempt history, exact commands, native limitations and missing functionality
are recorded in [the host checkpoint](RUST_HOST_CONFIGURATION_CHECKPOINT.md).

Inventory accounting remains **26 migrated / 17 retained Java plugin / 179 not_migrated /
0 verified**, with 370 frozen requirements and Java/reference bytes unchanged. Full
host/runtime composition, correlated player query, durable file/content effects,
configured pack/source/archive services, safe authenticated pack sessions/routes and
remaining assertion-family accounting are still unfinished. This scoped review is
not the required final independent whole-rebuild review; platform/MSRV/product/release
gates remain open. The protected launcher manifest retained its exact recorded digest.


## Pack preparation and closed player queries — 2026-10-09

Following `49b1e72` (checkpoint `61e2f5a`), explicit trusted pack preparation and a
closed `list` query are implemented.

**Pack preparation.** `files/pack_root.rs` privately reuses the native file boundary:
no-follow leaf opens, reparse and hard-link refusal, bounded streaming hashes, growth
detection against the observed metadata length rather than the budget, and post-read
size/mtime revalidation. Owned bytes are the artifact identity, never the mutable
filename. `packs/mod.rs` snapshots a configured root into a deterministic manifest plus
retained bodies under bounded file/total/scan work; hidden entries and configured/state
exclusions never enter the catalogue, and unsafe visible links or names refuse the whole
preparation rather than being catalogued. Local `.zip`/`.mrpack` archives are pinned as
opaque confined bytes and are never parsed or extracted.

`pack/sources.rs` adds a strict map-only source registry. A registered path whose content
no longer matches its pinned digest refuses instead of silently embedding bytes under a
stale provider description; a malformed registry refuses rather than degrading to empty.

**Defect found and fixed during this wave.** `SourceRegistry::source_for` was fail-open.
It consulted a case-insensitive collision scan *before* reporting a hash mismatch, so an
exact-path entry whose content differed returned `PathCollision`; a caller treating that
as "unrecognized" would answer by embedding unverified bytes under a filename the registry
already claims. The case scan is now a fallback consulted only after the exact key misses.
The protocol test that should have caught this asserted against a case variant of the
registered path, so it reached the collision branch for the wrong reason and passed
regardless — it has been corrected to exercise the exact-path/stale-hash path.

**Player queries.** `operations/players.rs` issues only the literal `list` to this
runtime's exact running generation. `ConsoleRead` is checked in an admitted current
snapshot before mailbox/native admission, again by the owned native writer immediately
before the fixed write, and again immediately before a result is exposed. The API accepts
no command text, handler or authorization callback. `process::queue_list_guarded` shares
the existing guarded writer with `stop`, re-checks generation/exit/stopping before the
guard runs, and does not mark the process stopping. Without request IDs in console output,
causal correlation is impossible, so a result describes an observed line for the requested
generation — never authenticated player identity, a causally proven response, server
readiness, or continued future liveness. Timeout, gap, exit and incomplete console text
are unavailable, never zero players or a repaired count.

**Fresh settled-source native Windows verification (Windows 11, Rust 1.96.0,
`--test-threads=1`):** **864 workspace Rust tests across 30 result summaries**, zero
failed and zero ignored; workspace format, `--all-targets` check and strict Clippy
`-D warnings` clean; **210 companion `piston-lib` tests** and its all-target check
(the existing unused `tokio::task` companion warning remains); **159 Node script tests**
and both accounting checkers valid. The protected launcher manifest retains SHA-256
`4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`.

Two Windows fixture details are recorded so they are not mistaken for policy results:
`mklink /J` only accepts its operands as simple relative names under the test process's
current directory, and its success must be judged by the created reparse point via
`symlink_metadata`, never by its exit code. Both behaviours were verified in isolation
before being fixed in the fixture.

Inventory accounting remains **26 migrated / 17 retained Java plugin / 179 not_migrated /
0 verified**, with 370 frozen requirements and Java/reference bytes unchanged. Durable
file/content effects, configured catalogue/archive *services*, safe authenticated pack
sessions/routes, full host composition and remaining assertion-family accounting are still
unfinished. This wave was not independently reviewed and is not the required final
independent whole-rebuild review; platform/MSRV/product/release gates remain open. No
Java, Minecraft, provider, installer or network execution, and no push, occurred.
