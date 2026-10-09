# Canonical operation authority and Argon2id checkpoint

Assessment date: **2026-10-04**. This extends the historical [execution/identity checkpoint](EXECUTION_IDENTITY_CHECKPOINT.md). It is a reviewed domain integration, **not a remotely usable or completed secure hosting product**. Database schema remains **3**; no new migration or network endpoint was introduced.

The companion launcher manual-import implementation is committed as **`37fab23c`** and assessed in `launcher-server-mvp:docs/VESTA_MANUAL_IMPORT_CHECKPOINT.md`. Progress commits are unsigned under explicit user authorization. Nothing has been pushed or deployed.

## Canonical typed submission and execution authority

New production classes in `wrapper/.../operations/`:

- `OperationBinding<I>` binds a trusted handler/type to canonical input, target, exact capability/resource requirements, resource locks and the handler's idempotency/confirmation contract.
- `OperationBindings` supplies both immutable binding lookup and a `HandlerRegistry` containing exactly those bound handlers.
- `OperationSubmissionService` accepts a transport-authenticated `RemoteActor`, audit source label, type, raw typed input and optional idempotency key. It derives security fields from trusted code, not request-supplied targets/locks/safety flags.
- `PolicyOperationAuthority` implements runner authority against stored canonical input and freshly resolved canonical identity.
- Internal `OperationPolicy` shares fail-closed exact-capability evaluation.

### Review-enforced boundaries

1. Canonical input must parse and serialize stably within the existing 65,536-character input limit. **Target, requirements and locks are derived from the persisted canonical reparse.** Each must also equal the corresponding original-input derivation; a lossy canonicalization cannot silently change authority or lock semantics. Requirements are nonempty and limited to 32, with exact capabilities and bounded resources. Sorted lock resources retain the existing limit of 32.
2. Requirements name concrete scoped paths, never wildcards; unscoped capabilities use an empty resource. Scope matching remains the authorization evaluator's job. This is not native filesystem confinement.
3. Submission resolves current identity/policy using the **same supplied SQL connection and transaction** that checks idempotency and inserts the operation/audit/event. Authority is checked even before returning an existing idempotent result. No stale authorization permits retrieving/reusing an operation after revocation.
4. Accepted mutations and their audit commit together. Known denials, invalid derivations, unknown types and idempotency conflicts append a bounded rejection audit; rejected submissions create no operation. Audit failure fails the submission and rolls back. Raw requested input is not placed in these audit records.
5. Rechecks deny null stages, wrong-server/unbound operations, unavailable or corrupt stored input, noncanonical input, target/lock/contract mismatch and mismatched snapshot-versus-stored payload hashes. They resolve current principal/device/role/override/expiry policy at CLAIM, BEFORE_RUN and BEFORE_EXTERNAL_EFFECT.
6. Exact deny wins even for Owner. Lower-rank imported grants cannot substitute for reserved Owner/Administrator membership, and local-only powers remain denied remotely. `loopback`, `op`, `local` and other source labels do not create authority. System actors do not resolve to canonical human identity and are denied by this adapter.
7. `OperationService.submit` remains an **unguarded trusted in-process API**, explicitly documented as such. Future user/remote callers must use `OperationSubmissionService`, and future transport must obtain actor IDs from authenticated device possession, never request bodies/headers or network location.

`StateDatabase` now rejects same-thread nested read/write calls from callbacks (`nested_call`; write-inside-write retains `nested_transaction`). This prevents cross-connection snapshots and read-to-write self-deadlock. Callbacks must use their supplied connection. The lifecycle lock, anchor connection and close semantics remain unchanged.

Trusted handler/parser/derivation code must be deterministic and side-effect free during binding, and must declare every real external effect through `beginExternalEffect()`. The adapter cannot detect a badly coded binding that underclaims its required permission. Revocation is checked at declared boundaries, not continuously during an already executing external effect. No production handlers, runtime registrations or authenticated HTTP operation API are wired yet. System/scheduled operations need separately reviewed authority. Existing confirmations still need current-policy composition and authoritative source-revision plumbing at the user-facing boundary.

## Argon2id password primitive

`PasswordHasher` and `PasswordHashException` provide bounded human-password hashing/verification using the existing BouncyCastle dependency, **not a login endpoint, credential store, session service or encrypted vault**.

- Argon2id version 19 (`0x13`), production defaults **19,456 KiB / 2 passes / 1 lane**; fresh 16-byte random salts and 32-byte derived hashes.
- Default stored-hash verification ceiling: **65,536 KiB / 4 passes / 4 lanes**. Absolute configurable ceiling: 262,144 KiB / 16 passes / 8 lanes. The 1,024 KiB / 1-pass hard minimum is test-speed/legacy acceptance only, not production guidance; weak stored hashes are flagged for rehash.
- Strict canonical PHC encoding: exact algorithm/version, ordered `m,t,p`, bounded nonnegative decimal form without signs/leading zeros, canonical unpadded standard Base64, no duplicate/extra/trailing fields. Encoded hashes are capped at 256 printable ASCII characters and salt/hash lengths at 16–64 bytes.
- Invalid/out-of-limit hashes reject **before KDF or large cost-dependent allocation**; bounded parser allocations still occur. No plaintext/legacy/other-algorithm fallback. Verification uses constant-time derived-byte comparison.
- Passwords are nonempty, at most 1,024 UTF-16 characters, strictly encoded to at most 4,096 UTF-8 bytes without creating a password `String`. Malformed surrogates reject. No implicit normalization or truncation.
- The caller retains and should clear their `char[]`. The encoder uses an explicitly owned fixed buffer cleared on all exits, including malformed input. Owned password/derived buffers clear in `finally`; RNG generation is within that boundary. Argon2 parameters clear in `finally`. Java/GC/library internals cannot guarantee erasure of every secret copy.
- Validation exceptions contain fixed codes/messages and no supplied password/hash/salt or causes. `needsRehash` parses and compares costs/sizes without a KDF. Immutable instances can be shared across callers, but **thread safety is not a concurrency budget**.

Independent public reference fixtures in the parent test were generated with **Bun 1.3.14 `password.hashSync`**, Argon2id at the production default costs, for ASCII and UTF-8 passwords. BouncyCastle verifies both. These are public test fixtures, not real credentials. No backup-engine format or native KDF was changed.

Remaining integration: credential persistence/rotation/principal binding, recovery passphrases, local bootstrap/login/session flows, rate/concurrency limiting, secret vault and credential migration. An accepted attempt can allocate up to the configured KDF ceiling; callers must bound concurrent attempts before any login integration. No device possession/mTLS or production OS-local proof follows from this primitive.

## Parent-authored verification

Final fresh server command, after production review fixes:

```sh
./gradlew :core:test :wrapper:test --rerun-tasks --offline --console=plain
```

**366 tests / 44 suites passed**: core 78 / 20, wrapper 288 / 24; **zero failures, errors or skipped tests** in fresh XML.

New parent-owned suites:

- `operations/PolicyOperationAuthorityTest.java`: **30 cases** against actual SQLite and canonical identities established by the real bootstrap/role service with a test-only local-proof verifier. Covers canonical input/target/locks/contracts, all-capability enforcement, explicit denies/scopes/reserved tiers, current-authority idempotency behavior/conflicts, unknown/invalid requests, loopback-label/system denial, device revocation/disabled principal/role expiry/device mismatch, corrupt/unavailable/unbound/wrong-server rows, null stage/snapshot hash mismatch, audit-failure rollback, nested SQL calls, a real runner's successful effect and revocation before an effect, and three distinct target/requirements/lock semantic round-trip regressions.
- `identity/PasswordHasherTest.java`: **43 cases**, covering production defaults/fresh salts/correct-wrong passwords, independent Bun ASCII/UTF-8 fixtures, weak/strong rehash policy, maximum UTF-8 input/no normalization, five invalid-password variants, 32 malformed/unsupported/cost/Base64 PHC variants, hard policy bounds/verification limits/fixed secret-free validation errors and 12 concurrent tasks using one hasher.

Parent review caught the canonical-meaning discrepancy, null-stage/snapshot-hash omissions and an encoder-owned secret-buffer cleanup gap; Opus fixed those production boundaries. The final small RNG/finally cleanup edit was made by the parent after Opus again hit HTTP 429. No production agent wrote repository tests or committed independently. An initial parent assertion had ambiguous JUnit overload inference; it was corrected, and complete fresh suites passed afterward.

Companion checks already completed: **608 Rust workspace tests passed / 7 pre-existing ignored**; **459 frontend tests / 75 files passed**; frontend typecheck, localization and production build passed. The launcher document records the real initial disk-exhaustion failure, external build-storage workaround, native-link coverage and seven ignored cases.

The requirement ledger now records **51 partially verified / 319 not verified / zero fully verified** of 370 normative bullets. Added password/manual-import evidence remains explicitly narrow; existing authority/operation entries gain domain regressions, not a full promotion. All five ledger consistency tests and `--check` passed for this checkpoint. Historical assessments remain dated rather than rewritten as current completion claims.

## Product and release gates remain

- Authenticated device/mTLS transport, production local OS proof, vault/credentials/sessions/invitations/abuse budgets, legacy-secret migration and removal of OP/connector/loopback/raw-TCP escalation.
- Versioned route/OpenAPI/body/error/pagination/event/idempotency contracts, real operation handlers/runtime/settings/retention, authoritative confirmations and current-policy composition of all user-facing mutations.
- Independent supervisor/protected IPC, separate agent/game OS identities and game continuity across agent-only shutdown/crash/restart. **The current wrapper still stops Minecraft on shutdown.**
- Native filesystem confinement and budgets; signed pack workspace/publication/trusted sync; backup/recovery; workflows and the remaining P4–P8 product scope; complete modern launcher hosting/onboarding and capability-gated management UI.
- Real Minecraft/loaders, Linux and clean Windows services/installers, signed distribution, public reachability/proxy defenses and independent security review. No such deployment/platform release run occurred here.

The legacy management trust model is still unsafe to expose. Passing domain primitives and local import tests is not authorization to install/deploy/publish a completed server product. Resume in dependency order from the reviewed commits and explicit gates; Opus rate exhaustion is a resource boundary, not product completion.
