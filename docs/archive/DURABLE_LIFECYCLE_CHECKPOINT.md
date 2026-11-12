# Canonical confirmation and durable lifecycle checkpoint

Assessment date: **2026-10-05**. This extends [canonical authority/password](OPERATION_AUTHORITY_PASSWORD_CHECKPOINT.md) with an opt-in composition root and real child-process lifecycle handlers. It is **not an independent supervisor, remotely usable management API, or completed secure hosting product**. Schema remains 3. Companion launcher evidence is in `launcher-server-mvp:docs/VESTA_IMPORT_LIMITS_CHECKPOINT.md`.

## Implemented boundaries

### Canonical confirmations

`PolicyConfirmationService` accepts an already authenticated `RemoteActor`, an audit source label, an operation ID, and either a TTL or presented token. It never accepts client assertions for expected action, target, payload hash, or source revision.

- One database write transaction contains current operation ownership/state, hash-verified canonical input, trusted binding/target/lock/contract integrity, exact current principal/device authority, server-owned source revision, token insertion/consumption, operation transition, audit and applicable events.
- `PolicyOperationAuthority` also now evaluates its ordinary runner recheck in one read snapshot. Connection-taking internal methods avoid nested transactions.
- Null/malformed/system callers, revoked/disabled/denied identities, unknown or altered operations, and invalid/unavailable/stale revisions fail closed. Provider SQL/runtime failures produce an audited bounded denial; a database/audit failure still rolls back.
- Rejected consumption burns the presented live token in the same transaction. Exceptions are reported only after rejection audit commits. Audit failure rolls back burns, transitions and events. Tokens remain single-use; only their hashes are persisted.
- Source labels such as `loopback` grant nothing. No transport authenticates actor possession yet. The old raw confirmation/submission APIs remain trusted in-process APIs, not permitted remote boundaries.

### Generation-bound lifecycle

`LifecycleBindings` registers three operations against `MinecraftServerProcess`:

| Type | Input / target | Authority / safety |
| --- | --- | --- |
| `game.start` | Exactly `{}` / `game` | `lifecycle.start`; non-idempotent; no confirmation; an already live game is a no-effect result |
| `game.stop` | `expected_generation` / `game:generation:<id>` | `lifecycle.stop`; idempotent only because replay cannot stop a replacement generation; never force-kills |
| `game.force_kill` | Same generation-bound shape | `lifecycle.force_kill`; non-idempotent; exact confirmation required |

All share `game.generation`. The strict flat-object parser rejects unknown/duplicate fields, wrong types and trailing JSON. Generation IDs contain 128 random bits, not a PID. One immutable volatile pair binds the ID to the actual live process.

Generation validation, fresh effect authority and the actual mutation run under the process monitor. Exit waiting does not hold that monitor. Force-kill confirmation uses the actual live `game-generation:<id>` revision; execution rechecks again after confirmation, so a replacement cannot be killed by an old request/token.

Stop timeout leaves the same process alive and visibly `STOPPING`, with no kill or replacement launch. Known no-effect launch failures are distinct from failures after process creation: the handle/generation is published immediately after creation, and an unknown non-idempotent outcome requires recovery rather than replay. A signalled force kill whose exit is unobserved also requires recovery and retains locks. The post-launch setup-failure and unobserved-kill branches were reviewed but were **not independently fault-injected** by this checkpoint's tests.

### Opt-in composition and fail-closed configuration

```toml
[state]
enabled = true
# Optional explicit directory outside both game and scan roots:
# directory = 'C:\\VestaPrivateState\\my-server'
```

Default is **disabled**, preserving explicitly labelled legacy compatibility behavior. The default state directory is `<user.home>/.vesta/state/<legacy-config-server-id>`; overlap with either game-visible root rejects rather than silently relocating it. Production local bootstrap authority is `UNAVAILABLE` until real OS-authorized proof is implemented.

`AgentRuntime.open` composes database, audit/events, identities/policy, bindings, submissions, confirmations and runner reconciliation. It never starts/stops Minecraft. It opens before listeners/legacy autostart. Raw selected state ancestors are checked for links/junctions before canonicalization/writes and rechecked after open; these are best-effort checks, **not native TOCTOU confinement**.

- Existing malformed, unreadable, nonregular or linked configuration fails instead of silently selecting defaults. `[state]` allows only exact `enabled` boolean and nonblank `directory` string keys. Errors do not echo configuration contents or parser causes. A missing file still selects documented legacy defaults.
- Durable mode refuses non-GET legacy power and console requests with `409 legacy_lifecycle_disabled`, before loopback/OP/password shortcuts. No replacement HTTP operation route is introduced.
- Shutdown stops the runner before closing its database. Workers that outlive bounded shutdown retain ownership/state until they actually exit, with deferred database close. Partial-open failure uses only that runner's actual ownership, never thread-name heuristics or other runtimes' workers. A never-started/failed-lock runner owns nothing after close and cannot subsequently start.
- The wrapper's shutdown hook skips its legacy game stop while durable workers remain active. Once they stop, the existing full-wrapper shutdown still requests Minecraft stop; this is not an agent-only continuity guarantee.

## Parent-authored verification

Final fresh server command:

```sh
./gradlew :core:test :wrapper:test --rerun-tasks --offline --console=plain
```

**437 tests / 48 suites passed**: core 78 / 20, wrapper 359 / 28; **zero failures, errors or skipped tests** in fresh XML. This is 71 additional server cases beyond the previous 366-test checkpoint: canonical confirmation 30, lifecycle composition 24, configuration 13, route suppression 1, and runner cleanup 3. All five Node ledger tests and the 370-entry consistency check passed. Ledger statuses remain **51 partially verified / 319 not verified / zero fully verified**; new evidence extends existing partial entries, not whole-requirement completion.

Companion fresh Rust workspace: **620 passed / 7 pre-existing ignored**, including 12 parent-authored import-budget tests. Frontend sources are unchanged and their checks were not rerun for this slice. Targeted Rust formatting and both repositories' `git diff --check` passed.

All new repository tests were authored by the parent; Opus implemented production code and review corrections. Tests use real SQLite, private test-only local-proof fixtures, temporary child JVMs running `FakeMinecraft`, temporary HTTP listeners, and a native Windows junction. They do not download Minecraft or touch the demo world.

New suites: `PolicyConfirmationServiceTest`, `AgentRuntimeLifecycleTest`, `DurableConfigLoaderTest`, `DurableLifecycleRouteTest`; shared fixture: `CanonicalOperationFixture`. Three additional `OperationRunnerTest` regressions cover never-started close, competing-lock failure, and audit failure during startup recovery. Existing uncooperative-worker ownership tests remain applicable.

The first focused run had two fixture readiness/timeout failures; the parent fixed readiness to wait for each actual new JVM and gave graceful fixtures a suitable timeout. The next full attempt found a checked-exception declaration in the parent helper; it was narrowed to `IOException`. An ensuing fresh **433-test** run passed before final ownership-cleanup regressions. Initial failures are not successful verification.

## Explicit unresolved product and release gates

- **No independent supervisor or protected named-pipe/Unix-socket protocol.** Minecraft remains a child of the wrapper JVM; agent crash/restart continuity, adoption, durable log offsets, real game readiness and separate OS identities are unimplemented.
- No production OS-local proof, owner enrollment, device possession/mTLS, session/vault/credential persistence, versioned API/OpenAPI, management UI, hosting/onboarding, or network confirmation flow.
- Legacy filesystem/content mutations can still alter launch code/config/mods; loopback security-setting mutation and connector/OP/raw-TCP flaws remain. Legacy autostart and internal player-query `list` are outside typed operation audit. Disabling two legacy routes does not secure the rest of the product.
- Persisted canonical database server ID is distinct from the old advertised configuration ID. Moving a path-derived legacy scan identity can change its default state location. Identity unification/migration is not implemented.
- Same-user native confinement, external audit checkpoints, global budgets/reservations, signed workspace/publication, backups/restore/workflows, service/install/update and P4–P8 release work remain.
- No real Minecraft/loader matrix, Linux native tests, Windows service/deployment, signing, GUI hosting or public exposure verification occurred. Downloads and reset/setup/launch scripts remain deferred; no real demo reset or server launch was executed.

The requirement ledger remains narrow partial evidence, never whole-product completion. **Do not expose or deploy this legacy MVP as the completed secure product.** Progress commits are unsigned under prior authorization; nothing is pushed.
