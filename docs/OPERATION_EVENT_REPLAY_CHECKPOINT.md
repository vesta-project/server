# Operation event replay checkpoint

Assessment date: **2026-10-05**. This extends the [pack-password checkpoint](PACK_PASSWORD_CREDENTIAL_CHECKPOINT.md) with a **trusted in-process, canonical-authorized, bounded replay of sanitized operation lifecycle events**. It addresses part of the P1 "event cursors / pagination / stable error codes" gap in the [implementation plan](SERVER_SOFTWARE_IMPLEMENTATION_PLAN.md) §15 and requirement `proxies-and-hosting-extensibility-004`. It is **not** an HTTPS endpoint, WebSocket/SSE stream, OpenAPI contract, version negotiation, console/status/player/notification feed or completed P1. Schema remains **4**; no launcher source changed.

## What exists

The durable journal (`EventLog`) and its emitters pre-date this slice: `OperationService` appends `operation.created`, `operation.state` and `operation.progress` in the same write transaction as each operation change, and prefix pruning keeps a persisted retention watermark. Nothing consumed that journal with authorization or bounded windows. This slice adds the consumer and leaves every emitter, the append/prune behavior and the low-level trusted `EventLog.read` unchanged.

## Contract

`OperationEventReplayService` (composed as `AgentRuntime.operationEvents()`; no route, listener or game effect):

- `openReplay(actor, checkpointOrNull, limit)` captures a finite high-water mark and reads the first page after the checkpoint (`null` = start of history). `readPage(actor, cursor, limit)` continues strictly inside that window; later appends never extend it. `currentCheckpoint(actor)` returns an empty, complete page at the current head.
- Every call resolves the **current exact `tasks.view`** capability through `AuthorizationService.resolve(Connection, …)` in the **same read transaction** that validates the cursor, reads journal bounds and selects rows. Nothing is cached; revocation, disablement, expiry, role downgrade and an explicit deny stop the very next page. The `RemoteActor` identifiers must come from a trusted authenticated transport, which does not exist yet; they and the cursor prove nothing. The static limit range is checked first (it reveals nothing about the journal); unauthorized callers then get `forbidden` before any cursor-validity or journal information. `tasks.view` is a server-wide sanitized lifecycle observation right; it does not expose operation inputs, targets, results or diagnostics.
- Limits: 1–100 events returned, at most 500 journal rows scanned per call, cursors ≤256 characters. When the return limit fills, advancement stops at the last *processed* row, so no eligible event is skipped. Pages made entirely of unrelated rows advance without claiming completion until the horizon is reached.
- **Cursor**: versioned (`v1`), canonical URL-safe Base64 text of server ID, position and captured high-water mark. Malformed, oversize, non-canonical, wrong-server, wrong-version, negative/leading-zero/overflowing, inverted or beyond-journal values are `invalid_cursor`. It is **not** a credential or signature and cannot detect replacement of the database by a different history with the same server identity or a restore of an older copy that still lies within the cursor's bounds; a lost journal tail is detected only when the cursor points beyond the observed head.
- **Retention**: a position before the persisted pruning watermark returns `RESYNC_REQUIRED` with the watermark, no events and no continuation cursor. The service never skips missing history silently. A fully pruned journal keeps its head via the watermark. A client that needs a fresh start must obtain a state snapshot and a consistent checkpoint from a future snapshot contract; `currentCheckpoint` alone does not make state and cursor atomic.
- **Projection**: only the exact kinds `operation.created`, `operation.state`, `operation.progress` are decoded, with a strict streaming parser: exact keys, no duplicates, no trailing content, string `operationId` equal to the stored header (lowercase UUID), a known state and an integer progress 0–100. The returned DTO carries a decimal event ID, type, operation ID, timestamp, state and progress and nothing else. Any other journal kind is skipped without being exported or its payload loaded. A malformed recognized event fails the whole page with `event_malformed`, returning no partial page; a persistently corrupt row therefore blocks replay across it until repaired or pruned.
- **Failures** return fixed codes/messages without a cause: `forbidden`, `invalid_request`, `invalid_cursor`, `event_malformed`, `replay_unavailable`, `state_io`, `state_closed`. SQL and runtime faults, including stored text, are not exported. Replay writes nothing and creates no audit or event rows.

`EventLog` gained a transaction-aware `bounds(Connection)` and a bounded `scan(Connection, …)` that truncates header columns in SQL and projects payloads only for requested kinds within the character limit.

## Parent-authored verification

New cases (all authored by the parent GPT‑6.1 Sol session; read-only agents wrote no code or tests): `OperationEventReplayServiceTest` 65, `EventLogBoundedScanTest` 5, `AgentRuntimeEventReplayTest` 2 — **72**. Coverage: a real operation lifecycle replayed in order without inputs/results/progress text; read-only behavior (table counts, audit chain); 13-way canonical authority matrix plus Viewer/Maintainer/Administrator allowed and explicit-deny-wins; revocation/downgrade between pages; 17 forged/ahead/non-canonical cursor shapes for both entry points and no cursor-validity oracle for unauthorized callers; limit boundaries; pagination without duplicates/skips; window stability under a concurrent appender (200 events, contiguous and ordered across windows); unknown kinds, 2 MB unrelated payload, more than one scan page of unrelated rows, journal holes, lost tail; partial and full pruning with resync and head preservation; clock rollback; 21 malformed-payload shapes and 4 header/size violations with fixed errors and no stored-text leakage; corrupted-metadata and failed-SQL redaction; closed database; production runtime composition with no owner; and a trusted-bootstrap runtime that replays a real lifecycle failure without its error text and resumes from a checkpoint across a runtime restart.

Mutation spot-checks: disabling the `tasks.view` check made 16 of the 65 service cases fail; removing the return-limit stop made 6 fail. The unmodified source was restored byte-for-byte.

Final fresh verification on **Windows, 2026-10-05**, after all production and test edits:

| Gate | Result |
| --- | --- |
| Focused new suites | 72 cases passed on the first compile-clean run (one earlier test-compile failure: an ambiguous JUnit `assertEquals` overload with a generic return, fixed by assigning to a primitive). |
| `:core:test :wrapper:test --rerun-tasks --offline --console=plain` | **579 tests / 55 suites passed**: core 78 / 20, wrapper 501 / 35 (previously 429 / 32); zero failures, errors or skipped tests in fresh XML. |
| `node --test scripts/requirements-ledger.test.mjs` / `node scripts/requirements-ledger.mjs --check` | **5 of 5 ledger tests passed** (the first attempt failed one: a partial entry must state a limitation and cite `IMPLEMENTATION_PROGRESS.md`; fixed in the evidence text) and all **370** entries are consistent: **52 partially verified, 318 not verified, zero fully verified**. Only `proxies-and-hosting-extensibility-004` moved (not verified → partially verified) with narrow in-process evidence; no other status changed. |
| `git diff --cached --check` | Passed. |

No launcher checks were rerun: no launcher source changed. Existing Java 8 source/target and deprecation warnings remain. Tests used temporary SQLite state and an unlaunched fake-game configuration only; no real Minecraft server, installer, demo reset, service, firewall, signing configuration or public deployment was touched.

## Explicit remaining gates

- No authenticated device transport, mTLS or session channel supplies a `RemoteActor`; no public/versioned API, OpenAPI document, WebSocket/SSE stream, compatibility negotiation, idempotency-key surface or deprecation policy.
- No consistent state-snapshot-plus-checkpoint contract for resync; no persisted per-principal delivery cursors or notification inbox (P7); no automatic retention/pruning policy.
- No console, status, metric, player, settings or audit event feed, and no per-operation target scoping (observation is server-wide under `tasks.view`).
- The snapshot-atomicity of authorization plus replay rests on a single read callback; it is not separately fault-injected. Cursors do not detect database replacement or an in-bounds restore.
- All earlier release gates stand: independent supervisor and protected IPC, separate OS identities, OS-local owner bootstrap, Microsoft/Minecraft UUID proof, device enrollment, legacy trust-route removal, signed distribution, Linux and native isolation. Do not describe the product as secure, complete or deployable.

Progress commits remain unsigned under prior authorization; nothing is pushed.
