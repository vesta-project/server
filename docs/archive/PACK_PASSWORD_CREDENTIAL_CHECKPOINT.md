# Durable pack-password credential checkpoint

Assessment date: **2026-10-05**. This extends [durable lifecycle](DURABLE_LIFECYCLE_CHECKPOINT.md) with **in-process pack-password verifier persistence and bounded KDF admission**. It is not a password login, device grant, remotely usable pack policy, Microsoft authentication adapter, vault or completed hosting product. Schema is now **4**; earlier checkpoints preserve their historical schema/count statements.

## Identity direction and preserved launcher UI

Microsoft/Minecraft authentication remains the expected common online-mode identity flow, as reiterated by the user. Its verified Minecraft UUID must map to a canonical server-local Vesta principal and the configured policy. A Microsoft sign-in, Minecraft OP, plugin permission or connector possession does not automatically confer management authority. Minecraft-policy pack access must evaluate actual admission data, including whitelist/bans and online-mode limitations; explicitly granted management capabilities remain separate.

Pack passwords are an alternative/fallback, especially for offline-mode servers. This slice does not replace Microsoft/Minecraft identity with password-based administrator accounts. It does not implement either remote identity adapter yet.

The current-main launcher UI and earlier manual Vesta import work were **not scrapped**. Hosting/onboarding and capability-gated management screens remain pending. This slice changes no launcher source; an existing launcher `vesta-launcher/src-tauri/Cargo.toml` working-tree edit was left untouched.

## Implemented boundaries

### Singleton storage and optimistic revisions

`PackPasswordSchema` adds a STRICT, canonical-server-scoped `pack_passwords` table. The primary/foreign key is the persisted database server ID; PHC length, positive integer revision and active/revoked consistency have SQL constraints. Schema 3 migrates through the existing verified pre-migration backup mechanism, without importing old TOML plaintext or generating a password.

- `setPassword(RemoteActor, expectedRevision, char[])` creates only at absent revision 0; otherwise it rotates/reactivates exactly the current positive revision.
- `revoke(RemoteActor, expectedRevision)` clears the live PHC and retains the singleton as a revoked tombstone, incrementing revision even on explicitly repeated revocation. Reactivation cannot reset the revision. Java `long`/SQLite INTEGER overflow rejects before derivation/mutation.
- Original creation time survives rotation/reactivation. Metadata has no verifier accessor.
- Live-row clearing is **not secure erasure** of SQLite pages, WAL, backups or immutable JVM strings. Backups can retain historical verifiers. Same-user/native filesystem isolation is not added.

### Canonical administrative authority and audit

Create/rotate/revoke and metadata inspection require the exact current `credentials.manage` capability. Current principal/device binding, active role assignment, expiry and explicit denies are resolved from storage. Owner/Administrator membership still gates this administrator-reserved capability; Maintainer/Pack User do not acquire it through rank or pack access.

Policy and expected revision are checked before KDF work and again in the final write transaction. Hashing holds no database transaction. Verification similarly snapshots, derives outside transactions and rechecks the active revision before reporting a match; a changed PHC at the same revision also rejects.

Mutation and secret-free audit commit atomically. Admitted verification outcomes have a fixed **system/in-process audit identity**, not a fabricated remote principal; final revision checking and audit share a transaction. Audit failure cannot commit a credential mutation or return a successful match. Applicable administrative refusals are audited after rollback; failure to record a refusal returns `audit_unavailable`. Rate/concurrency refusals intentionally do not emit unbounded audit writes; future transport-level bounded source/attempt accounting is still required.

No password/PHC enters normal records, audit metadata, events or operation canonical inputs. Crypto runtime-fault messages and potentially secret-bearing SQL/audit exception messages or cause chains are not exported; the service preserves only fixed machine-readable failure codes. Password arrays remain caller-owned; `PasswordHasher` retains its strict UTF-8/PHC/cost validation, constant-time hash comparison and owned-buffer cleanup. There is no automatic rehash mutation.

### Admission and no authority-bearing result

`PasswordKdfGate` provides a nonblocking semaphore plus separate fixed-key monotonic token buckets. Production services in the same JVM share the gate:

| Budget | Production default |
| --- | --- |
| Concurrent derivations | 2 |
| Verification attempts, including absent/revoked/malformed/mismatch | 60/minute; burst 60 |
| Administrative attempts after authority/revision precheck | 10/minute; burst 10 |

There is no waiting queue or attacker-controlled per-IP/principal counter map. Permits release exactly once, including audit/database/crypto failures. With the default hasher's 64-MiB verification ceiling, simultaneous **configured Argon2 working memory** is at most 128 MiB; library/JVM overhead is not included. Rate buckets reset on restart and are shared across server runtimes in this JVM, not across independent agent processes.

Revocation requires its administrative rate admission but **no KDF permit**, so a saturated derivation gate cannot prevent this cheap administrative command. Verification cannot consume the management rate bucket, though hashing still shares the concurrency ceiling. OS-local owner recovery is not routed through this gate.

`Verification` contains only status, canonical server ID and observed revision. It is advisory—not a `RemoteActor`, bearer token, device grant, principal or role. Its public record constructor is not proof of password possession. A future grant issuer must run its own trusted verification and atomically bind the still-current revision; it must never authenticate a caller by accepting a supplied result/revision.

`AgentRuntime.packPasswords()` composes production defaults in opt-in durable mode, with no new listener or route. Production `LocalAuthority.UNAVAILABLE` remains unchanged. Opening/reopening creates no owner, credential or Minecraft process.

## Parent-authored verification

Final fresh verification on **Windows, 2026-10-05**, after all production/redaction and test-synchronization edits:

| Gate | Result |
| --- | --- |
| `:core:test :wrapper:test --rerun-tasks --offline --console=plain` | **507 tests / 52 suites passed**: core 78 / 20, wrapper 429 / 32; zero failures, errors or skipped tests in fresh XML. |
| `node --test scripts/requirements-ledger.test.mjs` | **5 tests passed**, zero failures or skips. |
| `node scripts/requirements-ledger.mjs --check` | All **370** normative entries consistent; **51 partially verified, 319 not verified, zero fully verified**. No status was promoted by this slice. |
| `git diff --cached --check` | Passed for all scoped new and modified files. |

The **70 new parent-authored cases** are: pack-password service 55, KDF gate 9, migration 4 and runtime composition 2. No launcher checks were rerun: this slice changes no launcher source. Existing Java 8 source/target and deprecation warnings remain. Tests used temporary SQLite state and owned fake-game JVM fixtures only; no real Minecraft server, demo reset, profile, service, firewall, signing configuration or public deployment was touched.

New suites:

- `PackPasswordServiceTest`: canonical policy/expiry/reserved-role matrix; persisted reopen and fresh salts; rotation/revocation/reactivation/overflow; audit rollback; invalid buffers/PHCs and redaction; bounded missing/revoked/malformed attempts; fault-path permit release; transaction-free KDF; deterministic authority/revision changes and competing CAS writers; stale matching snapshot rejection; database failure and shutdown during derivation; no principal/device/role creation.
- `PasswordKdfGateTest`: exact rate/refill boundaries, rollback/long-idle clocks, independent rates/shared concurrency, idempotent release, concurrent saturation, cheap revocation admission, fixed defaults and invalid settings.
- `PackPasswordMigrationTest`: actual schema-3-to-4 migration and verified original backup, stable server identity/built-in roles, clean-schema constraints, migration rollback and backup-collision refusal.
- `AgentRuntimePasswordTest`: production/no-owner/no-credential/no-game startup and trusted fixture bootstrap/reopen persistence without game effects.

The first focused attempt failed compilation in a parent-authored generic Boolean assertion (JUnit overload inference). It was fixed by assigning the database result to a primitive boolean. The ensuing focused run passed **63 tests**, before three additional shutdown/database/no-grant cases and stronger permit-release assertions. A full run then passed **503 tests**, before the final SQL-error redaction edits and four trigger-fault cases. Those earlier runs do not certify the subsequent edits.

The following full run passed `:core:test` but failed `:wrapper:test`: **429 wrapper tests completed, one failed** in the existing lifecycle shutdown test: it assumed runner ownership release meant the separate database-cleanup thread had already closed the database. The failure XML showed a successful database read where `state_closed` was expected, followed by locked SQLite temporary-file cleanup. The test and teardown now wait for `AgentRuntime.databaseClosed()`, then assert that this runner is fully stopped; the test still verifies the original process/generation is alive in `STOPPING` and database reads fail with `state_closed`. No production lifecycle behavior or timeout was relaxed.

The next full run passed the corrected shutdown test and all **70 new credential/admission/migration/runtime cases**, but failed the existing `slowGracefulExitCanFinishAfterTimeoutWithoutKill` test: the OS process had exited before the watcher recorded `last_exit_code`. That test now waits for a nonnull recorded result before asserting exit code **0**, stopped state and exactly one launch, matching the existing crash-result test's observation contract. Production process code and timing are unchanged. This failed run's fresh XML contained core **78/20 suites** and wrapper **429/32 suites**, with one failure, zero errors and zero skipped tests.

The parent GPT‑6.1 Sol session implemented and reviewed the backend and authored every new test. Read-only exploration/design agents implemented no production or test code; Opus remains reserved for UI under the updated user direction.

## Explicit remaining product/release gates

- No Microsoft/Minecraft UUID proof adapter, authenticated device enrollment/possession/mTLS, pack grants or old-grant invalidation mechanism. A password revision can anchor future grant invalidation; no grants are currently issued/revoked by this service.
- No full public/password/Minecraft-policy pack access endpoint, management password account, session, SFTP credential, recovery-passphrase integration, encrypted vault, credential expiry UI, password-strength policy or transport backoff.
- Process-local KDF limits are not persistent per-IP/per-principal throttling, endpoint/body/session/bandwidth caps, global fleet/storage quotas or a protected local recovery transport. Verification/administrative metadata still require careful response shaping in any future remote adapter.
- No production OS-local proof/CLI, independent supervisor, protected named pipes/Unix sockets, separate game identity, canonical-to-legacy advertised identity migration or legacy trust-model removal.
- Legacy plaintext credentials, filesystem/content mutations, connector/OP/loopback/raw-TCP and autostart gaps remain as described in the lifecycle checkpoint. This service does not replace or secure those routes.
- No Linux native execution, real Minecraft/loader matrix, launcher GUI flow, services/firewall, installer, signing, deployment or public exposure certification. Reset/setup/launch scripts and automatic server downloads remain deferred.

Progress commits remain unsigned under prior authorization; nothing is pushed. **Do not expose or deploy the legacy MVP as the completed secure product.**
