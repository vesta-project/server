# Coordinated Rust migration pass

Date: **2026-10-08**. Status: **assessment and implementation in progress; not verified**.

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
passed: **635 tests, 0 failed, 0 ignored** across 36 test binaries
(`cargo test --workspace --offline --locked -- --test-threads=1`), on top of passing workspace
format, all-target check and strict all-target Clippy.

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
The control suite and final integrated workspace/strict-Clippy pass remain pending.

### Behavior accounting boundary

The operation candidate supplies useful equivalents across Java kernel/submission/authority,
confirmation/replay and lifecycle/runner families, without copying their class/handler registry
architecture. Rust evidence is concentrated in `vesta-control/src/operations` (closed typed
bindings, transaction-taking private repositories, owned driver and focused tests), with native
process effects in `vesta-game-runtime/src/process` and lifecycle/receipt durability in
`vesta-state`. Generic Java handler interfaces are not new Rust production requirements.

The preserved inventory remains **181 not_migrated, 24 migrated, 17 retained Java plugin,
0 verified**. No row or whole requirement is advanced by this partial slice. In particular,
`OperationService`, `OperationSubmissionService`, `OperationRunner` and their test families still
need complete behavior-by-behavior disposition, including non-game domain composition and staged
purge/progress functions. This record supplies bounded progress/evidence, not a claim that every
method or future binding has a production counterpart. Protected files/content, packs/sync/ZIP,
configuration/application, safe transports and shared host/plugin ownership remain open, alongside
the separate OS-authentication, supervisor, installer activation, platform/MSRV and release gates.

