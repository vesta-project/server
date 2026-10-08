# Rust-native host backend — implementation plan and evidence

Owner direction and implementation approval: **2026-10-07**.
Status: **implementation in progress; not parity complete**.

This plan guides the owner's Rust-native rebuild, not a new product specification or an
API-by-API Java port. [SERVER_SOFTWARE_REQUIREMENTS.md](SERVER_SOFTWARE_REQUIREMENTS.md),
the **370** original indexed normative bullets in [REQUIREMENT_TRACEABILITY.json](REQUIREMENT_TRACEABILITY.json),
and their dated evidence remain authoritative. Original requirement statuses are not
promoted by implementation or tests. Historical Java/provider checkpoints and the
roadmap/status assessment are retained as historical evidence.

## Scope and behavioral references

Implement useful host-side behavior and durable safety foundations. Existing Java
wrapper/core code, tests and Gradle builds remain available as behavioral references where
they express accepted product needs; Java class boundaries, method names and incidental JDK
runtime behavior are not required Rust architecture. Keep the Java Minecraft plugin/mod.
Account for each relevant behavior and test family; missing required behavior is an explicit
gap, not successful parity. Known unsafe legacy trust paths need a safer replacement, not
a compatibility route that weakens canonical policy.

Reference commits:

- Server: `c30800e1b750586df8f69036d9cc78ffb8e61f78`.
- Companion launcher: `e5bc7e00b6f88a303bf6430b4c07ca30d36e4a14`.

Both origins were fetched before implementation; neither work branch was behind
`origin/main`. Existing owner-direction document edits remain. The user's modified
launcher `vesta-launcher/src-tauri/Cargo.toml` is protected: do not edit, format, stage,
or commit it. Its starting SHA-256 is
`4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`.
Do not switch/reset/stash worktrees or remove Java comparison code.

## Product and authority boundaries

- One installation script will set up the backend on the server host. Its service,
  privileges, distribution and recovery behavior remain separate unfinished work.
- The launcher is the initial authenticated management client; a web client is planned
  later. Clients send desired software/version changes, not URLs, digests, sizes,
  library membership, launch arguments, executable policy or artifact paths.
- The backend resolves metadata, produces an exact review, gets confirmation and
  downloads/verifies/installs. Preparation may occur while Minecraft runs. Activation
  requires **observed game exit**; no automatic stop/restart was selected.
- Software versions are side by side. World/configuration persist and the previous
  software remains for rollback; world restore is never automatic.
- Microsoft/Minecraft UUID proof is the expected normal identity flow, resolved into
  current server-local canonical authority. Identity proof is not administration.
  Pack passwords are an alternative/fallback, not administrative passwords.
- OP/permission plugins, connector possession, loopback/LAN location, software support
  and local review consent do not confer management authority.

See section 0 of [the implementation plan](SERVER_SOFTWARE_IMPLEMENTATION_PLAN.md)
and [the provider plan](SERVER_SOFTWARE_PROVIDER_FRAMEWORK_PLAN.md).

## Cohesive workspace boundaries

The root Cargo workspace coexists with the unchanged Gradle/Stonecutter tree. Crates
are admitted as implemented; an empty binary/crate must not imply completed behavior.
All new workspace packages are non-publishable development packages.

| Location | Responsibility / boundary |
| --- | --- |
| `crates/vesta-protocol` | Bounded discovery/negotiation and eventual pack/wire contracts. No authority, process control or network listener. **Admitted; executed tests.** |
| `crates/vesta-state` | Protected state, SQLite schema/transactions/migrations/backup, audit, events, desired/effective settings. Trusted in-process persistence, not a remote API. **Admitted; executed tests.** |
| `crates/vesta-control` | Identity/policy/password/proof components; operations/confirmations/runner/replay; protected file/content/pack services and runtime composition, split into cohesive modules. **Admitted 2026-10-08; canonical identity, bounded pack-password primitives/service and one-time Minecraft hasJoined challenges executed and tested.** Authenticated device/claim linking, the complete Microsoft identity flow, operations, file/content and full pack/runtime composition remain unfinished. |
| `crates/vesta-game-runtime` (planned) | Literal launch definitions, owned game generations, stdin/exits, bounded console and platform adapters. No network listener or administrative policy. |
| `crates/vesta-server-install` (planned) | Narrow extracted installer/providers; explicit progress/transport policy, not Tauri/full Piston. |
| `apps/vesta-agent` (planned) | Thin configuration/CLI/composition. Remote management remains closed until canonical authenticated transport composition exists. |

The current Java game process is an in-process wrapper child. Keep the future
independent supervisor at the runtime boundary, but do not add a stub binary or claim
agent-crash continuity/protected IPC. Containers are optional hardening, not a
replacement for OS identities, permissions, process ownership and network policy.

## Checked accounting and evidence

[The parity inventory](RUST_WRAPPER_PARITY.json) freezes Java file byte hashes and
requirement ID/text pairs independently of changing evidence. The checker validates
explicit inventory accounting rather than automatically regenerating away new files.

Account for all 222 inventory entries and each relevant behavior/test family with Rust
evidence or a defensible disposition: Java-plugin retention, safer replacement, pending
adapter or unresolved required behavior. A Java-to-Rust file mapping is traceability, not
behavioral equivalence or required one-file-per-class architecture. Record applicable
requirement IDs only when supported; an empty mapping remains pending. Evidence must name
actual command, date, platform, assertion scope and limitation. A test count or compiler
success alone is not parity. The checker validates structure and evidence references; it
does not execute those tests or certify their meaning.

```sh
node scripts/rust-wrapper-parity.mjs
node --test scripts/requirements-ledger.test.mjs scripts/server-feature-protocol.test.mjs scripts/rust-wrapper-parity.test.mjs
```

## Implementation waves

1. **Preserve behavioral accounting/contracts.** Keep all original requirements and Java
   references. Implement strict discovery idiomatically in Rust, using the independent Node
   peer and shared `protocol/v1/fixtures` corpus as relevant contract evidence. Features
   describe implementation support only. Preserve justified pack-model contracts with
   frozen fixtures, not live auto-scanning; Java APIs are not Rust design blueprints.
2. **Durable state.** Retain pinned rusqlite and its supported online backup API for this
   increment. Build one transaction-owned typed unit-of-work boundary: policy resolution,
   mutation and success audit use the same immediate transaction; returned domain errors
   roll back without sentinel values or `Ok(Err(...))` bridges. Preserve WAL/FKs/FULL/busy
   timeout, serialized writes, query-only snapshots, fair lifecycle/close, nested-call
   rejection, idle anchor, stable UUID, transactional migrations and verified backups.
   Never share one live production database between Java and Rust writers.
   **Done**: `TransactionError<E>` plus `read_with`/`write_with`; the old sentinel bridge is gone.
3. **Audit/events/settings.** Mutation and audit share one typed transaction. Preserve
   current persisted `vesta-audit-v1` bytes only as a justified storage-format contract;
   validate it against independently captured Gson vectors rather than Rust-generated
   expectations. Preserve contiguous event-prefix pruning/watermark/gaps and bounded
   projections where required, without copying a Java service API. Desired changes,
   including Immediate, are not effective until explicit adapter acknowledgement.
4. **Canonical identity/policy.** Build domain commands over private typed identity
   repositories, preserving principals/devices/roles/assignments/overrides, current-policy
   checks, deny-wins/scopes/delegation containment/expiry/revocation and local-only
   privileged grants. Use the stable 51 capability IDs and six built-in role definitions.
   The first scope increment uses portable ASCII components, segment-boundary matching,
   case-sensitive allows and ASCII-insensitive denies; reject ambiguous paths instead of
   emulating Java. Unsupported stored allows cannot grant, unsupported denies fail closed,
   and Unicode file scopes remain an explicit gap. Do not emulate Java casing or timestamp
   formatting. Production local-authority proof stays unavailable without real
   OS-authenticated implementation. No fake enrollment/session/vault completion.
   **Done 2026-10-08** — see [the identity increment record](RUST_IDENTITY_API_FREEZE.md).
   Unicode file scopes remain an open gap and no identity row is `verified`.
   **Password increment 2026-10-08:** owned UTF-8 secret buffers, bounded Argon2id, caller-owned
   zeroized KDF workspace, process-wide nonblocking admission and server-local audited credential
   lifecycle are implemented. Authority/revision are checked before derivation and in the final
   admitted writer; matches create no identity or management authority. See
   [the password/state checkpoint](RUST_PASSWORD_STATE_CHECKPOINT.md) for exact execution,
   intentional differences and remaining proof/device-grant/runtime gaps.
   **Minecraft challenge increment 2026-10-08:** one-time bounded server-bound online-profile
   verification and the fixed hasJoined adapter are implemented and covered by controlled fixtures.
   [The Minecraft checkpoint](RUST_MINECRAFT_IDENTITY_CHECKPOINT.md) distinguishes the fresh
   Windows result from real transport, device/claim linking and full Microsoft login composition.
5. **Durable operations.** Implement the required behavior with Rust-native domain APIs:
   bounded canonical inputs, exact idempotency, all-or-none sorted locks/FIFO fairness,
   private runner ownership, OS runner lock, current authority at submission/claim/run/effect,
   safe-point cancellation, revision-bound single-use confirmation, per-read authorized
   sanitized replay and recovery-required uncertain outcomes. Keep ownership/database/locks
   until an uncooperative worker actually exits; dropping a handle is not termination.
   The next bounded slice is the [durable-operations kernel](RUST_OPERATIONS_KERNEL_PLAN.md):
   canonical admission/idempotency, owned fair claiming/locks, cancellation journaling and
   conservative reconciliation over the existing schema. Its design has been scoped by Sol;
   **implementation and tests have not begun**. Production execution handlers, real worker
   supervision, confirmation consumption, authorized replay and remote wiring remain separate
   work; no fake handler or successful no-op may imply those are complete.
6. **Game/files/pack/composition.** Build literal argv/cwd, generation-bound start/stop/kill,
   timeout ownership, graceful-stop refusal before effects and continuous bounded output
   draining as cohesive Rust components. PID/open port is not readiness. Current durable
   lifecycle types are start/stop/force-kill, not a new durable restart. Required behavior
   and legacy transports must be accounted for, not reproduced by copying Java APIs or
   weakening trust paths.
7. **Installer extraction/reuse.** Extract the reviewed installer and all tests, with
   exactly four external seams: progress types, Java-major parser, configured proxy,
   and linked-ancestor checks. Preserve transfer restrictions, deadlines, original
   pins, receipts, no-clobber publication, schema-1 golden hash and schema-2 v2 domain.
   Keep six automatic providers and explicit Forge/NeoForge unavailability. No broad
   launcher engine feature split, managed-JRE import, JNI or unselected sidecar.

## Model allocation and owned work

GPT-6 Luna handles bounded inventory, fixture and test-family accounting against
approved Rust interfaces. New tests preserve relevant behavioral and safety assertions
without copying obsolete Java APIs, class structure or incidental JDK semantics.
Parent/GPT-6.1 Sol handles architecture, security-sensitive implementation, integration,
independent assertions and cleanup. A final **independent GPT-6.1 Sol review** is
mandatory. No Anthropic-model helpers/fallbacks or default-model nested workflows
are authorized for this migration. Luna test assistance supersedes the earlier
parent-only test-authorship preference for this migration, not all future work.

Assign disjoint owned files and frozen APIs. No worker may weaken assertions,
regenerate golden output from the port, edit another worker's source, add hidden
ignores, change shared manifests/locks or self-certify parity. Parent owns shared
build invocation and verification. A model rate limit is a blocker, not permission
to silently substitute another model.

## Verification and honest current evidence

Historical Windows foundation checkpoint on 2026-10-07, preceding the current storage/identity redesign:

- `:core:test :wrapper:test --rerun-tasks --offline --no-daemon --console=plain`
  succeeded serially: core **78 passed**; wrapper **812 passed, 1 skipped**. The
  skipped launch-shape assertion needs a POSIX executable bit, absent on Windows.
  Earlier interrupted/reporting-failed invocations are not passing evidence.
- Independent Node protocol/requirement suites: **146 passed**, zero failures/skips.
- Companion Piston pre-migration suite: **372 passed**, zero failures/ignored;
  extraction/adapter regressions still need a fresh later run.
- Initial Rust discovery and adversarial behavior tests executed on Windows; these
  bounded tests are not source-to-source parity or completion of the rebuild. The
  2026-10-07 admitted foundation result is historical evidence only.
- **2026-10-08, Windows 11 / cargo 1.96.0: the storage and canonical-identity increment is
  implemented, admitted and executed.** `cargo fmt --all -- --check`,
  `cargo check --workspace --all-targets --locked`,
  `cargo test --workspace --locked` (**192 passed**, zero failed/ignored) and
  `cargo clippy --workspace --all-targets --locked -- -D warnings` all passed across three
  admitted members, together with 159 passing Node accounting suites. `vesta-control`
  contributes 109 real-SQLite tests; the typed transaction/error boundary replaced the
  sentinel bridge; JDK text emulation was removed. An **interim** independent review fixed a
  real audit-metadata bound defect and an error-code namespace collision. Parity accounting now
  carries 14 `migrated` rows, 17 `retained_java_plugin`, 191 `not_migrated` and **0 `verified`**.
  At that checkpoint the mandated independent **GPT-6.1 Sol** review was **`not_run`** and
  that model was unavailable; nothing was silently substituted. Later scoped Sol source reviews
  are recorded in the password/state checkpoint, not as the pending final full-rebuild review.
  This remains neither whole-product completion nor final sign-off. See [the identity increment record](RUST_IDENTITY_API_FREEZE.md).
- **Later 2026-10-08 password/state review increment, native Windows 11 / Rust 1.96.0:** fresh
  format check, all-target offline locked check, **237 passing Rust tests** and strict all-target
  Clippy passed after review-driven assertions; **159 Node tests** and both accounting checks passed.
  Bounded password mappings bring the unchanged 222-row inventory to **21 migrated, 17 retained
  Java plugin, 184 not migrated, 0 verified**. Independent Sol checkpoint source review and regression
  strengthening are recorded in [the password/state checkpoint](RUST_PASSWORD_STATE_CHECKPOINT.md).
  This does not satisfy the pending final whole-rebuild review or promote original requirement statuses.
- **Later 2026-10-08 Minecraft challenge increment, native Windows 11 / Rust 1.96.0:** fresh
  format/check/offline-locked workspace tests/strict all-target Clippy passed: **279 Rust tests**,
  zero failed/ignored, including 40 Minecraft unit tests and two new compile-fail boundaries.
  Only three bounded Java reference rows are mapped: **24 migrated, 17 retained Java plugin,
  181 not migrated, 0 verified**, with all 222 hashes and 370 requirements preserved. Scoped
  independent Sol source/test review and parent corrections are recorded in
  [the Minecraft checkpoint](RUST_MINECRAFT_IDENTITY_CHECKPOINT.md). Real transport/Mojang,
  authenticated-device/canonical-claim composition, dependency Trace redaction, hard DNS
  cancellation, other platforms/MSRV and final whole-rebuild review remain unfinished.
- Owner direction is to design for all intended platform compatibilities while testing
  Windows only in the current increment. Linux, macOS and non-Windows architecture/native
  behavior remain unexecuted and unverified; WSL presence is not platform evidence.

For each admitted Rust wave, run locked check/tests, format check and scoped strict
Clippy. Use real temporary SQLite/native links and inert owned fake-process fixtures.
Keep downloaded dependency code, native loading, filesystem/ACL behavior and release
provenance/notice obligations separate from compilation evidence.

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Use reproducible sibling checkouts for future launcher reuse, with relative path
adapters and CI fetching pinned references; no absolute machine dependencies,
moving unpinned Git dependency, new publication or invented release crate.

Completion requires behavioral and security obligations across the inventory to be
accounted for, fresh Windows and Linux evidence, preserved 370 original requirements,
and completed independent Sol review with verified fixes/regressions and fresh reruns.
Design for the intended supported platforms; test Windows only in the current increment,
then gather evidence on each required platform/architecture before claiming compatibility.
Until then implementation and final review remain **pending**. Checkpoint evidence never
makes the product secure, complete, publicly deployable or release-ready.

## Explicitly unfinished and prohibited effects

Independent supervisor/protected IPC/crash continuity; OS-authenticated bootstrap;
device enrollment/mTLS/sessions/invitations/vault; canonical remote handlers;
durable install/update/activation/adoption/world-backup composition; managed signed
Java; Forge/NeoForge execution; host onboarding/UI; stronger native isolation;
services/signing/distribution and the real Minecraft/platform matrix remain open.

No real provider artifact/installer/Minecraft execution, EULA acceptance, world/demo
erasure, service/firewall/router changes, public exposure, signing/global-security
changes, arbitrary privileged commands or automatic world restore are authorized.
Push, PR creation or publication needs separate approval.

**2026-10-08 owner update:** make local **unsigned checkpoint commits** as verified chunks are
ready. Use a per-command `commit.gpgsign=false` override only; do not change repository/global
signing configuration, request a PIN or bypass a failing hook. This local-commit authorization
supersedes the earlier no-commit restriction for future checkpoints, not the no-push/publication,
protected-launcher or original-requirement preservation boundaries.
