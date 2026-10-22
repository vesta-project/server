# Server / launcher integration review — 2026-10-22

## Scope and checkpoints

Only `server-connections-mvp` (`feat/launcher-compat`) and the server integration in
`launcher` (`feat/frontend-rewrite`) are in scope. Shared launcher auth, transport,
installation, persistence and Tauri infrastructure are in scope where integration
depends on them. Other sibling repositories are excluded.

Local baseline tag in both repositories: `review-baseline-2026-10-22`.

- Server baseline: `752fd17cff14e4286ff935dfc01f8fcb56ece033`.
- Launcher baseline: `24fb0eeb1f3075eb183b3651a5686b3f116608bc`.
- The launcher checkpoint fixes same-server snapshot updates repeatedly loading
  settings, resetting unsaved drafts/errors, and churning topic subscriptions.
  The regression reproduces 61 loads for 60 updates before the fix, one load after.
- Baseline validation: TypeScript passes; 39 focused frontend tests pass. Oxlint
  runs under Bun and exits successfully with existing warnings. The default lint
  script's installed Node 22.14 cannot load its TypeScript config.

## Working method

The parent verifies findings, owns regression tests, prioritizes changes and commits.
Luna gets narrow source scans with separate finding files, no repeated full-repo
onboarding. Findings need a concrete trigger, consequence, source location and a
credible reproduction. Hypotheses stay labeled; test success does not establish
untested platform/network behavior. Each implementation batch records validation.

No production deployment, OS trust-store edits, public exposure, destructive
cleanup or unrelated launcher redesign is implied. Preserve current user data.
Use scratch fixtures for fault injection; coordinate any necessary live restart.

## Review coverage

1. TLS trust, the eMudhra incident, auth/identity boundaries, redirects, secrets,
   downloadable artifacts and update trust.
2. Server process ownership/recovery; sleep/offline/wake; backups and storage;
   SQLite writer contention, lock duration, blocking work and idle overhead.
3. Request admission, backoff, subscriptions, disconnects, cancellation,
   idempotency, slow/unreliable networks, memory/disk/CPU budgets.
4. Launcher reactive state, draft retention, error recovery, IPC payloads,
   multi-window behavior and server-management experience.
5. Measurable adaptive behavior: reduce optional work under pressure, protect
   lifecycle controls, preserve correctness rather than silently dropping effects.

## Initial evidence

### R01 — Repeated settings loads on status snapshots (fixed at baseline)

Confirmed with installed Solid 2 diagnostics. A raw ID accessor passed to an
effect still reruns when its parent snapshot changes. A memoized identity gates
policy loads and subscriptions, preserving drafts and visible failures. The same
pattern was fixed in Settings, Sleep/MOTD, Access, profile, logs, trash and topics.

### R02 — eMudhra trust exposure (investigation)

Primary incident report: https://bugzilla.mozilla.org/show_bug.cgi?id=2083449

Upstream verifier patch: https://github.com/rustls/webpki/releases/tag/v%2F0.103.16

Upstream root update: https://github.com/rustls/webpki-roots/releases/tag/v%2F1.0.10

The incident includes compromised intermediate keys and cross-signs. OS trust
updates do not establish the safety of applications using independent roots or
verifiers. Inventory actual resolved dependency graphs and client construction
sites before choosing a mitigation. Pinned Vesta agent identity and public-CA
provider/auth/download connections need separate analysis.

## Plan status

Checkpoint complete. Source scans are recorded alongside this ledger and in
`launcher/docs/reviews/2026-10-22-launcher-integration-findings.md`.

## Host target and constraints (owner, 2026-10-22)

Target comfortable operation on 1–2 cores and 2–4 GB RAM, preserving resources for
Minecraft. Favor low idle work, bounded memory/queues and adaptation to pressure.
Breaking internal changes are acceptable before release; disruptive architecture,
deployment or data changes still need the owner's later approval. Keep progress
and unresolved questions on disk. Do not equate a debug Windows measurement with
release performance on low-end Linux/ARM or with Minecraft's own memory needs.

## Implementation plan — first pass

1. **P0 trust:** update affected Rust verifier/root dependencies in both launcher
   lockfiles and the server lockfile. Move the pinned management client off its
   unsupported TLS branch, preserving leaf pins and handshake signature checks.
   Use a patched Rust TLS path for integration's public Mojang proof and server
   artifact downloads. Record remaining OS/WebView trust dependencies explicitly.
2. **P1 host protection:** bound aggregate request-body ownership independently
   of connection count; give TLS negotiation its own deadline. Keep authenticated
   lifecycle controls separate from bulk transfers and bound work *before*
   allocation/dispatch. Exercise slow clients and cancellation against fixtures.
3. **P1 storage correctness:** fix backup manifest double charging. Verify the
   listing record is also covered by the byte budget before publishing a snapshot.
4. **P1 recovery/persistence:** verify and fix worker exit after transient local
   credential errors, unbounded terminal intent retention, and removal ordering.
   Audit telemetry-triggered synchronous metadata writes; remove unnecessary
   writes without weakening durable mutation journals or enrollment safety.
5. **P2 frontend resilience:** latest-response guards, stable identity dependencies,
   bounded read retry with server cooldown guidance and cleanup on disposal for
   automation history/details. Never automatically replay uncertain mutations.
6. **P2 transfer lifecycle:** reclaim staged uploads on terminal/expiry/window
   cleanup while preserving retry receipts, and add a real download cancellation
   route if it can be integrated without a task-system redesign.
7. Validate each batch with focused regressions; run relevant integration/type
   checks once after changes settle. Record measurements, remaining gaps and an
   experience roadmap. Commit logical batches; no pushes or unsolicited restarts.

## Second plan review — parent challenge pass

Reviewed against the source findings, requirements and the low-end host target:

- Root removal alone cannot block cross-signed compromised intermediates: the
  verifier update is mandatory. Pinned agent TLS is not public-CA validation, so
  preserve exact DER pins and test both HTTP and WebSocket failures. Avoid a broad
  unrelated launcher HTTP migration; native OS clients require vendor trust updates.
- A body permit released when an HTTP future is dropped is insufficient: blocking
  dispatch cannot be aborted. Move its permit with the erasing body into the
  blocking closure and hold it through completion. Empty GETs must not compete
  with bulk body readers. Fixed memory bounds are safer than RAM-driven increases.
- A short TLS timeout improves recovery but does not prevent sustained admission
  exhaustion. Keep that residual limitation visible; do not promise DoS immunity.
- Backup listing bytes were omitted in addition to the manifest arithmetic bug.
  Admit manifest plus listing together before either write; test exact/one-byte-
  short boundaries and compare actual store growth with reported bytes.
- Journal pruning must retain uncertain/nonterminal intents and a documented retry
  horizon. A hard capacity is a bound, not adequate lifecycle management. Removal
  must save metadata before deleting credentials, keeping cleanup recoverable.
- Upload completion with a lost response must remain retryable. Terminal receipts
  can outlive staged bytes; periodic cleanup must have bounded work and no lock
  held across network IO. Download cancellation must stop before atomic publication.
- Telemetry persistence and adaptive polling need measurements and source evidence;
  do not change SQLite durability, shrink security logs or introduce a new executor
  merely to improve a synthetic number. Existing channel streaming is appropriate.

Plan accepted for incremental implementation. Newly discovered evidence may refine
individual fixes; record the reason and validation here before broadening scope.

## Technical research (primary sources)

- Rust verifier distrust: https://rustsec.org/advisories/RUSTSEC-2026-0382 and
  https://github.com/rustls/webpki/pull/546 (includes cross-sign path checks).
- Root bundle: https://github.com/rustls/webpki-roots/releases/tag/v%2F1.0.10.
- Tokio blocking tasks cannot be aborted once running; admission must outlive a
  cancelled waiter: https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html.
- Unbounded task queueing is not backpressure:
  https://tokio.rs/tokio/tutorial/channels.
- Tauri recommends channels for streaming:
  https://v2.tauri.app/develop/calling-frontend/.
- SQLite WAL NORMAL can lose commits on power failure. Preserve durability and
  remove needless transactions instead: https://www.sqlite.org/wal.html.

## Resume notes

### Implementation checkpoint A

- Server `774b2bf`: patched verifier 0.103.16 and roots 1.0.10; installer moves
  from native reqwest 0.11 TLS to Rust TLS 0.12.28. 168 installer tests pass.
- Launcher `cd67d95e`: pinned client moves to reqwest 0.12 / rustls 0.23 /
  tungstenite 0.24, preserving real handshake signature verification. Both app
  lockfiles pass the new trust regression gate. Legacy OAuth feature selection
  no longer pulls rustls 0.21/obsolete roots; existing native TLS clients remain
  OS trust dependent. Integration Mojang proof explicitly uses patched Rust TLS.
  Truncated established TLS response bodies remain resumable with strong ETags.
  50 focused transport/management/policy/scheduler tests and native rewrite check
  pass (native check retains pre-existing unused/dead-code warnings).
- Server `529d7e8`: snapshot manifest and listing admitted once before publication;
  exact/one-byte-short quota test and actual store growth accounting pass, as do
  all 13 engine tests.
- Server `0e75165`: fixed 4 MiB aggregate body reservation, retained through blocking
  completion, plus independent five-second TLS negotiation deadline. Real HTTPS
  pressure/cancellation and incomplete-ClientHello tests pass. 42 transport unit
  tests and workspace all-target check pass. Sustained adversarial connection
  flooding still requires deployment-level protection.
- Server `bddb114` only formats previously committed integration changes, restoring
  the repository formatting gate; no behavior changes.
- Commits in this review after the baseline are unsigned local commits: GPG waited
  on interactive agent startup/lock state. No global signing setting changed.

Next: launcher persistence/reconnect/journal lifecycle; frontend read recovery;
staged-file ownership/cancellation; then consolidated validation and resource
measurements. No live binary restarted or OS trust store modified in this review.

### Implementation checkpoint B (2026-10-23)

- Launcher `6999ad85`: view-owned automation reads use stable keys, latest-response
  guards, disposal/obsolete-pagination cancellation, one retry timer, Retry-After
  guidance and capped exponential backoff. Visible failures stay until recovery.
  Successful list polling scales with pages read (at most 12 list requests/min).
  41 focused frontend tests pass, including installed Solid diagnostic captures
  for draft retention, out-of-order responses, disposal and transient recovery;
  TypeScript passes. This is not a full visual/platform acceptance pass.
- Launcher `6045bf91`: ordinary telemetry stays in memory rather than cloning,
  serializing and fsyncing the profile each observation. Meaningful state changes
  still persist. Disk commits move off the async worker, with an owned transaction
  that commits memory too even if its waiter is cancelled. Removal saves a durable
  vault-cleanup record first; vault failure cannot strand a listed credentialless
  connection. Seven native store/credential tests pass (one opt-in live diagnostic
  is intentionally ignored). A 60-observation fixture performs zero file changes.
- Worker recovery now retains demand through transient credential construction
  errors, with 1/2/4/.../60-second backoff. Trust/access failures stay blocked.
  Status-only worker wakeups reduce from 10/s to 1/s; console remains 10/s and
  metrics-only 4/s. Policy tests and native rewrite check pass; real vault-outage
  recovery still needs a packaged integration exercise.
- Upload staging now has a 64 MiB aggregate reservation, including file growth
  while selected. Success/abort releases staged bytes but retains small retry
  receipts; idle sweeps expire abandoned handles once/minute after the existing
  one-hour lifetime. Unit tests cover pressure, actual temp-file deletion and
  shared owners; all 22 real pinned-HTTPS fixtures pass after the change.
- **Plan refinement / deferred policy:** do not automatically compact the intent
  journal yet. Some single-use prepare receipts look terminal but must permanently
  prevent replay after restart. A time-only deletion policy could weaken that
  invariant. The 4,096 lifetime cap remains a confirmed concern. Design a bounded
  anti-replay tombstone/intent-epoch policy for owner review before changing it;
  this is a security contract change, not permission to evict uncertain history.

Next: download cancellation with an explicit atomic-publication boundary; focused
validation, measurements and final experience recommendations. Avoid live restarts
while the owner is away.

The development agent is the updated staged binary at
`%LOCALAPPDATA%/Temp/vesta-dev-server/bin/vesta-agent.exe`. It was restarted cleanly
after a graceful Minecraft stop. Real authenticated management reads returned 200
in 35–73 ms at that point. Do not assume those timings cover sleep transitions or
slow hardware. The current frontend checkpoint is loaded by the running dev server.

Next: narrow Luna scans; dependency trust inventory; technical primary-source
research; validate and rank findings; write/review plan; implement in small commits.
