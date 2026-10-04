# Durable execution and local identity checkpoint

Assessment date: **2026-10-04**. Reviewed implementation and tests are committed in **`6052229`**. This extends the historical first checkpoint in `IMPLEMENTATION_PROGRESS.md`; it is not a release or full P1/P2 declaration. The 370-bullet requirement ledger now has **47 partially verified, 323 not verified, zero fully verified**; its five consistency tests and `--check` pass. Commits remain unsigned under the user's authorization; nothing has been pushed.

**Historical assessment:** the later [canonical-authority/password checkpoint](OPERATION_AUTHORITY_PASSWORD_CHECKPOINT.md) records the implemented canonical submission/authority adapter, Argon2id primitive and newer verification. Gaps and counts below describe `6052229`, not a current assessment of those subsequently addressed domain gaps.

## Implemented and reviewed

### Persistent inputs and execution

- Central transactional migrations advance the database to schema **3**: version 2 adds bounded canonical operation inputs and private claim ownership; version 3 composes the identity schema. Existing databases receive a verified online pre-migration backup. Hash-only legacy rows never get fabricated inputs or execute; safe rows fail explicitly, while potentially completed non-idempotent effects retain recovery-required state and locks.
- Operation inputs are retained privately, limited to 65,536 characters, and excluded from operation DTOs and audit/event records. They must be secret-free. Terminal-input retention preserves idempotency identity; deletion and its bounded audit commit atomically. No runtime retention schedule exists yet.
- Typed handlers register explicit input, resource, idempotency, confirmation, and cancellation contracts. Unknown handlers, invalid inputs, mismatched contracts/resources, and unavailable authority fail before effects. There is no default-allow authority implementation.
- Current authority is rechecked after claim, after potentially blocking preparation, and immediately before each handler-declared external effect. Production handlers must call `beginExternalEffect()` before **every** effect and cooperate at safe points; this is a trusted-code contract, not a sandbox.
- Per-claim private owner tokens guard runner transitions. A protected cross-process file lock must be bound to the actual database directory. Startup recovery runs only after exclusive ownership. Shutdown retains ownership and operation locks until uncooperative handler threads truly exit; it cannot forcibly terminate Java handler code.
- Unknown non-idempotent outcomes require explicit recovery, rather than automatic replay or rollback. Audit/state failure stops further claims.

### SQLite lifecycle

- An idle autocommit anchor connection keeps WAL/SHM companions alive while per-call connections open/close. This fixes a real Windows `AccessDeniedException` race caused by SQLite deleting the last connection's SHM while another caller checked its attributes. No file protection was relaxed and no exception was ignored.
- A fair lifecycle read/write lock makes close wait for active callbacks. Calls after close reject; close is idempotent; close from inside a callback rejects instead of deadlocking. Existing query-only snapshots, fair serialized writes, nested-write rejection, foreign keys, integrity checks, and protected-file checks remain.

### Server-local identity and authorization

- Canonical server-scoped principals, device fingerprints, six built-in roles, role assignments, overrides, local-proof replay records, and hashed single-use bootstrap/recovery codes persist in SQLite. Current service behavior is one primary role plus overrides; the schema permits future multi-role composition.
- `LocalAuthority.UNAVAILABLE` denies bootstrap/recovery/local-role grants by default. A supplied verifier must prove OS-authorized local access, not accept a Boolean, network location, OP status, or connector response. Attestations bind purpose, nonce, OS actor, issue/expiry times, a strictly positive lifetime of at most two minutes, and single use. **No production OS-proof verifier is implemented.**
- Bootstrap/recovery codes default to ten minutes, permit only one to sixty minutes, rotate older codes, and redeem atomically to one principal/device/Owner assignment. Fingerprints are canonical 64-character lowercase SHA-256 hex. Recording a fingerprint does **not** prove device-key possession.
- Named exact capabilities have no grouped or wildcard fallback. Policy resolves current principal/device/server binding, revocation, active roles, overrides, and expiry on each call. Explicit deny wins, including over Owner. Path allows are case-sensitive; denies match case-insensitively. Malformed stored allows drop; malformed denies fail closed.
- Owner/Administrator grants are local-only. Remote delegation requires the exact delegation capability, strictly lower target/role rank, and capabilities/scopes held by the delegator. Reserved recovery/restore powers require actual active built-in Owner membership; credentials/security powers require actual built-in Owner/Administrator membership. Rank alone and imported custom/override grants cannot confer these tiers.
- Device revocation requires its exact capability and respects denies. Built-in Owner may then revoke any device; other holders may revoke only strictly lower-ranked principals' devices, not their own or equal-ranked devices.
- Successful identity mutations and audit commit together. Refusals are audited after rollback; if that audit fails, `audit_unavailable` is reported rather than silently dropping the refusal. Expiry applies immediately; a bounded sweep records audit events once. Notifications are not implemented.
- Built-in role/capability mutation triggers check both OLD and NEW targets, preventing custom rows from being promoted or moved into immutable built-ins. These triggers are not protection against an OS administrator replacing the database.

## Parent-authored verification

Fresh command on Windows 11, 2026-10-04:

```sh
./gradlew :core:test :wrapper:test --rerun-tasks --offline --console=plain
```

**293 tests / 42 suites passed**: core 78 / 20, wrapper 215 / 22. Zero failures, errors, or skipped tests in fresh XML. All new repository tests were written by the parent reviewer, not production programmers.

New suites:

- `wrapper/src/test/java/com/vesta/wrapper/operations/OperationRunnerTest.java`: **20 cases**, including contract rejection, authority-denial/exception/null decisions, revocation during blocked preparation, effect recovery, safe cancellation, cooperative/uncooperative shutdown, audit failure, owner tokens/redaction, oversized results, and lock-directory binding.
- `wrapper/src/test/java/com/vesta/wrapper/state/OperationInputMigrationTest.java`: **5 cases**, including real v1/v2 upgrades, verified old-schema backups, missing legacy input, reopen, retention/idempotency, and audit-failure rollback.
- `wrapper/src/test/java/com/vesta/wrapper/state/StateDatabaseLifecycleTest.java`: **3 cases**, including six-thread mixed reads/audited writes over three reopen rounds (540 committed increments), stable WAL/SHM companions, close waiting for an in-flight snapshot, and nested-close rejection.
- `wrapper/src/test/java/com/vesta/wrapper/identity/IdentityAuthorizationTest.java`: **41 cases**, including all six role/capability matrices, bootstrap/recovery proof validation/replay/expiry/rotation/concurrent redemption, fingerprint canonicalization, secret redaction, audit rollback/refusal failure, malformed/imported authority, wrong-server/device binding, scopes, strict delegation/device hierarchy, primary-role replacement, expiry sweep, and immutable built-in SQL targets.

The first new focused runner test exposed the SHM race; production was fixed and full regression tests passed afterward. The first identity run had one incorrect test expectation for a backslash path alias; the shared policy intentionally normalizes it, and the corrected test checks its canonical form without relaxing production validation.

## Explicit remaining gates

- No production operation handlers, canonical `OperationAuthority` adapter, runner/runtime composition, versioned HTTP operation API, event transport, or settings/retention scheduler is wired yet.
- No authenticated transport, mTLS/device possession, invitations/sessions, Argon2id credentials, OS-protected vault, abuse/rate limits, production local-proof verifier, or credential migration. `RemoteActor` IDs are lookup keys only; future authenticated transport must supply them.
- No independent supervisor/IPC or separate game/agent OS identities. Agent shutdown still stops Minecraft. No Linux/service/real-Minecraft release checks were run.
- Lexical path scopes do not provide native confinement. Existing filesystem TOCTOU/hard-link/reparse limits, same-user plugin risks, and audit history truncation/replacement without external checkpoints remain.
- Existing connector/OP/loopback/raw-TCP authority, CORS, request-bound, plaintext/default-secret, and installation/distribution defects listed in `IMPLEMENTATION_PROGRESS.md` are not replaced by these domain classes. **Do not expose this MVP as a completed secure product.**
- The launcher manual-import wave is assessed separately after its own tests; this server checkpoint does not certify UI, pairing, publication, trusted sync, signatures, or provider installation.

Requirement evidence remains **partial**, even where a focused domain assertion passes. No requirement is promoted to fully verified. Continue P1/P2 composition and authenticated transport before opening new remote management authority, then independent supervision and the remaining dependency-ordered product work.
