# Server software implementation checkpoint

Assessment date: **2026-10-04**. This is the historical **first** partial engineering checkpoint, not a release declaration. See [the execution/identity checkpoint](EXECUTION_IDENTITY_CHECKPOINT.md) for the subsequent schema-v3, runner, authorization, and 293-test assessment, then [the canonical-authority/password checkpoint](OPERATION_AUTHORITY_PASSWORD_CHECKPOINT.md) for the 366-test backend assessment and companion launcher manual-import integration. See [the durable lifecycle checkpoint](DURABLE_LIFECYCLE_CHECKPOINT.md) for the later opt-in real-process composition, canonical confirmation gateway and companion import-budget work. The subsequent [pack-password credential checkpoint](PACK_PASSWORD_CREDENTIAL_CHECKPOINT.md) records schema-4 verifier persistence, canonical administrative checks, bounded KDF work and the preserved Microsoft/Minecraft identity direction. The latest [operation event replay checkpoint](OPERATION_EVENT_REPLAY_CHECKPOINT.md) records the in-process, canonical-authorized, bounded replay of sanitized operation events (579-test fresh suite) and its remaining transport/API gates. The [launch definition checkpoint](LAUNCH_DEFINITION_CHECKPOINT.md) records the typed, shell-free `java-jar`/`java-argfile`/`native` launch definition and the unsupported-stop rejection (687-test fresh suite); it adds no installer or provider. The subsequent [server-feature protocol checkpoint](SERVER_FEATURE_PROTOCOL_CHECKPOINT.md) records draft-v1, transport-neutral discovery/negotiation with strict Java and independent Node codecs; no listener, action API or permission is added. The [automatic JAR-provider checkpoint](AUTOMATIC_JAR_PROVIDER_CHECKPOINT.md) records the companion launcher's callable Rust backend for local Vanilla/Paper/Folia/Purpur installation (exact reviewed plans, bounded verified transfers, caller-selected Java, no-clobber bundles and receipts); loaders, managed Java, UI/headless wiring and remote installation remain open. Counts and unresolved implementation statements below describe the first checkpoint; the current requirement ledger contains later partial evidence.

**2026-10-06 follow-up:** the automatic-provider checkpoint now includes the Linux mount-capacity/OpenJ9 probe review fixes. The [loader processor context checkpoint](LOADER_PROCESSOR_CONTEXT_CHECKPOINT.md) adds explicit Client/Server data selection and client regressions only; all four loader installation backends remain unavailable, with bounded server execution and recipes still open.

**2026-10-06 bounded execution foundation:** the [bounded server execution checkpoint](BOUNDED_SERVER_EXECUTION_CHECKPOINT.md) maps the companion launcher's private owned-child and declared-file primitives to the original requirements. Production use remains the selected-Java probe; prepared jobs are inert fixtures only. Loader recipes, native confinement and durable operation/reconciliation composition remain open. No requirement status or phase is promoted.

**2026-10-06 Fabric/Quilt servers:** the [Fabric/Quilt server bundle checkpoint](FABRIC_QUILT_SERVER_BUNDLE_CHECKPOINT.md) records download-only Fabric/Quilt installation in the companion launcher. No requirement status or phase is promoted; independent review findings are fixed.

## Scope and authority

The accepted baseline is `SERVER_SOFTWARE_REQUIREMENTS.md`, with sequencing in `SERVER_SOFTWARE_IMPLEMENTATION_PLAN.md`. The user's accepted scope also includes complete modern launcher hosting/onboarding and capability-gated management UI. That UI is **not implemented in this checkpoint**.

The dated original roadmap Markdown/JSON/HTML are preserved rather than promoting their historical claims. This document and `REQUIREMENT_TRACEABILITY.json` provide the new narrow assessment. The ledger indexes **370 normative bullets**, not every prose statement or each clause within compound bullets: **26 partially verified, 344 not verified, zero fully verified** at this checkpoint. `partially_verified` means only the explicitly recorded assertions passed; it never means the whole requirement is complete. Unmapped entries remain `not_verified`. No requirement is marked fully verified by this batch.

Work branches:

- Server: `feat/server-software-foundation`, based on preserved MVP `6dd558e`.
- Launcher: `feat/server-software-integration`, fast-forwarded to upstream `ecf498fa` before finishing this batch. The obsolete MVP branch, other worktrees, and stashes were preserved.
- Commits are unsigned under explicit user authorization, using per-command signing override. Nothing has been pushed. A later approved signing/squash/rewrite changes commit identities; do not publish first and assume signing can be added without rewriting.

## Tested checkpoints

| Server commit | Narrow scope |
| --- | --- |
| `cbac6a3` | Preserve accepted requirements, plan, roadmap, and status documents. |
| `85a6f5c` | Shared file/content protection, custom TLS exclusion, bounded output, strict console lines, and non-destructive stop timeout/restart refusal. |
| `a1ca0e1` | SQLite durable-state, operation, confirmation, audit, event, and settings primitives. |
| `b90a86f` | Frozen Java-builder/Rust-parser interoperability fixture. |

### Files and lifecycle

- Shared policy rejects traversal, drive/UNC/ADS paths, DOS devices, protected agent/VCS paths, and observed linked/junction/special ancestors. File and content routes protect configured TLS material. Ordinary dotfiles remain visible.
- File replacement stages a sibling temporary file and preserves the original on failed input. This is not recoverable trash, optimistic editing, upload journalling, or resumable transfer.
- Graceful `stop()` timeout/interruption retains the live process and `STOPPING`; restart refuses to duplicate it. Force kill stays explicit and does not discard a still-live handle. Failed launch clears stale running state.
- Console commands are one bounded line with controls/line separators rejected. Output draining discards overlong line tails without allocating unbounded lines, even with no viewer. The ring retains at most 10,000 lines / 2 MiB of UTF-8 text, with a per-line bound.
- **Not solved:** Java path checks are check-then-use; hostile ancestor replacement, hard-link reads, and every Windows reparse subtype are not certified. Separate OS identities and native race-resistant confinement are release gates.
- **Not solved:** `VestaWrapper` shutdown still stops Minecraft. There is no independent supervisor, protected local IPC, generation ownership, or true readiness protocol. These tests do not prove agent-crash continuity.

### Durable domain primitives

- Actual SQLite WAL, foreign keys per connection, busy timeout, schema versioning, startup integrity checks, stable server UUID, online pre-migration backup with verification, owner-only state permissions, and linked-child checks.
- Transactional operation submission/state, exact actor/server/idempotency binding (including device/source/locks/safety flags), sorted all-or-none resource locks, FIFO conflict fairness, safe-point cancellation, and explicit recovery for uncertain non-idempotent effects.
- Short-lived single-use confirmations bind requester/device/operation/action/target/payload hash/source revision. Acceptance, rejection/token burn, transition, and audit share one transaction. Injected audit failure rolls the mutation back.
- Append-only hash-chained audit with startup chain verification, bounded records, transactional writes, and an accidental-secret metadata-key guard. Callers must still keep values secret-free.
- Durable event cursors/retention-gap detection; pruning removes only an expired contiguous prefix when the clock goes backwards. Desired/effective settings stay separate until explicit adapter acknowledgement.
- **Not composed:** no versioned API, authenticated handler authority, executor, startup reconciliation wiring, live settings adapter, or operation/event client. The operation payload is hashed but **not persisted**; add bounded recoverable requested inputs before implementing an executor. Confirmation source revision must come from authoritative current server state, not client assertions.
- **Not solved:** local/network-filesystem support enforcement, service/game-account isolation, audit retention/export/reserved capacity, external checkpoints (tail truncation/whole-history replacement), secret vault, full apply-mode distinctions, and corruption-to-Minecraft-launch/repair orchestration. Owner-only ACLs do not isolate a game running under the same account.

### Vesta pack compatibility

- One frozen fixture is emitted by the real Java builder and read by Rust. It preserves current Java snake-case provider aliases, canonical aliases, IDs, URLs, SHA-1/SHA-256, size, environment, revision/minimumRevision, and loader identity.
- Launcher parser supports `.vestapack` / legacy `.vpack`, a root index or one wrapper directory, immutable provider validation, declared embedded files only, streaming verification of every declared embedded hash, bounded entry/index sizes, safe replacement, and destination junction checks.
- Signatures are preserved but **never grant trust**. Optional-client entries and symlink sources are explicitly rejected until selection/sandbox/exact-revision consent exist. Executable archive permissions and protected/traversal paths reject.
- **Not solved:** signed workspace/build/publication, pack access policy, server/client pairing and sync, full transactional provider installation, optional-entry UI, symlink metadata/consent, total storage/memory/extraction quotas, or Vesta export. CurseForge's current resolver chooses the destination filename/subfolder rather than enforcing the declared path; `resolver_chosen_paths` exposes that limitation. Missing provider resolution is not yet fail-closed in the existing installer.
- Existing live-root rescanning/automatic pack generation remains incompatible with the accepted separate-workspace/manual-publication contract.

## Verification performed

All new tests were written by the parent reviewer, not the programming subagents. Native process/link fixtures use temporary directories; no real game world, service, firewall, or deployment was touched.

| Command (run inside the corresponding repository) | Result on Windows, 2026-10-04 |
| --- | --- |
| `./gradlew :core:test :wrapper:test --rerun-tasks --offline --console=plain` | **224 tests / 38 suites passed**: core 78 / 20, wrapper 146 / 18; zero failures, errors, or skipped tests in fresh XML. |
| `cargo test -p piston-lib --offline` | **161 tests passed**: 132 unit, 29 integration (13 new Vesta tests); zero failures or ignored tests. |
| `cargo check -p vesta-launcher --offline` | Passed; unchanged unused-import/dead-code warnings remain. |

Focused server suites: file boundary 30 cases; durable operations 14; real-JVM lifecycle 16; audit/events/settings 14; SQLite state 8. Native Windows junction tests executed, not skipped. Command validation also has direct buffer tests and HTTP operator boundary regressions. Fake JVM stop/exit/output tests are not real Minecraft/loader compatibility tests.

The initial full server run found a changed exception-contract expectation and a Windows temporary-JAR cleanup race. Both were fixed and the entire suite rerun successfully. The first Rust fixture run found an invalid ambiguity test fixture; it was corrected to use two actual top-level directories, with no production relaxation. A final backend review caught unsupported Vesta export truncating output before rejection; it now rejects before file creation, with existing/absent-output regression coverage.

Java 8 source/target and deprecation warnings remain. Tauri warnings concern `tokio::task`, `instance_file::File`, `BaseKind::Home`, `home_preset`, and `rename_file_no_replace_best_effort` in unchanged code. No broad warning cleanup was attempted.

The cross-language fixture copies match SHA-256 `c6c8866d8277e02b4512faa3930124d079f1e68ec435b12c57f82449e5c7631b` before Git line-ending conversion; compare normalized JSON/line endings when checking Windows checkouts. Both language tests check the parsed fixture contract.

The **five Node ledger tests passed**, and `node scripts/requirements-ledger.mjs --check` passed for all 370 entries. These are a separate consistency gate; they do not execute the product suites or prove every requirement. Dependency intake is documented in `DEPENDENCY_DECISIONS.md`.

## Existing critical security gaps — do not expose this MVP

This batch does **not** repair the existing management trust model:

- Unauthenticated connector loopback TCP 9091; OP/permission-node-derived management authority; loopback power/security bypass.
- Arbitrary Origin reflection and legacy raw TCP 9092 plaintext/public management.
- Legacy plaintext/default secrets under live roots, unauthenticated status disclosure, missing game/agent OS-account separation.
- Unbounded whole-body request allocation, incomplete malformed-input error contracts, SSRF/global transfer/concurrency quotas.
- Installer probe TLS validation disabled; checksums of mutable sources are not signed distribution.

The new domain classes are not hooked up as replacement authority. Passing legacy operator HTTP tests certifies the file boundary only, not canonical authorization. Do not install/deploy/expose the current MVP as the completed secure product.

## Resume in dependency order

1. Finish P1: bounded persisted operation inputs; real executor/reconciliation; versioned route/OpenAPI/error/idempotency/pagination/event contracts; live settings composition. Keep new remote actions deny-by-default until canonical identity exists.
2. P2: server-local principals/devices/bootstrap/recovery, named capabilities/scopes/denies/expiry/delegation, Argon2id/vault/mTLS/stable identity, abuse controls; remove connector/OP/loopback/raw-TCP escalation. Test every permission boundary.
3. P3: independent supervisor, protected named-pipe/Unix-socket IPC, separate game identity, real readiness/generations/save leases/adoption/log catch-up; prove Minecraft survives agent-only crash/shutdown/restart.
4. P4: trash, optimistic edits, safe configuration, resumable transfers/SFTP/archives/quotas, verified content/SSRF defense, staged JAR apply after approved actual stop.
5. P5/P6: separate signed pack workspace/revisions/build/publish/update/trust and optional/link selection; pinned backup tooling/save coordination/manual restore/recovery export.
6. P7/P8: typed durable workflows/schedules, notifications/moderation/metrics/maintenance/sleep; signed update/install/network/proxy/ACME/security/distribution gates.
7. Complete modern launcher pairing, hosting/onboarding, capability-gated management UI and transactional sync against working APIs. Do not port the obsolete updater/auth UI wholesale. P9 remains deferred.

Remaining verification includes frontend typecheck/build/Vitest/i18n, full launcher workspace tests, Linux sockets/permissions/services, clean Windows installation/services, real Minecraft/loader matrix, signing/distribution, and independent security review. None was run or certified by this checkpoint.

Opus programmers completed bounded production partitions and the parent performed review/tests/integration fixes. Opus reached HTTP 429 while finishing follow-up edits; this is a resource boundary, not completion of P0–P8. Resume from the commits and explicit gaps above rather than widening remote authority around unimplemented adapters.

## Stats follow-up reviewed on 2026-10-14

The Rust metrics follow-up adds Windows/Linux physical-interface counter deltas, schema-v24 stored telemetry policy, bounded columnar revision cursors and ETags, latest-only authorized WebSocket updates, and default-off receipt-gated command polling for TPS/MSPT. Policy updates invalidate range caches and incremental projections in the same transaction as their audit event.

Verification on Windows: 28 focused metrics tests and 4 HTTPS metrics tests passed, including explicit 24h/30d/365d cleanup boundaries and audit rollback. All 164 Node tests and strict workspace Clippy passed. The full workspace run is recorded separately in SERVER_SOFTWARE_STATUS.json. Linux native collection and live Minecraft parser compatibility remain unverified. Diagnostic bundles, metrics export, resource enforcement and the remaining product/launcher work are still incomplete.
