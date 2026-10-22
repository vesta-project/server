# Server runtime review findings — 2026-10-22

Scope: source-only review of sleep/runtime/state contention, recovery/process ownership,
and backup correctness under resource pressure. No tests, production edits, commits, or
live operations were performed.

## SR01 — Backup manifest byte budget is counted twice

- **Location:** `crates/vesta-control/src/backups/engine.rs:495-497`
- **Trigger:** A backup reaches the manifest write with newly stored chunk bytes plus
  the compressed manifest fitting the remaining configured byte budget exactly (or
  nearly exactly).
- **Impact:** `summary.data_added` is incremented by `object.len()` and then that
  already-incremented total is passed to `within(..., object.len())`, charging the
  manifest a second time. `LimitReached` can be returned even though the actual
  repository writes fit the budget. The already-uploaded chunks are left unreferenced
  for later pruning, and the user sees a false storage-limit failure.
- **Confidence:** High; direct arithmetic error in the write path.
- **Fix idea:** Check `within(summary.data_added, object.len())` before incrementing
  `summary.data_added`, then add the manifest length only after admission succeeds.
- **Suggested regression:** Use a deterministic store and a backup where new chunk
  bytes plus the compressed manifest equal the budget; assert success and exact
  `data_added`. Also assert one byte less returns `LimitReached` without a snapshot.

## SR02 — Eight incomplete TLS handshakes can monopolize admission for five minutes

- **Location:** `crates/vesta-transport/src/lib.rs:71, 853-864`; timeout maximum is
  `crates/vesta-control/src/config/mod.rs:76, 641-642`.
- **Trigger:** An unauthenticated peer opens eight TCP connections, begins TLS, and
  withholds the rest of each ClientHello. The accepted tasks hold all eight handshake
  permits while `tls.accept(socket)` waits; the watchdog uses the configured request
  timeout, which is allowed up to 300 seconds. New accepted sockets are immediately
  dropped when `try_acquire_owned` cannot obtain a handshake permit.
- **Impact:** A small unauthenticated slow-client burst can make the listener refuse
  every new TLS handshake for as long as five minutes, including legitimate users.
  The global concurrency cap bounds memory but converts a tiny number of slow peers
  into a long service-wide outage.
- **Confidence:** High; the accept path, permit lifetime, watchdog, and maximum timeout
  establish the behavior directly.
- **Fix idea:** Give TLS negotiation a short independent hard deadline (separate from
  the configurable HTTP request timeout), or avoid holding all handshake slots for
  the full request timeout.
- **Suggested regression:** Open `HANDSHAKES` sockets and send partial TLS ClientHello
  bytes without completing them; verify a legitimate handshake is still admitted
  within the short handshake deadline even when `request_timeout_seconds` is maximal.

## SR03 — Request-body buffering is bounded per connection but not in aggregate

- **Location:** `crates/vesta-transport/src/routes.rs:224-245`; connections are
  admitted up to the configured cap in `crates/vesta-transport/src/lib.rs:851-856`;
  body size and connection maxima are `crates/vesta-control/src/config/mod.rs:74-75,
  638-642`.
- **Trigger:** Many clients send valid, maximum-sized request bodies concurrently to
  endpoints that require a body. The transport reads each entire body into a separate
  `Zeroizing<Vec<u8>>` before it tries the two-slot blocking-job admission in
  `routes.rs:256-269`. Authentication/authorization for management mutations runs
  in the later blocking dispatch, so an unauthenticated caller can occupy this memory
  before rejection.
- **Impact:** The configured maxima permit up to 1 MiB per body and 1,024 live HTTP
  connections, allowing roughly 1 GiB of body buffers (plus TLS/Hyper/task overhead)
  before the two-worker cap provides backpressure. On a modest host this can cause
  memory pressure or process termination, taking the server and management interface
  down together.
- **Confidence:** High for the lack of aggregate accounting and possible allocation;
  actual OOM threshold depends on host memory and deployment limits.
- **Fix idea:** Acquire a global in-flight body-byte or bounded body-read permit before
  consuming frames, with a small aggregate budget independent of `max_connections`.
  Reject or defer reads before allocating more buffers when the budget is exhausted.
- **Suggested regression:** Configure a small aggregate body budget and concurrently
  stream multiple max-sized bodies; assert only requests within that budget are
  buffered and excess requests receive a bounded refusal without growing retained
  body memory.

## Independent review of server fixes (2026-10-23)

Reviewed commits `774b2bf`, `529d7e8`, and `0e75165` against the integration ledger.
This was source-only; no tests or live operations were run.

- **SR01 resolved:** `write_snapshot` now admits the manifest and serialized listing
  together before either is written, fixing the double charge and the previously
  omitted initial listing bytes.
- **SR02 materially mitigated:** TLS negotiation now has a separate five-second
  timeout, independent of the configurable HTTP timeout. A peer can still repeatedly
  refill the eight handshake slots, so the ledger's caveat about sustained flooding
  and deployment-level protection remains accurate.
- **SR03 resolved:** body readers reserve their configured maximum from a fixed 4 MiB
  semaphore before buffering. The permit and erasing buffer move into the blocking
  closure, so an HTTP timeout does not release accounting while `spawn_blocking` work
  still owns the bytes. Empty bodies avoid the permit. The inspected cancellation and
  ownership path is coherent.

## SR04 — Post-validation backup tag promotion grows the listing outside the quota

**Parent remediation:** `e3190ac` adds bounded tag promotion/rejection, checks net
growth and full temporary replacement space separately, and includes growth in
the job summary. All 37 backup tests passed, including exact/one-byte-short
promotion budgets. This finding is resolved; repository-index memory remains a
separate capacity concern in the final review results.

- **Location:** `crates/vesta-control/src/backups/mod.rs:989, 1018-1023`;
  `crates/vesta-control/src/backups/engine.rs:498-523, 547-559`.
- **Trigger:** A backup consumes the admitted byte budget down to its boundary, then
  passes source-consistency checks and is promoted. `backup_within` writes and charges
  the `candidate=<profile>` listing. `Backups::backup` subsequently calls `tag_add`
  to append `profile=<profile>` while retaining the candidate tag; the rewritten tag
  object is therefore larger than the listing size admitted by `write_snapshot`.
  The rejected path also appends a tag after admission.
- **Impact:** The final repository can exceed `max_repository_bytes` or the local
  free-space reserve by the post-validation tag growth, and `data_added` omits those
  bytes. The overshoot is small per job, but repeated boundary-sized jobs can cross a
  configured hard quota despite the new exact initial manifest/listing accounting.
- **Confidence:** High; candidate tags are written by `backup_within`, and `tag_add`
  serializes the retained tags again after the bounded operation returns.
- **Fix idea:** Include the final tag record size in the budget before writing the
  candidate snapshot (or provide a bounded promotion operation that checks and
  accounts for the replacement record). Report the actual final metadata bytes in the
  job summary.
- **Suggested regression:** With a deterministic store, set the budget to fit chunks,
  manifest, and candidate listing exactly but not the promoted listing; assert the
  backup does not publish a profile snapshot that exceeds the budget and that reported
  bytes match final store growth.

