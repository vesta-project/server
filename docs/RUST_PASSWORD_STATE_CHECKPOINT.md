# Rust password and state-review checkpoint

Date: **2026-10-08**. Executed platform: **native Windows 11**, Rust/Cargo **1.96.0**.
Status: **bounded development increment; full rebuild and final independent review incomplete**.

This extends, rather than replaces, the historical [state checkpoint](RUST_STATE_PROTOCOL_CHECKPOINT.md)
and [canonical identity record](RUST_IDENTITY_API_FREEZE.md). Java source/tests/builds remain
available for useful behavior comparison. Rust ownership, validated values, typed transaction
errors and cohesive domain modules replace Java API/class structure and incidental JDK semantics.
The 222 Java inventory entries and 370 original normative requirements remain frozen;
no original requirement or whole product phase is promoted by this checkpoint.

## Implemented state corrections

- Identity observation, mutations and expiry sweeps sample time inside the established snapshot
  or admitted writer. Waiting for FIFO admission or `BEGIN IMMEDIATE` cannot preserve a
  pre-admission authority timestamp or shorten a newly requested relative grant lifetime.
- Event append rejects embedded NUL. Selected NUL-bearing corrupt rows keep their bounded
  header/cursor, report `payload_chars = -1`, and never project payload text, even when the
  complete byte length happens to fit the requested scan cap. Character and byte guards remain.
- `StateError::new` enforces nonempty `state_` codes for trusted program constants. Caller/native
  SQL, paths and secret text are not retained in errors.
- `SettingsStore::new(db)` constructs its audit log from that database, removing foreign-audit
  composition. Settings mutation and success audit still commit/rollback together.
- Schema **5** adds pending-expiry partial indexes on `(expires_at, id)` for both assignments
  and overrides, excluding revoked/already-recorded history. Tests exercise genuine v4 reopening,
  preserved complete data/audit rows, standalone verified v4 backup, exact index definitions and
  production-shaped query plans without a temporary ordering B-tree. Sweeps process at most
  500 eligible rows per table; 507 + 507 fixtures produce batches 1000, 14, 0 with exactly-once
  markers/audit and deterministic deadline/ID ordering. Policy expiry never depends on a sweep.
  These indexes avoid scanning historical rows, not a universal constant-work proof for arbitrary
  externally corrupted or differently scoped datasets.
- Documentation now states the actual device-revocation rule: only Owner bypasses hierarchy/no-self
  **after** the exact capability/deny check. Administrator has no such exemption.

## Rust-native password boundary

`identity/password/{secret,hash,gate}.rs` supplies bounded primitives. `identity/pack_passwords.rs`
composes them with current canonical policy, one server-local credential and atomic audit.
No listener, management session, identity proof, enrollment, principal or grant is created.
Microsoft/Minecraft remains the expected normal identity flow; pack passwords are alternatives
for pack access, never administrative passwords.

### Owned secret and verifier material

- `SecretPassword` consumes an owned `Vec<u8>` or `String` into `Zeroizing<Vec<u8>>`.
  It requires nonempty valid UTF-8, at most **4096 bytes** and **1024 Unicode scalars**.
  Password bytes, including NUL and surrounding whitespace, are exact: no trimming,
  normalization, surrogate emulation or truncation. No secret Clone/serialization exists;
  Debug is redacted. Public errors retain only fixed material-free codes.
- Argon2id **v19** emits a fresh OS-random **16-byte salt**, **32-byte output**, and canonical
  PHC with new-hash cost **19456 KiB / 2 passes / 1 lane**. Default verification ceilings are
  **65536 KiB / 4 / 4**. Hard configuration bounds are memory **1024..262144 KiB**, passes
  **1..16**, lanes **1..8**, and memory at least eight times the lane count. New cost must fit
  the configured verification ceiling.
- Parsing caps the whole envelope at **256 printable non-space ASCII bytes**; requires exact
  algorithm/version and exactly one each of `m`, `t`, `p`; refuses extra/default/duplicate
  parameters and noncanonical integers/Base64. All six parameter orders are accepted deliberately;
  emission orders `m,t,p`. Decoded salt/output lengths are **16..64 bytes**.
  Every format/cost check precedes KDF-workspace reservation and derivation. Test-only thread-local
  counters directly assert this for the rejection matrix and unauthorized/overflow prechecks.
- Raw Argon2 uses its supported `hash_password_into_with_memory` API with a fallibly reserved,
  caller-owned **`Zeroizing<Vec<Block>>`**. The main heap workspace, raw outputs, decoded fields,
  encoded temporary fields and owned verifier are guarded, rather than assuming the allocating
  Argon2 API wipes its heap. Equal-length outputs use `subtle::ConstantTimeEq`.
- `needs_rehash` is an observation, not automatic rotation/downgrade. A wholly stronger cost is
  not downgraded merely for differing from defaults; weaker components/nondefault lengths can
  be flagged. No handwritten KDF/Base64/comparison or uncontrolled engine injection is used.

Zeroization is best-effort owned-buffer hygiene: it cannot erase caller copies, compiler temporaries,
SQLite's internal copies, historical WAL/backups, swap or crash dumps. Revocation nulls the current
PHC but is **not secure erasure** of past database material. Whole verification is not constant-time.

### Admission and persistence

- Production `PackPasswords::new` privately shares one process gate across factories/databases:
  **2 concurrent KDFs, 60 verification admissions/minute, 10 management admissions/minute**.
  Primitive clones share accounting; isolated primitive gates are not a remote composition seam.
  Rates use integer continuous refill and monotonic time. Admission is nonblocking, rate is charged
  before Busy, buckets are separate and concurrency is shared. Revocation charges management rate
  without requiring a KDF slot. RAII permits cannot be cloned; closing a primitive gate refuses new
  work without pretending active work stopped. Poisoned accounting refuses new admission.
- Set/revoke require the exact current canonical `credentials.manage` policy and expected revision
  both before work and in the final admitted mutation transaction. Deny and active built-in reserved
  membership rules remain. Real derivation occurs outside **every** DB transaction.
- Absence is revision **0**; stored revisions are positive and increment with checked arithmetic.
  Rotation/revocation/reactivation preserve creation time. Repeated revocation retains a tombstone
  and increments rather than resetting reusable creation state. Reopen preserves verifier/revision.
- Verification consumes owned bytes before admission, snapshots the bounded PHC, derives outside
  storage, then compares the complete stored snapshot again in an admitted writer. Rotation,
  revocation or same-revision PHC replacement produces `Changed`, never a stale `Match`.
  Match only leaves the method after its audit **and commit** succeed. Audit contains fixed outcome
  and revision, not password/PHC. No identity facts change and a Viewer still cannot manage credentials.
- PHC reads inspect the complete borrowed `ValueRef::Text` bytes **before copying**. SQLite's
  NUL-truncated `length(TEXT)` constraint cannot admit an unbounded owned copy. Corrupt state fails
  closed. Bounded malformed verifiers are classified/audited, not treated as matches.
- Local Busy/rate refusals do not flood audit. Domain refusals are separately audited only after
  clean rollback/snapshot disposal; refusal-audit failure preserves the exact primary rejection.
  Infrastructure/rollback/commit failures remain errors rather than successful refusal or match.

## Verification

After the final review-driven assertion corrections, a fresh sequential native Windows run passed:

```sh
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo check --workspace --all-targets --offline --locked
CARGO_INCREMENTAL=0 cargo test --workspace --offline --locked
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --offline --locked -- -D warnings
node --test scripts/requirements-ledger.test.mjs scripts/server-feature-protocol.test.mjs scripts/rust-wrapper-parity.test.mjs
node scripts/rust-wrapper-parity.mjs
node scripts/requirements-ledger.mjs --check
```

**237 Rust tests passed, zero failed/ignored**: 146 control unit tests (109 existing canonical
identity, 22 password primitive/contract, 15 pack-password application), 3 control integration,
29 protocol integration, 1 state unit, 56 state integration and 2 compile-fail doctests.
**159 Node tests passed, zero failed/skipped/cancelled**. Inventory and requirements checks passed:
**222 Java files / hashes and 370 frozen normative requirements**. Accounting now has **21 migrated,
17 retained Java plugin, 184 not migrated, 0 verified** rows. The seven new bounded password mappings
are not whole-product equivalence; executable runtime composition remains unmigrated. No original
requirement status, whole-platform gate or final-review status was promoted.

Maintained assertion families include independent retained Bun ASCII/UTF-8 Argon2 fixtures,
production costs/fresh salts, full PHC/cost/Base64 matrices, Unicode/byte bounds, exact NUL/whitespace,
concurrent hasher reuse, deterministic admission/refill/poison/unwind tests, real SQLite lifecycle,
post-KDF authority changes, held-writer expiry with clock advancement, final revision conflict,
audit-insert failure, **deferred-constraint COMMIT failure after successful audit insertion**,
refusal-audit failure, stale matches, full-byte NUL-tail storage bounds, reopen and process-wide factory
sharing. The held-writer observation windows detect early sampling when the requester is scheduled;
they are not scheduler-fairness proofs. Workers are released/joined before result assertions.

Failed development runs remain failures: timing-test result conversion; migration audit tuple beyond
standard trait support; a batch-test generic delimiter; unused imports/fields, derivable Default and
tuple-complexity Clippy issues; and an extra test's `at`/`at_ms` field typo. They were corrected without
weakening behavior assertions, disabling tests or suppressing strict lint rules.

## Independent review and remaining gaps

An independent **GPT-6.1 Sol scoped checkpoint source review** found no verified production defect.
Its follow-up found a concrete weak permit-release assertion and gaps in commit/refusal audit faults,
queued expiry, factory sharing, direct pre-KDF evidence and stale-match/no-authority assertions.
Parent strengthened those regressions and reran them. This is not the mandatory final full-rebuild
review; that remains pending until the actual rebuild is complete.

Still unverified/not implemented: deterministic real RNG/KDF/allocation failure execution; simultaneous
same-revision rotation stress beyond the controlled conflicting-create/interleaving cases; per-IP/
per-principal transport controls and remotely configurable composition; real authenticated Microsoft/
Minecraft adapters, device-bound pack grants/rotation revocation of those grants, mTLS/session/vault/
recovery-passphrase integration, authenticated management routes and executable runtime composition.
No fake fault injector, proof, session, grant or stub agent is used to claim these gates closed.

Linux/macOS/other architectures and declared Rust **1.85 MSRV execution** remain unverified.
Dependency pins/features/metadata are recorded in [dependency decisions](DEPENDENCY_DECISIONS.md),
not provenance/vulnerability/SBOM/licence-packaging certification. Native ACL/path checks remain
same-account storage hygiene, not independent game confinement or race-free filesystem resolution.
Unicode resource scopes, durable operations/runner/replay, game/runtime, installer extraction,
independent supervisor/IPC, activation/onboarding and signing/distribution remain unfinished.

No commit/push/PR/publication, real provider/Minecraft execution, EULA acceptance, world/demo erasure,
service/firewall/router/public-exposure change, signing/security-setting change or automatic restore
occurred. The protected launcher manifest is unchanged. This increment is not secure-product,
complete-migration, public-deployment or release-readiness approval.
