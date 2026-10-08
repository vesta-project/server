# Rust operation observations and staged durable kernel

Date: **2026-10-08**, native **Windows 11 / Rust and Cargo 1.96.0**.
Status: **selected control and fresh four-member workspace verification passed;
scoped independent source review completed**. This is not the final whole-rebuild review.

This is a bounded increment of the approved [migration](RUST_WRAPPER_MIGRATION_PLAN.md) and
[operations plan](RUST_OPERATIONS_KERNEL_PLAN.md), not Java API emulation or completion of durable
workers, effects, confirmations, replay, or authenticated management. Schema version 5 is reused;
no database migration or dependency change was needed for operations. Java remains available.
All 222 inventory entries/byte hashes and 370 original requirements/statuses remain preserved.
No operations inventory row or whole requirement is promoted by this checkpoint.

## Ordinary library boundary

`vesta_control::operations::Operations` exposes `new`, `get`, `list`, and `request_cancel` over a
trusted `Subject` lookup key. Those IDs are not authenticated transport/device evidence.

Reads resolve exact current `tasks.view` in the same server-filtered snapshot as the task
projection. Limits 1..=200 and nonnegative sequence cursors are validated without clamping.
Summaries expose only fixed lifecycle facts (ID/sequence/state/percentage/cancel flag/timestamps),
never input/result/error/progress text, arbitrary headers, actor/device, lockset, hash or owner.
This is not journal replay, fixed-horizon synchronization or an authenticated remote endpoint.

There is **no `tasks.cancel` capability**. Nonterminal cancellation requires current task access,
exact initiating principal **and device**, a hash-verified canonical server-derived binding and
all of its exact current action requirements in the same writer. No rank/Owner bypass is added.
Terminal no-op still requires access/exact actor-device but may use purged input. RecoveryRequired
remains nonterminal and refuses cancellation. The **production binding registry is empty**;
unbound retained work fails closed, including test binding names in an ordinary linked library.
Public production observation is useful; this increment admits no executable production action.

The crate-private `identity::policy_in` delegates to the existing policy repository using the
supplied connection/server/time. It opens no transaction and accepts no retained policy grants.
Three real-DB seam tests demonstrate uncommitted revocation, exact expiry/server behavior, and
same-snapshot policy consistency across a concurrent committed revocation. A gated-clock public
integration witness separately demonstrates old policy **and old task together** while a concurrent
writer commits revocation plus task state before the read resumes; the next get/list is forbidden.

Mutation, success audit and sanitized lifecycle event share one typed immediate transaction.
Only clean domain rejection is separately audited after rollback; policy/infrastructure/rollback
failures remain typed errors, and a failed refusal audit preserves the primary refusal.

## Explicit staging boundary

Private admission, OS owner/claim/release, startup reconciliation and terminal-input purge are
**`cfg(test)` staged kernel code**, absent from ordinary library builds and the library linked by
integration tests until reviewed trusted composition and real closed domain bindings exist.
Exclusively used helpers/types/fields are gated with the roots, not warning-suppressed or exposed
publicly. All shared production bound checks, raw header corruption validation, canonical binding,
lock validation, current policy and public mutation/audit/event helpers remain ordinary code.
Activating the staged kernel requires production-mode builds/tests/review, not merely ungating it.

The source and private real-state tests establish a staged design for:

- 64 KiB raw/canonical bounds, semantic round-trip stability and exact canonical SHA-256;
- SQL UTF-8-byte/NUL/header guards before string materialization, 32 resources with a 33rd sentinel;
- exact current-authority idempotency binding (principal/server/device/source/type/target/hash,
  requirements/locks/safety), including retained purged terminal input;
- one-transaction sorted all-or-none claims and transitive conflicting FIFO across bounded
  keyset pages; independent work progresses, allocation is bounded but queue latency is not;
- actual state-directory OS runner lock, opaque redacted token/session-bound claims, non-Send/
  non-Sync borrowed synchronous owner, wrong/stale/NULL owner rejection;
- claimed cancellation retaining locks/token until exact known-unstarted release; dropping an
  owner or claim never fabricates completion or actual worker termination;
- startup preserving unknown/unverifiable started effects as RecoveryRequired with input/locks
  retained regardless of a stored idempotent bit; no blind replay or automatic resolution;
- terminal-input retention refusing nonterminal/recovery or contradictory held locks.

These are **staged unit-test witnesses**, not a shipped scheduler, production handler, confirmation
consumer, effect marker/dispatcher, worker safe point, recovery resolution or remote endpoint.

## Source review and integration history

Independent Sol source review found no concrete authorization/canonical/FIFO/atomicity defect.
Parent's earlier preflight identified source binding inconsistency and historical started-terminal
lock handling; the implementer corrected them with changed-source, legitimate started-terminal/
no-lock and terminal-held/orphan regressions. The later independent review identified additional
witness gaps, not demonstrated production failures: policy infrastructure versus ordinary denial,
public policy+task snapshot coupling, and late failure after an earlier startup transition.
Parent implemented the public race witness; the reviewer supplied four separately owned private
regressions using actual malformed active denies, audit/event triggers, OS lock reacquisition,
and a second-row trigger requiring the first row's state/token/locks **and both new journals**
to have changed before it fires. Parent owns inspection, wiring and executed verification.

Failed attempts remain historical failures:

1. First all-target check compiled with 14 warnings from intentionally unwired private roots.
   Roots/exclusive dependencies were explicitly staged behind cfg(test), not authority-widened.
2. First control unit run: **230 passed / 1 failed**. A redaction assertion searched the generic
   input word `requested`, which also matched fixed field `cancel_requested`. It now directly
   admits/persists/retries a distinctive planted payload and checks actual sentinel redaction.
3. First public integration run: **1 passed / 6 failed during fixture setup** because parent SQL
   omitted two required schema booleans. Fixture inserts were corrected; schema was not weakened.
4. Corrected control run: **231 unit + 11 integration + 2 compile-fail doc tests passed**;
   strict Clippy then refused the complex selected-lifecycle tuple return type. A named test-only
   alias resolves it without suppressions; the helper deliberately observes selected columns,
   not every raw header/input byte. This run is not a strict-Clippy pass.

The fresh post-review selected control sequence passed package formatting, all-target offline
locked check, **248 tests** (235 unit, 11 integration, 2 compile-fail docs), zero failures/ignored,
and all-target strict Clippy. All four review regressions executed and passed. This checks ordinary
and unit-test build modes; it is not yet the final integrated four-member workspace run.
No helper's source-only report was used as executed evidence.

```sh
cargo fmt -p vesta-control
CARGO_INCREMENTAL=0 cargo check -p vesta-control --all-targets --offline --locked
CARGO_INCREMENTAL=0 cargo test -p vesta-control --offline --locked
CARGO_INCREMENTAL=0 cargo clippy -p vesta-control --all-targets --offline --locked -- -D warnings
```

Commands used the root workspace manifest. The later fresh integrated run passed workspace
format check, all-target offline locked check, **353 Rust tests** including compile-fail docs,
zero failures/ignored, and all-target strict Clippy. **159 Node tests**, parity inventory validation
of all **222 Java hashes / 370 frozen requirements**, requirement-ledger check and git diff check
also passed. Exact integrated commands and preservation hash are recorded in
[the console checkpoint](RUST_CONSOLE_CAPTURE_CHECKPOINT.md#fresh-integrated-windows-result).
No source changes followed that final run. Runtime/operations inventory dispositions remain
unchanged (24 migrated, 17 retained Java plugin, 181 not migrated, 0 verified); this deliberately
does not mark staged kernel tests as shipped production behavior.

## Remaining limitations

Blocked-writer private tests signal a held writer plus requester-thread start, not observed
requester-queue admission; early-time-sampling mutant detection is not deterministic on every
schedule. The public gated-snapshot witness has an explicit committed-before-release handshake.
Future bindings must derive conservative filesystem conflicts; exact lexical lock keys are not
hierarchical/native path confinement. No real worker/process/provider/game/network effect ran.

Workers retaining database/OS lock/ownership through actual uncooperative exit, real effects and
revision-bound single-use confirmations, authorized replay/recovery resolution, game/files/install
bindings, authenticated device/session/claim/transport composition and the final whole-rebuild
independent Sol review remain unfinished. Windows execution does not verify declared MSRV 1.85,
Linux/macOS/other architectures or release readiness. No protected launcher edits, EULA/world
changes, service/firewall/router/signing-security changes, push, PR or publication occurred.
