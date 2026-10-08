# Bounded Rust console-capture increment

Date: **2026-10-08**. Native **Windows 11 / Rust and Cargo 1.96.0** only.
Status: **selected package and fresh four-member workspace verification passed;
scoped independent Sol source review completed**. This is not the required final whole-rebuild review.

This is one component from the approved [Rust migration](RUST_WRAPPER_MIGRATION_PLAN.md), not
completion of game/runtime supervision, authenticated console delivery, or the Java rebuild.
The dependency-free `vesta-game-runtime` crate is now admitted alongside the three foundation
members. Cargo.lock adds only that local package; no external dependency was added or repinned.
Java comparison sources, all 222 inventory entries and all 370 original requirements remain.
No parity row or original requirement is promoted by this record.

## Actual implemented boundary

`console::ConsoleCapture` synchronously owns/drains a supplied `Read` value, without detached
workers, listener callbacks, network access, process launch or caller-chosen stream labels.
Stdout/stderr are typed; shared storage uses a private mutex/ring. Reader calls occur outside
that mutex. There is no hard read latency, deadline or cancellation claim.

- LF, CR and CRLF framing across read boundaries; empty lines are intentionally retained.
- Successful EOF flushes a final unterminated line. On read failure, completed lines remain
  published while only the incomplete failed suffix is discarded. Interrupted reads retry.
- Raw pending bytes are bounded at 16,384 + 3, including undelimited input; excess is drained
  and discarded until a delimiter. Lookahead completes a scalar crossing the publication bound.
- Invalid UTF-8 is replaced explicitly. Published text is at most 16,384 UTF-8 bytes, cut only
  at a scalar boundary, with an explicit truncation flag. This is not Java character emulation.
- Retention is bounded by both 10,000 lines and 2 MiB text payload. Metadata/allocation overhead
  is separate from the payload bound; line-count bounds keep it finite.
- Page limits 1..=200 are validated, not clamped; future cursors are refused. `next_cursor`
  means the last returned line, not the latest retained head. Eviction reports a gap while
  returning the newest retained window in order.
- Cursor arithmetic is checked before storage mutation. Storage poison refuses publication/
  snapshot with a fixed error. A capture with no publication need not access storage.
- `Debug` redacts line text and native I/O failures have fixed codes/Display without an I/O
  source chain. **The public text field remains sensitive**, not an authorized task summary.
  Future remote console reads need exact current console-read policy, never task-view alone.

## Executed selected-package evidence

Commands used the workspace manifest with `-p vesta-game-runtime`:

```sh
cargo fmt -p vesta-game-runtime
CARGO_INCREMENTAL=0 cargo check -p vesta-game-runtime --all-targets --offline --locked
CARGO_INCREMENTAL=0 cargo test -p vesta-game-runtime --offline --locked
CARGO_INCREMENTAL=0 cargo clippy -p vesta-game-runtime --all-targets --offline --locked -- -D warnings
```

The first corrected run passed check, **11 tests** (4 private + 7 public), zero failures/ignored,
zero doc tests, and strict Clippy. After independent review-driven additions, a fresh selected
run passed check, **17 tests** (4 private + 13 public), zero failures/ignored and strict Clippy. This was package formatting, not the final workspace format
check. The later fresh integrated run below passed the workspace format check.

Assertion scope includes actual one-byte reader splits at CRLF and 3-/4-byte UTF-8 scalar
boundaries, exact publication fit and every crossing offset, lossily expanded replacement
scalars, bounded undelimited overflow, marker drainage, read failure after a completed line,
interrupted reads, both stream labels, resumable pagination, count/byte retention and both
sides of the gap boundary. The 2 MiB test retains exactly 128 16,384-byte lines from 200,
starting at cursor 73. The repeating-reader fixture has a separate small-buffer byte-for-byte
correctness test. Internal tests exercise cursor exhaustion with unchanged storage, real
mutex poisoning, and snapshot success **before** a controlled blocking reader returns.

An independent Sol reviewer found no concrete production defect but did find a weak Debug
assertion comparing one line with a joined whole-page payload. Parent replaced it with a real
distinctive sentinel and added independently reviewed regressions: decoded expansion below
raw overflow, mid-CRLF/scalar interruptions, owned-reader destruction on success/failure,
multiple byte-budget evictions in one publication, raw snapshot clone independence and
second-stream publication before a blocking reader returns. The exhausted-cursor test now
fills 2 MiB, actually issues `u64::MAX` from `MAX-1`, and verifies unchanged full storage on
subsequent refusal. Two gated nonempty readers establish overlapping invocations, exact local
reports, unique contiguous global cursors and intact ordered stream/text pairs without
claiming a guaranteed interleaving of individual publications. Nonempty-page progression
assertions prevent a pagination regression from silently hanging test loops.
Review was source-only; executed results come from parent-run commands, not reviewer claims.

### Failed attempts retained

The initial broader run compiled and passed the four private tests but failed three public
assertions: replacement-scalar byte rounding, scalar input arithmetic and gap semantics.
Inspection also found the helper's repeating reader dropped partial chunks, its failing reader
failed before delivering the promised data, and gap/no-gap/maximum-page expectations did not
match the actual contract. These fixtures/assertions were corrected, not production behavior.
A subsequent run passed six public tests but failed the remaining wrong oldest-cursor arithmetic
(69 versus actual 73); this was corrected with explicit exact budget/count assertions.
Both sequences stopped at test failure and therefore **did not execute strict Clippy**.
Only the later corrected sequence above is passing evidence.

## Fresh integrated Windows result

After all review-driven source/test changes, the parent executed the four-member workspace:

```sh
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo check --workspace --all-targets --offline --locked
CARGO_INCREMENTAL=0 cargo test --workspace --offline --locked
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --offline --locked -- -D warnings
node --test scripts/requirements-ledger.test.mjs scripts/server-feature-protocol.test.mjs scripts/rust-wrapper-parity.test.mjs
node scripts/rust-wrapper-parity.mjs
node scripts/requirements-ledger.mjs --check
git diff --check
```

All commands passed: **353 Rust tests** including compile-fail doc boundaries, zero failures/
ignored; **159 Node tests**, zero failures/cancelled/skipped; all **222 Java source hashes** and
**370 frozen requirements** validated. Strict Clippy emitted no code warnings. Git emitted existing
LF-to-CRLF configuration notices, not verification failures. The protected launcher manifest's
SHA-256 remained `4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`.
No runtime/operations parity row was promoted: inventory remains 24 migrated, 17 retained Java
plugin, 181 not migrated, 0 verified. Source-only partial accounting remains an explicit later task,
not a whole-behavior completion claim. No source changes followed this final verification.

## Intentionally unfinished

No Minecraft process/pipe, executable launch, stdin, lifecycle binding, generation ownership,
stop/kill/exit observation, independent supervisor/protected IPC, crash continuity, readiness,
authenticated console API or early real-JVM output capture has been implemented/tested here.
Inert readers do not prove a full pipe cannot stall Minecraft or that future process owners
join/drain through actual exit. Empty-line behavior, UTF-8 byte bounds and rejected invalid
pagination are deliberate Rust-native choices, not new normative requirements.

MSRV 1.85, Linux/macOS/other architectures, final whole-rebuild independent review and product
release gates remain unverified. No provider/game execution, EULA acceptance, world changes,
service/firewall/router/signing-security changes, push, PR or publication occurred.
