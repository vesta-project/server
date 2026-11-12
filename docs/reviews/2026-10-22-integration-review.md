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

### Implementation checkpoint C

- Launcher `0190ab8c`: downloads have bounded, window-owned cancellation handles
  exposed through the existing Activity cancel action. An atomic state transition
  decides cancellation versus final destination publication; publication already
  admitted cannot be reported cancelled. Window destruction cancels pending work,
  while hide-to-tray preserves it. Nine client unit tests and all 23 pinned-HTTPS
  fixtures pass, including mid-stream cancellation, preserved destination, temp
  removal, released bulk admission, pre-cancel and successful subsequent download.
- Launcher `b884c871`: credential deletion maintenance drains concurrent removals
  and retries failed vault cleanup once/minute. Eight native store/credential
  tests pass, one live-profile diagnostic intentionally ignored; strict client
  all-target Clippy passes. Changed frontend files pass focused lint; 46 activity
  tests pass (the old harness prints its pre-existing multiple-Solid warning).
- Independent Luna review found and parent fixed post-check backup tag growth in
  server `e3190ac`. Metadata promotion checks both net growth and full temporary
  replacement space, and job accounting includes final listing growth. All 37
  backup unit tests pass. Rejected wakeup hypothesis is explicitly labeled in the
  launcher findings: installed Tokio guarantees `notify_waiters` wakeups from
  `Notified` creation, even before first poll.
- Broader server HTTPS run: 108 pass / 3 invitation failures. Investigation proved
  the debug dependency cache contained schema version **24** while current source
  declares **25**. Fresh release compilation was unaffected. Invalidated workspace
  Rust source mtimes (no source content or dependency cache deleted) and rebuilt;
  both focused invitation tests now pass. Added a startup table regression. This
  is stale build evidence, not a production migration defect; revalidate affected
  scenarios after rebuilding rather than trusting cached binaries.
- Isolated release idle sample, Windows, pinned to CPU 0, no game/clients, loopback
  TLS: 20.011 s wall / 0.65625 s CPU (3.279% of one core), 24.633 MiB working set /
  3.445 MiB private. Repeat: 15.002 s / 0.484375 s (3.229%), 12.992 MiB working set /
  3.770 MiB private. Fresh owned fixture each run; retained under the temp directory.
  Binary SHA256 `C281E2B264531B0281F379A7157E3D67F9FB1925475DD67A26A02952D51DE267`.
  These are baseline observations on this host, not proof of a low-end Linux/ARM
  envelope. Twelve threads; platform thread descriptions were unavailable.

Next: reduce measured idle polling without slowing active sleep/wake or controls,
then rerun the isolated sample and record the remaining release/experience plan.

### Historical live checkpoint (before the overnight changes)

The following live observation predates the overnight work. It is not evidence
that the newly committed binaries have been installed or tested in the running UI.

The development agent is the updated staged binary at
`%LOCALAPPDATA%/Temp/vesta-dev-server/bin/vesta-agent.exe`. It was restarted cleanly
after a graceful Minecraft stop. Real authenticated management reads returned 200
in 35–73 ms at that point. Do not assume those timings cover sleep transitions or
slow hardware. The current frontend checkpoint is loaded by the running dev server.

### Final implementation checkpoint D (2026-10-23)

- Server `6fc111d`: disabled sleep policy observation reduces from four times/second
  to once/second; active sleep/offline cadence stays 250 ms. Shutdown unparks the
  driver before joining. All nine sleep tests pass. The owned release fixture
  script is committed for repeatable measurements without touching the dev server.
- After tuning: 20.011 s wall / 0.50000 s CPU = 2.499% of one core, working set
  12.906 MiB / private 3.711 MiB. SHA256
  `8B13E8267A379A641989D8F852C808584588DDEEBADB96809CB6C9BE4C0823AE`.
  Fixture `vesta-idle-review-0f12c2df8cdb4d3b93041937f8c4ed94` under Windows temp.
  Observed improvement is about 24% versus the first baseline, with host/cache
  noise. This is not a target-machine guarantee; remaining idle CPU needs work.
- Quiet repeat after all checks: same release SHA256, 20.016 s wall / 0.46875 s
  CPU = 2.342% of one core, working set 13.031 MiB / private 3.984 MiB, 12 threads.
  Fixture `vesta-idle-review-1712fd35ed784ea28939d11546b0bf95`. The two after
  samples give an observed 24-29% reduction versus the first before sample;
  retain the limitations above rather than advertising a universal speedup.
- Launcher `83f03c9e`: progress uses time rather than a 512 KiB byte threshold,
  bounding fast-link display updates to ten/second plus initial/final callbacks.
  Small slow-link chunks can report without waiting for 512 KiB. Ten client unit
  tests pass and all 23 pinned-HTTPS fixtures pass; strict client all-target Clippy
  passes. The cancellation fixture initially failed after throttling exposed that
  its stalled-stream branch was on the content route, not the download route;
  corrected the fixture and verified real mid-stream cancellation.
- Server `3c38305`: startup invitation inventory assertion. After rebuilding stale
  workspace debug artifacts, the full HTTPS suite passes **111/111** (369.95 s,
  serial). Strict workspace all-target Clippy and formatting pass on fresh source.
- The [review results and experience roadmap](2026-10-23-review-results.md) capture
  the remaining journal-policy decision, large-backup metadata memory exposure,
  host/platform acceptance matrix and release trust limits. These are not hidden
  by the completed small-host improvements.
- Final launcher checks: 107 focused frontend tests and TypeScript pass; ten
  client unit, 11 management, 12 policy, five scheduler and eight native store/
  credential tests pass (one explicit live-profile diagnostic ignored), as do
  the 23 HTTPS fixtures. Strict client Clippy and two lockfile trust gates pass.
- Broader validation found a pre-existing supervisor crash test that accepted
  only observation failure, not the offline-release failure that can win the
  same dead-channel race. `b1407d7` accepts those exact outcomes, adds an explicit
  assertion that the orphan survives shutdown, and preserves the duplicate-launch
  and deliberate-exit checks. All three supervisor end-to-end tests pass.
- `b2bcce1` retires discarded prototype test sources from nine requirement entries
  while preserving historical notes, and corrects a validator that demanded the
  oldest archive even for current setup/probe evidence. All 166 top-level Node
  tests pass; requirement-ledger and roadmap freshness checks pass. No unrelated
  repository or historical files were modified.

- Consolidated fresh-source Rust validation completed successfully:
  `cargo test --workspace --exclude vesta-agent --exclude vesta-transport --locked
  -- --test-threads=2` passes, including all **924 control unit tests**, backup and
  recovery integrations, 168 installer tests, 16 setup tests, state migrations,
  process ownership and supervisor suites plus doctests. Agent unit/CLI suites
  passed in the preceding broader run; the one overly narrow supervisor assertion
  was corrected and its full three-test suite rerun successfully. Transport's
  **42 unit tests** and **111 HTTPS tests** pass separately to avoid rerunning the
  six-minute TLS suite without a new concern. Final strict workspace Clippy,
  formatting and `git diff --check` pass.

No live binary was restarted during the overnight work. No implementation task
or delegated scan remains running. The remaining release/platform checks and
owner policy decisions are listed in the results rather than claimed complete.
