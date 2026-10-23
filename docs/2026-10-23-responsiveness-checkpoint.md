# Backend responsiveness checkpoint — 2026-10-23

Implemented with the launcher integration update on `feat/frontend-rewrite`.

- Reserve one of the existing two HTTP blocking workers for closed-list stop/kill/restart/session-auth routes. Ordinary IO retains one worker; cancelled waiters retain permits until synchronous work actually finishes.
- Reserve 16 KiB of the existing 4 MiB aggregate body budget for these routes, with an 8 KiB per-control-body cap. Authentication, current authorization, rate limits and idempotency still apply.
- Add bounded exact-ID operation observations: `GET /api/v3/manage/operations?ids=...`, at most 32 unique IDs, one database/current-authority snapshot. Missing records are omitted and never treated as success. Existing history behavior is unchanged.
- Quiet running-game driver waits at most 25 ms rather than 5 ms when there is no active operation or pending player/tick writer; admitted work wakes it immediately. Active work retains its existing cadence.
- Verify that whitelist management observations reread disk rather than predicting command effects. `stdin_written` remains delivery evidence, not proof Minecraft applied the change.

Validation: transport library 45 tests, runner 41 tests (two threads), fresh-whitelist regression one test, and the complete transport HTTPS suite 111 tests (one thread) passed. Control/transport strict all-target Clippy and touched Rust formatting passed.

Parallel HTTPS fixture setups initially hit the shared password-authentication gate: 18 failures at default parallelism and five with two threads. Serial execution passed all 111 without changing production limits. An initial parallel runner run had one sleep/login-burst failure; isolated and two-thread runs passed. Fixture isolation under parallel load remains a testing concern.

No live server was restarted or benchmarked. Reserved capacity cannot bypass exhausted rate limits, failed authentication, unavailable networks or a blocked game main thread. Ordinary read throughput may decrease under contention because its worker budget is partitioned. Measure control latency, console latency, idle CPU/wakeups and memory on actual 1–2-core / 2–4-GB hosts before claiming performance gains.

The launcher contains the full review, protocol integration and remaining limits in `docs/reviews/2026-10-23-backend-responsiveness-results.md`.
