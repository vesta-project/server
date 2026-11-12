# Rust state and descriptive-protocol checkpoint

Date: **2026-10-07**. Platform: **Windows 11**, native Rust/Cargo **1.96.0**.
Status: **bounded foundation checkpoint; migration and final independent review incomplete**.

This records executed Windows evidence for the two admitted workspace members,
`vesta-protocol` and `vesta-state`, before the current storage/identity redesign. Java
remains a reference for useful behavior and safety, not a source-to-source or class/method
blueprint. This is not complete behavioral/security accounting, a replacement requirement
ledger, a release assessment, or evidence for unfinished `vesta-control` sources.
The original **370** requirements and their statuses remain unchanged. Java wrapper/core,
Gradle builds and Minecraft plugin/mod remain available as behavioral references.

Owner direction (2026-10-07): design the Rust equivalent for all intended platform
compatibilities; tests run on Windows only in the current increment. The 70-test result
below is real historical foundation evidence, but it predates the redesigned state
transaction/error boundary and canonical identity work and does not verify them. No new
build/test, platform pass, crate admission or requirement promotion is recorded here.
Linux, macOS and non-Windows architecture/native behavior remain unexecuted; CI definitions
on disk are prospective coverage, not executed evidence.

## Fresh verification

Run from the server repository (Windows Git Bash):

```sh
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo check --workspace --all-targets --offline --locked
CARGO_INCREMENTAL=0 cargo test --workspace --offline --locked
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --offline --locked -- -D warnings
```

All four commands completed successfully in one fresh sequential invocation after the
corrections below. Tests: **70 passed, zero failed/ignored**; no unit/doc tests were
present beyond these integration targets. Compilation and Clippy do not establish
unexecuted platform behavior, dependency provenance or a minimum-supported-Rust guarantee.

| Integration target | Passed | Assertion scope |
| --- | ---: | --- |
| `vesta-protocol/tests/discovery.rs` | 13 | Existing independent corpus/goldens; strict decoding, bounds, ordering, negotiation, fixed errors, known-only support. Corpus rows are iterated inside a test, not additional test-count claims. |
| `vesta-protocol/tests/discovery_adversarial.rs` | 4 | Surrogate/error precedence, invalid public values, raw controls/deep containers, oversize-before-UTF-8 precedence. |
| `vesta-protocol/tests/pack_index.rs` | 9 | Unchanged Java semantic index fixture, UTF-16 sorting/input preservation, optional fields, known/unknown source and loader projection, signed limits and fixed null-path rejection. |
| `vesta-protocol/tests/pack_adversarial.rs` | 3 | Stable duplicate-path ordering, distinct input/output source wire names and the explicit lack of path/digest admission in descriptive projection. |
| `vesta-state/tests/database.rs` | 9 | Actual SQLite/WAL/FK/FULL/timeout, stable UUID, rollback/nesting, verified online backup, corruption/newer-schema rejection, real junction/path/permission refusals. |
| `vesta-state/tests/database_lifecycle.rs` | 3 | Concurrent audited connections/reopen and stable companions, close draining an admitted snapshot, callback-close rejection. |
| `vesta-state/tests/audit_events_settings.rs` | 9 | Append-only chain/tamper detection, mutation/audit atomicity, bounded safe metadata, event pruning/gaps/clock rollback, explicit desired/effective acknowledgement and audit-failure rollback. |
| `vesta-state/tests/audit_vectors.rs` | 2 | Eleven independent Java/Gson byte/input/hash vectors, UTF-16 bounds and fixed input-free errors. |
| `vesta-state/tests/event_bounded_scan.rs` | 8 | Snapshot bounds, exclusive/inclusive cursor range, selected-type SQL payload caps, bounded headers, invalid requests and trusted-read compatibility. |
| `vesta-state/tests/operation_input_migration.rs` | 2 | Genuine legacy v1/v2 schema fixtures, preserved hash-only rows/locks/UUID and standalone verified backup; direct-SQL input-erasure/audit fault rollback, **not an operation-service implementation**. |
| `vesta-state/tests/pack_password_migration.rs` | 4 | Schema-three fixture preservation/backup, v4 singleton/FK/strict/tombstone constraints, real migration rollback and no-clobber backup failure. |
| `vesta-state/tests/state_adversarial.rs` | 4 | Corrupt NUL payload byte-bound regression, UTF-16 append/multibyte projection, pre-established read snapshot alongside a WAL writer, lifetime advisory runner lock without truncation/unlinking. |

## Independent contracts and intentional differences

- The persisted `vesta-audit-v1` chain is retained as a narrow byte-format compatibility
  contract, not a general mandate to emulate Java. Its frozen vectors in
  `protocol/audit-v1/fixtures/vectors.json` use actual Gson **2.11.0**;
  `scripts/AuditHashOracle.java --verify-reference` checked compiled Java reference methods,
  and the previously executed comparison matched all **11 vectors byte for byte**. Those
  cases cover the existing format's escaping, nulls, nullable operation ID, metadata-key
  ordering and signed millisecond bounds. The metadata JSON remains a string inside the
  hash-field array, not a nested object. No unrelated Java/JDK behavior is thereby required.
- Discovery uses the existing independent Node/shared JSON corpus. Unknown valid feature
  identifiers round-trip as inert observations; they never authorize dispatch.
- SQLite `length(TEXT)` stops at NUL. The Rust bounded event scan additionally checks
  `length(CAST(payload_json AS BLOB)) <= max_payload_chars * 4` before projecting a selected
  payload, and caps requested character limits at 4,096. A huge corrupt NUL-containing
  selected row now returns only its bounded header/cursor, not the hidden payload tail.
  These are intentional stronger bounds than the legacy helper, not silent equivalence.
- Pack projection recognizes compiled ASCII loader/provider identifiers locale-independently.
  The legacy Java builder's default-locale lowercase behavior is incidental and is not a Rust
  compatibility target; ordinary compiled IDs and the retained golden fixture are tested.
  Projection copies supplied paths, sizes, hashes and references; it does not validate them
  for disk access, fetching or installation. A separate protected host boundary is required.
- The Java golden pack test compares parsed semantic JSON, **not deterministic ZIP bytes**.
  Java's builder does not freeze ZIP timestamps. No Rust ZIP writer, protected embedded-file
  resolver, pack publication, signature verification or authenticated route is established.

## Corrections and failed runs retained as history

Interrupted worker source was inspected before resuming; existence was not treated as
completion. The first recovered full verification stopped at formatting differences in two
older protocol tests; formatting was applied only inside the admitted server workspace.
The next invocation passed check and all 70 tests, but strict all-target Clippy failed on
seven unnecessary test-path borrows. Those borrows were corrected without changing assertions;
the fresh four-command invocation above then passed. Failed invocations are not passing evidence.

Earlier state-foundation corrections included real Windows junction argv quoting, explicit
foreign-key disable/assertion for a deliberately corrupt SQLite fixture, and checking lock-file
contents after releasing a Windows byte-range lock rather than reading through it. No native
link cases were ignored, and the runner-lock test still checks exact preserved bytes and reuse.
These are historical results for the tested foundation, not verification of redesigned state/control.

## Explicit migration-test gaps

Operation-input coverage remains limited to two schema/database integration tests; they do
not implement or verify the operation-domain behavior represented by the former Java tests.
Unverified behavior includes private bounded-input persistence across reopen and exclusion
from public operation/audit views; exact idempotency binding; terminal-only/time-bounded
retention with tombstones and preserved hashes; keeping queued inputs; refusing execution
when legacy rows have no canonical input; and safe restart recovery of legacy states while
preserving resource locks. The audit-trigger case uses direct SQL and a real database
transaction to prove rollback of the erasure statement, but it does not establish a domain
service's purge/audit contract. No fake service or test-only operation implementation is
used to claim these gaps closed.

The schema fixtures are independently authored SQLite test data. They do not prove every
production database variant or concurrent Java/Rust production compatibility. Java source
and tests remain behavioral reference material, not required Rust API signatures.

## 2026-10-08 review-fix note — later evidence recorded separately

The following state-layer corrections are not verified by the historical 2026-10-07 runs above.
Fresh native Windows evidence for them and the password increment is recorded in
[the dated password/state checkpoint](RUST_PASSWORD_STATE_CHECKPOINT.md):

- Event payload writes reject NUL-containing JSON text. Bounded event scans recognize a corrupt
  NUL-payload marker (`-1`) and enforce the payload **byte** bound before projecting selected rows;
  SQLite `length(TEXT)` alone is not a sufficient bound because it stops at NUL.
- `SettingsStore::new(db) -> Result<_>` derives its audit store from that same database handle, so
  settings mutation and its audit cannot be accidentally composed over different database instances.
- The state error namespace is now complete: all `StateError` codes carry the `state_` prefix. The
  settings-store constructor enforces the same-database/audit invariant rather than accepting a
  separately supplied audit store.
- Schema version 5 adds partial expiry indexes for bounded expiry work. This is a schema change, not
  post-fix migration or runtime verification evidence.

The 70-test 2026-10-07 run remains historical foundation evidence, not the post-fix result.
The linked later checkpoint supplies the new execution scope; neither record promotes original
requirements or establishes unexecuted platforms, MSRV or the pending final full-rebuild Sol review.

## Platform, composition and release gates

- The executed Windows foundation used real temporary SQLite, native junctions/ACL checks and
  advisory locks. Owner-only ACL/path checks are same-account hygiene, not game isolation or
  TOCTOU certification. Advisory runner locking does not establish independent supervisor IPC.
  These observations do not verify the redesigned state/control implementation.
- The design target is all intended supported platform compatibilities, while this increment's
  tests run on Windows only. **Linux, macOS and non-Windows architecture/native execution remain
  pending**; installed WSL is not a Rust test result. The Windows/Linux CI definitions on disk
  are prospective, unpublished/unrun coverage and satisfy no platform gate by existing alone.
- `rust-version = 1.85` is a declared workspace floor, **not verified MSRV evidence**. CI selects
  the actually exercised development toolchain, 1.96.0, without claiming MSRV coverage.
- Pending product behavior includes queued database maintenance, complete configuration
  application adapters/API, audit export/checkpoints/retention/capacity, authorized replay,
  independent supervisor, OS-authenticated proof, device transport/session/vault, canonical
  remote handlers, complete host pack behavior and durable installer/activation composition.
- The Rust-native transaction/error boundary and canonical identity design are not verified by
  this historical checkpoint. The approved identity direction uses typed domain commands and
  conservative portable ASCII path scopes; unsupported stored allows cannot grant, unsupported
  denies fail closed, and Unicode file-scope support remains an explicit gap. Preserve the
  persisted audit-v1 chain bytes only as a narrow storage-format contract, not general JDK
  emulation. The mandatory final independent GPT-6.1 Sol review remains pending; no status is
  promoted here.

The protected launcher manifest remains untouched. No commit, push, PR, publication, real
provider/Minecraft execution, EULA acceptance, service/firewall/router changes, signing,
world/demo erasure or automatic restore occurred. No original requirement status is promoted.
