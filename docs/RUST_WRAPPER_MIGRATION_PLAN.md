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
| `crates/vesta-control` | Identity/policy/password/proof components; operations/confirmations/runner/replay; protected file/content/pack services and runtime composition, split into cohesive modules. **Admitted 2026-10-08; canonical identity, bounded pack-password primitives/service and one-time Minecraft hasJoined challenges executed and tested.** The 2026-10-09 closed game-operation/confirmation/replay slice is implemented and Windows-tested in [its API](RUST_GAME_OPERATIONS_API.md) and [integration record](RUST_INTEGRATED_MIGRATION_PASS.md); the latest driver correction passed fresh integrated verification and scoped rereview, not whole-migration approval. Authenticated device/claim linking, complete Microsoft identity flow, file/content and full pack/host composition remain unfinished. |
| `crates/vesta-game-runtime` | **Admitted 2026-10-08: bounded console and owned direct-child component**, with native inert-process tests. Literal argv/cwd, generation-bound stdin/stop/kill, timeout retention and actual exit/drain ownership are implemented. The 2026-10-09 concrete lifecycle driver and trusted launch-only TOML adapter are tracked separately in [the integration record](RUST_INTEGRATED_MIGRATION_PASS.md) and [launch checkpoint](RUST_LAUNCH_CONFIGURATION_CHECKPOINT.md); independent supervisor/IPC/process-tree control remain unfinished. No network listener or administrative policy. |
| `crates/vesta-server-install` | **Admitted 2026-10-08; extracted and verified**, with all installer test families and a narrow Piston adapter. Explicit private transport configuration/live progress, no Tauri/full Piston. Authenticated install/activation/launch composition remains unfinished. |
| `apps/vesta-agent` | **Added 2026-10-09: passive `check-config` CLI only**, the sixth non-publishable member. Strict trusted host descriptions/protected loading, bounded player parsing, peer admission and framing are tracked in [the host checkpoint](RUST_HOST_CONFIGURATION_CHECKPOINT.md). Full startup/runtime/transport composition remains unfinished; no listener, bootstrap or game start is supplied. |

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
   conservative reconciliation over the existing schema. Its narrow API has been frozen by Sol;
   **ordinary observation and a staged test-only private kernel are integrated and verified** —
   see [the bounded operations checkpoint](RUST_OPERATIONS_OBSERVATION_CHECKPOINT.md) for corrected
   execution, failed-attempt history and the explicit production/test staging boundary. The useful public facade
   observes sanitized summaries under current task access; cancellation additionally requires the
   exact initiating actor/device and current action requirements of a verified binding. There is
   no invented `tasks.cancel` capability. That checkpoint preceded production bindings. The later
   **2026-10-09** closed Start/Stop/ForceKill composition adds real owned execution, canonical
   confirmation consumption and authorized replay; see [the integration record](RUST_INTEGRATED_MIGRATION_PASS.md).
   Non-game bindings, authenticated remote wiring and independent supervision remain separate
   work; no fake handler or successful no-op may imply those are complete.
6. **Game/files/pack/composition.** Build literal argv/cwd, generation-bound start/stop/kill,
   timeout ownership, graceful-stop refusal before effects and continuous bounded output
   draining as cohesive Rust components. PID/open port is not readiness. Current durable
   lifecycle types are start/stop/force-kill, not a new durable restart. Required behavior
   and legacy transports must be accounted for, not reproduced by copying Java APIs or
   weakening trust paths. **Bounded console capture is admitted and verified 2026-10-08** —
   see [the console checkpoint](RUST_CONSOLE_CAPTURE_CHECKPOINT.md). Inert readers, ring retention
   and paging alone do not establish process-pipe draining, lifecycle ownership or authenticated output.
   **Later 2026-10-08 owned direct-child increment admitted and verified** — see
   [the process checkpoint](RUST_GAME_PROCESS_CHECKPOINT.md) for native literal argv/cwd, bounded
   stdin, actual exit/drain ownership, timeout retention and error-path recovery evidence. This
   ordinary component does not activate the staged operations kernel, install production lifecycle
   bindings or establish authenticated management, artifact/Java-plan composition, readiness,
   process-tree containment or independent supervisor continuity.
   **2026-10-09 trusted launch configuration increment:** a bounded launch-only TOML document
   compiles Java JAR, ordered argfile or native shapes into the existing private process spec.
   Base/cwd/executable are explicit; no PATH/shell/legacy/missing-config/auto-start fallback.
   Required JAR/argfile references are rechecked at real spawn before its effect guard. One real
   canonical composition fixture witnesses no automatic start or config-derived authority.
   [The launch checkpoint](RUST_LAUNCH_CONFIGURATION_CHECKPOINT.md) records exact executed
   evidence, failed attempts, deliberate Rust-native changes and remaining full-config/file-opening
   gaps. This does not supply Java discovery, real loader execution or a host executable.
7. **Installer extraction/reuse.** Extract the reviewed installer and all tests, with
   exactly four external seams: progress types, Java-major parser, configured proxy,
   and linked-ancestor checks. Preserve transfer restrictions, deadlines, original
   pins, receipts, no-clobber publication, schema-1 golden hash and schema-2 v2 domain.
   Keep six automatic providers and explicit Forge/NeoForge unavailability. No broad
   launcher engine feature split, managed-JRE import, JNI or unselected sidecar.
   **Later 2026-10-08 extraction integrated and verified** — see
   [the installer checkpoint](RUST_INSTALLER_EXTRACTION_CHECKPOINT.md). All test families moved;
   Piston retains a borrowed progress/proxy adapter, not duplicate implementation. Six requirement
   rows relocate test pointers only, without changing claims/statuses/history. Relative sibling
   checkouts need revision-pinned CI wiring before publication; local-only references are not
   remote CI evidence. Host operation/confirmation/recovery/activation remains unfinished.

## Model allocation and owned work

GPT-6 Luna handles bounded inventory, fixture and test-family accounting against
approved Rust interfaces. New tests preserve relevant behavioral and safety assertions
without copying obsolete Java APIs, class structure or incidental JDK semantics.
Parent/GPT-6.1 Sol handles architecture, security-sensitive implementation, integration,
independent assertions and cleanup. A final **independent GPT-6.1 Sol review** is
mandatory. The owner requested **fewer helpers on 2026-10-09**, allowing bounded Luna and
explicitly requested Haiku assistance as a limited exception to the earlier no-Anthropic-helper
rule. The requested Haiku 5.5 model/registration must actually be verified; no version/model
fallback is authorized silently. Serious backend/review remains with Sol. This increment uses
one resumed Sol reviewer and narrow Luna accounting, not nested/default-model workflows.
Luna test assistance supersedes the earlier parent-only test-authorship preference for this
migration, not all future work.

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
- **Later 2026-10-08 operations/console increment, native Windows 11 / Rust 1.96.0:** fresh
  four-member format/check/offline-locked tests/strict all-target Clippy passed: **353 Rust tests**,
  zero failures/ignored, and **159 Node tests**, zero failures/skips. Ordinary current-policy
  operation observations/cancellation validation and bounded console capture are integrated.
  Private admission/OS scheduling/recovery/retention is **staged cfg(test) code**, not a shipped
  scheduler or worker. Scoped independent Sol source reviews and parent-run fault/concurrency
  regressions are documented in the two checkpoints. All 222 hashes/370 original requirements
  remain intact; inventory dispositions stay **24 migrated, 17 retained Java plugin, 181 not
  migrated, 0 verified**. Final whole-rebuild review, authenticated composition and actual game/
  worker effects remain pending.
- **Later 2026-10-08 owned direct-child increment, native Windows 11 / Rust 1.96.0:** fresh
  settled-source format/check/offline-locked workspace tests/strict all-target Clippy passed:
  **388 Rust tests**, zero failed/ignored, and **159 Node tests**, zero failed/skipped. Runtime
  now includes literal native launch, bounded stdin, generation-bound stop/kill/wait, timeout
  ownership and actual exit/drain finalization. Scoped independent Sol source review identified
  two corrected error-path defects; parent executed real inert-process and private fault witnesses.
  [The process checkpoint](RUST_GAME_PROCESS_CHECKPOINT.md) retains earlier failed attempts and
  the exact assertion limits. All 222 hashes/370 requirements and the protected launcher digest
  remain unchanged; parity stays **24 migrated, 17 retained Java plugin, 181 not migrated,
  0 verified**. Production lifecycle bindings, authenticated/Java-plan composition, process-tree
  containment, independent supervision and mandatory final whole-rebuild review remain pending.
- **Later 2026-10-08 installer extraction, native Windows 11 / Rust 1.96.0:** fresh settled
  server format/all-target check/workspace tests/strict all-target Clippy passed: **556 Rust
  tests**, including **168 extracted installer cases**, plus **210 full Piston tests** and
  **159 Node tests**, zero failed/ignored/skipped. Old Piston count is intentionally reduced
  by the moved 164 cases, with two new adapter tests; no family was dropped. Sol's scoped
  extraction/follow-up source review and parent's inert-failure correction/cleanup are recorded
  [at the checkpoint](RUST_INSTALLER_EXTRACTION_CHECKPOINT.md), including earlier lint/accounting
  failures. All 222 Java hashes/370 frozen requirements and original statuses/history remain;
  six rows change only 14 live test pointers. Parity stays **24 migrated, 17 retained Java plugin,
  181 not migrated, 0 verified**; final whole-rebuild review stays `not_run`. Core Rust minimum
  is honestly **1.88** for the preserved sysinfo pin; no MSRV or non-Windows execution is inferred.
- **2026-10-09 closed lifecycle/launch continuation:** the preceding operation checkpoint and
  fresh pre-launch baseline passed **635 Rust tests**, plus fresh whole-workspace format/check/
  strict Clippy and **159 Node tests**. The next bounded trusted launch adapter has **78 passing
  runtime tests**, **2 canonical-composition tests** and **14 passing focused driver tests**;
  the separate fresh settled-source integrated rerun passed **658 Rust tests**, whole-workspace
  format/check/strict Clippy, **210 companion tests** and **159 Node tests** plus both checkers.
  [The launch checkpoint](RUST_LAUNCH_CONFIGURATION_CHECKPOINT.md) records those results and
  failed attempts separately from the component results. Scoped Sol review corrected mid-dispatch observation-result loss and a new
  test readiness race. Only LaunchSpec/LaunchSpecTest receive migrated-not-verified mappings:
  **26 migrated, 17 retained Java plugin, 179 not_migrated, 0 verified**. Full config/application,
  protected files, packs/sync, transports, platform/MSRV and final whole-rebuild gates remain open.
- **2026-10-09 file/pack implementation continuation:** canonical bounded file listing/read
  and protected native path observations, strict Rust-native catalogue/revision validation,
  deterministic pure add/update/keep/remove proposals, and ten new real SQLite foundation
  assertions are implemented. The first full source run passed **707 Rust tests**, workspace
  format/check/strict Clippy, **159 Node tests**/both checkers and **210 companion tests**.
  Scoped Sol review found configured missing-secret alias/existence gaps; the parent fixed
  them and added actual junction/missing-suffix/pre-probe regressions. Fresh final corrected-source
  verification passed **710 Rust tests**, format/all-target check/strict Clippy; fresh Node and
  accounting/preservation checks also passed. Exact commands and the Windows concurrent-linker
  failed attempt are recorded [at the checkpoint](RUST_FILE_PACK_CHECKPOINT.md). No complete Java family or original requirement is promoted:
  **26 migrated, 17 retained Java plugin, 179 not_migrated, 0 verified**, with final whole-rebuild
  review still `not_run`. Durable file/content mutations, host catalogue/archive services,
  full configuration/application and safe transport/session composition remain unfinished.
- **2026-10-09 host-description continuation:** strict trusted full-host configuration,
  protected bounded local loading, the passive sixth-member `check-config` executable,
  one-line player parsing, bounded monotonic peer admission and UTF-8 framing are
  implemented. Fresh settled-source Windows verification passed **816 Rust tests**,
  format/all-target check/strict Clippy, **159 Node tests**/both accounting checkers
  and **210 companion tests**/all-target check. Scoped Sol review and parent regression
  fixes are recorded [at the host checkpoint](RUST_HOST_CONFIGURATION_CHECKPOINT.md).
  Full host/runtime/session/route composition remains unfinished; no family, original
  requirement or final whole-rebuild review status is promoted by this slice.
- **2026-10-09 pack-preparation and player-query continuation:** explicit trusted pack
  preparation (bounded deterministic snapshot, retained bodies, opaque archive pinning,
  strict map-only source registry) and a closed authorized `list` query are implemented,
  including a fixed fail-open `SourceRegistry::source_for` and the protocol test that had
  masked it. Fresh settled-source Windows verification passed **864 Rust tests** across
  30 summaries, format/all-target check/strict Clippy, **159 Node tests**/both accounting
  checkers and **210 companion tests**/all-target check. Durable file/content effects,
  configured catalogue/archive *services*, authenticated pack sessions/routes, full host
  composition and remaining assertion-family accounting are still unfinished; this wave
  was not independently reviewed and promotes no family, original requirement or
  final whole-rebuild review status.
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
