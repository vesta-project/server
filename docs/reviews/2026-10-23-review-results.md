# Server and launcher integration: review results

Date: 2026-10-23. Scope: `server-connections-mvp` on `feat/launcher-compat`
and server integration in `launcher` on `feat/frontend-rewrite`.

## Assessment

The main risks were work and state accumulating across otherwise ordinary use:
request bodies competing for host memory, idle polling and synchronous profile
writes consuming background resources, temporary failures stopping subscriptions,
and transfers retaining files or providing no effective cancellation. These are
more useful fixes for a 1-2 core / 2-4 GB host than increasing concurrency.

The reviewed implementation now has tighter admission and ownership, explicit
recovery, and less optional work. This is a targeted source and regression review,
not certification of every requirement or platform. The owner's minimum host is
the agent target; Minecraft's version, world and modpack still determine whether
the whole installation fits that host.

Baseline tags in both repositories: `review-baseline-2026-10-22`. The
[working ledger](2026-10-22-integration-review.md) contains the plan, parent second
review, measurements and incremental validation. Narrow Luna scans and a strict
follow-up were independently checked by the parent; one reported cancellation
race was rejected after checking the installed Tokio implementation.

## Changes implemented

| Area | Confirmed issue and resulting behavior | Evidence / commits |
| --- | --- | --- |
| Public TLS trust | Root removal alone did not address compromised cross-signed intermediates. Resolved Rust verifier and roots are patched; management HTTP/WebSocket transport moved off the older Rust TLS branch without weakening exact agent pins or handshake signatures. Installer downloads and integration's Mojang proof use the patched Rust TLS path. | Server `774b2bf`; launcher `cd67d95e`; lockfile regression gates in CI; real pinned HTTPS/WebSocket fixtures. |
| Host memory | A 1 MiB body limit combined with 1,024 connections permitted about 1 GiB of request buffers. A fixed 4 MiB aggregate reservation now precedes buffering and stays owned by blocking dispatch after HTTP cancellation. Empty GETs do not reserve body memory. | Server `0e75165`; pressure, cancellation and recovery fixtures. This bounds body buffers, not total process RAM. |
| Slow TLS clients | Partial handshakes could retain all handshake slots for a configurable five minutes. Negotiation now has an independent maximum five-second deadline. | Server `0e75165`; eight partial handshakes plus legitimate recovery fixture. Sustained refill remains a deployment concern. |
| Backup storage | Manifest bytes were charged twice, initial listing bytes were omitted, and final promotion could grow metadata after the quota check. All stages now admit their actual bytes; promotion checks net growth and full temporary replacement space separately. | Server `529d7e8`, `e3190ac`; exact / one-byte-short quota and actual-growth tests. |
| Settings stability | Same-server snapshots repeatedly reloaded configuration, replacing drafts and clearing errors. Memoized identity prevents unrelated status updates from reloading view state or churning subscriptions. | Launcher baseline `24fb0eeb`; installed Solid diagnostics reproduce 61 loads before / one after for 60 updates. |
| Read recovery | A temporary automation-history error stopped polling; older detail responses could overwrite newer state. View-owned reads have latest-response/disposal guards, one retry timer, capped exponential backoff with jitter and server cooldown guidance. | Launcher `6999ad85`; out-of-order, disposal, retained error/draft and recovery regressions. Pagination-aware successful polling allows at most 12 list requests/minute. |
| Profile persistence | Ordinary telemetry cloned, serialized and synchronously persisted the profile. Volatile observations now remain in memory; coarse changes still persist. Blocking commits own the transaction guard, so cancelling the waiter cannot split disk and memory state. | Launcher `6045bf91`; 60 observations make zero file changes; cancelled queued commit completes before the next writer. |
| Credential lifecycle | Removing a connection deleted its key before metadata save, and cleanup could strand concurrent removals or a transient vault failure. Removal first saves a durable cleanup record; serialized maintenance drains additions and retries once/minute. | Launcher `6045bf91`, `b884c871`; save failure, overlapping removal and injected vault failure tests. |
| Subscription recovery | A temporary credential/client construction error permanently ended existing workers. Demand survives transient errors with 1/2/4/.../60-second backoff; trust/access failures remain blocked. | Launcher `b29ec9d5`; retry policy tests. A real packaged vault outage still needs exercise. |
| Background wakeups | Status-only workers woke ten times/second; disabled sleep features still loaded policy four times/second. Status-only wakeups are now once/second; metrics-only four/second; console remains ten/second. Disabled sleep uses one-second observation; active sleep/offline status retains 250 ms cadence. Driver destruction unparks before joining. | Launcher `b29ec9d5`; server `6fc111d`; policy and nine sleep regressions. A newly enabled sleep feature can take up to one second to be observed. |
| Upload lifecycle | Up to about 1 GiB of selected files could stay staged until another picker action or process exit. Aggregate staging is now 64 MiB, including source growth. Known finish/abort releases file bytes while preserving small retry receipts; idle sweeps enforce the existing one-hour lifetime. | Launcher `db73fd41`; pressure, shared-owner and actual temp-file deletion tests; pinned transfer fixtures. Unknown finish outcomes retain recoverable state. |
| Download lifecycle | Downloads could occupy a bulk slot for a long time with no cancellation action. Activity now cancels owner-bound transfers; an atomic boundary decides cancellation versus final destination publication. Destroyed windows cancel pending work; hide-to-tray preserves it. | Launcher `0190ab8c`; cancellation preserves an existing destination, removes temporary bytes, releases admission and permits a following transfer. |
| Transfer IPC | A byte-only progress threshold flooded fast links and left slow links quiet. Progress now updates at most ten times/second per response, plus initial/final updates. | Launcher `83f03c9e`; deterministic fast/slow progress test and 23 HTTPS fixtures. The stalled-stream cancellation fixture now actually streams the download route. |

The new limits are intentionally fixed and conservative. Automatically increasing
queues or byte budgets merely because total host RAM is large would weaken the
low-end envelope and can compete with Minecraft under pressure.

## eMudhra concern

The incident involves compromised intermediate keys and cross-signs. The patched
`rustls-webpki` verifier applies distrust across certificate paths; removing one
root alone is insufficient. Resolved dependencies use `rustls-webpki` 0.103.16
and `webpki-roots` 1.0.10, with gates covering the server and both launcher
lockfiles. These are dependency and integration checks; no new malicious-chain
fixture was invented to substitute for upstream verifier tests.

Exact pinned Vesta leaf identity remains a separate trust mechanism. Initial
capture is unauthenticated discovery, not authorization to send credentials.
Legacy native-TLS clients and the WebView still depend on OS/vendor trust updates.
No OS trust store was changed. Release/update authenticity and timely distribution
of rebuilt binaries remain release gates; a patched source tree does not update
an already installed executable.

Primary references: [incident](https://bugzilla.mozilla.org/show_bug.cgi?id=2083449),
[RustSec advisory](https://rustsec.org/advisories/RUSTSEC-2026-0382),
[cross-sign verifier change](https://github.com/rustls/webpki/pull/546),
[root update](https://github.com/rustls/webpki-roots/releases/tag/v%2F1.0.10).

## Resource evidence and remaining performance work

Owned Windows release fixture: one CPU affinity, loopback TLS, no clients,
no runnable Java, two-second warmup. The benchmark creates its own temp directory
and only stops its own matching PID/executable/start time. Logs and state remain
available. Reproduce with `scripts/measure-idle-agent.ps1 -Seconds 20`.

| Sample | Wall / CPU seconds | One-core CPU | Working set / private MiB |
| --- | --- | --- | --- |
| Before, first | 20.011 / 0.65625 | 3.279% | 24.633 / 3.445 |
| Before, repeat | 15.002 / 0.484375 | 3.229% | 12.992 / 3.770 |
| After disabled-policy tuning | 20.011 / 0.50000 | 2.499% | 12.906 / 3.711 |
| After, repeat following completed checks | 20.016 / 0.46875 | 2.342% | 13.031 / 3.984 |

These observed reductions are about 24-29% against the first sample, not a guaranteed
improvement across machines. Cache state and other host activity were not tightly
controlled. CPU affinity is not a real low-end CPU or a 2 GB memory limit. No
Minecraft-under-load, Linux/ARM, suspend/resume or packaged acceptance result is
implied. Twelve threads were present; thread descriptions were unavailable.

The remaining idle CPU is worth reducing. Automation still opens a write
transaction each pulse even when no workflow can run; metrics and policy checks
also open protected database connections. Next measure those individual paths,
then remove needless transactions or use explicit invalidation before considering
connection pooling. Pooling must preserve protected-path validation, revocation,
SQLite ownership and cancellation behavior. Do not lower synchronous durability
or drop journals to improve a benchmark. See [SQLite WAL durability](https://www.sqlite.org/wal.html).

Suggested development gates, to validate rather than advertise: idle under 1% of
one target core with no clients, stable private/RSS envelopes, no sustained growth
after a transfer/cancel burst, and responsive lifecycle admission during bulk work.
The measured CPU target is not met yet. Test both a genuinely slow CPU and storage;
a fast desktop restricted to one core does not establish that envelope.

### Technical direction for pressure adaptation

Linux exposes CPU, memory and IO stall time through PSI, including threshold
notifications. This is more directly useful for identifying contention than CPU
percentage alone. A future controller can reduce optional sampling and defer
restartable index work when pressure persists, then recover with hysteresis;
thresholds need measured tuning and supported-kernel fallbacks. This is a proposal,
not implemented behavior. [Kernel PSI documentation](https://docs.kernel.org/accounting/psi.html).

Container limits need separate treatment. cgroup v2 resource limits are
hierarchical, so inspect effective ancestor constraints as well as process CPU
affinity, `cpu.max`, `memory.max` and current usage before making capacity
recommendations. Physical host totals alone are not the container budget.
Current `metrics/native.rs::host` reads `/proc/stat` and `/proc/meminfo`; those
are host observations, not permission to allocate up to that total. Parse bounded
files, handle unavailable controllers and namespaces, and test nested limits
before using them to tune concurrency. [Kernel cgroup v2 documentation](https://docs.kernel.org/admin-guide/cgroup-v2.html).

On Windows, QoS can affect core choice and power management. Evaluate it only
for clearly separated optional work after measuring latency; an agent-wide
setting could also slow control and recovery paths. This is an inference for
Vesta, not a verified optimization. Keep benchmarks' power/QoS conditions
recorded and do not change system settings or Java priority implicitly.
[Microsoft QoS documentation](https://learn.microsoft.com/en-us/windows/win32/procthread/quality-of-service).

Pressure adaptation must not silently cancel durable jobs or lower authorization
checks. Its first outputs should be fewer optional reads and bounded admission,
with visible delay reasons. Keep the existing control reserve, use monotonic
timing for cooldowns, and restore work gradually to avoid reconnect/IO bursts.

## Remaining findings and decisions

| Priority | Finding / concrete risk | Next step |
| --- | --- | --- |
| P1, owner policy decision | The native and client intent journals have a 4,096-entry lifetime cap. Heavy or long-lived profiles eventually refuse new mutations. Some single-use preparation receipts must prevent replay even when they look terminal; deleting old entries by age is unsafe. | Design bounded anti-replay tombstones or versioned intent epochs, including upgrade/restart semantics and explicit retry horizons. Review the contract with the owner before migration. Preserve uncertain entries. |
| P1, low-end capacity | Backup indexes are bounded by object count, not allocated bytes. `MAX_FILES=1,000,000` and `MAX_CHUNKS_LISTED=10,000,000` still permit large `Vec` / `HashSet<String>` allocations before backup work on a 2 GB host. | Add streaming/paged repository enumeration and an explicit metadata-memory budget. Measure before/after; refuse before allocation. Review repository compatibility and large-host behavior before changing the storage interface. |
| P1, release gate | Packaged Windows/Linux, ARM, native-vault outage, real suspend/resume and slow/dropping-network checks are incomplete. Independent Rust trust mitigation does not update OS/WebView trust or installed binaries. | Run the matrix below and establish signed build/update distribution. Do not call this release-ready based on unit tests. |
| P2, service availability | A sustained peer can refill handshake slots even with a five-second deadline. Bounded memory does not guarantee availability under attack. | Measure authenticated control latency under connection churn; evaluate per-peer/pre-auth fairness and deployment firewall limits without penalizing users behind one NAT. |
| P2, background cost | Empty automation/metrics/policy pulses still perform protected database work. | Attribute CPU/IO by subsystem and remove proven idle work. Preserve durable effect and authentication checks. |

No journal deletion, repository format rewrite, trust-store edit, data migration,
live restart or deployment was performed while the owner was away. The decisions
above are durable follow-up items, not requests that block the completed fixes.

## Experience roadmap

This sequence follows the lifecycle, status/MOTD, backups, connectivity and recovery
requirements in the [requirements](../SERVER_SOFTWARE_REQUIREMENTS.md),
[implementation plan](../SERVER_SOFTWARE_IMPLEMENTATION_PLAN.md) and
[roadmap](../SERVER_SOFTWARE_ROADMAP.html). Existing screens alone do not prove
those acceptance gates complete.

1. **Make connection state actionable.** Display management connectivity separately
   from Minecraft readiness, and the game join address separately from management
   port 8443. Show the age of the last observation. Keep an outdated snapshot
   visibly stale instead of presenting a fresh-looking success or generic busy
   error. Distinguish a local queue, remote 429, transport loss and denied access.
2. **Explain effective sleep state.** Keep saved desired policy separate from
   listener availability and the actual sleeping/starting/stopped state. Report
   port-bind failure and a useful recovery action. The current configured address
   alone is not evidence of a successfully bound offline responder. Show that
   server-list pings do not wake Minecraft and that a real join can require retry.
3. **Finish backup onboarding.** Offer the concrete host prerequisite/configuration
   path when backups are not configured, rather than a generic unsupported state.
   Show consistency, estimated space and actual job progress; make restoration's
   downtime and rollback boundary reviewable before admission.
4. **Keep intent and recovery visible.** Preserve drafts and meaningful errors
   through status updates. Show accepted/running/unknown outcomes separately.
   Retry an uncertain mutation only with the same recorded intent. Make transfer
   cancellation and delayed completion clear without interrupting navigation.
5. **Adapt optional observation.** Keep lifecycle control capacity reserved. Use
   streaming and visibility-aware subscriptions; reduce optional metrics/history
   work when hidden or backing off. Honor remote cooldowns and jitter reconnects.
   Do not silently stop security checks, durable jobs or necessary recovery work.
6. **Guide resource allocation.** Recommend Java heap from available memory with
   explicit OS, agent and native-memory headroom. Explain when a chosen modpack
   cannot fit a 2 GB host. Keep small-host defaults conservative and permit
   measured larger-host configurations without unbounded queues.

## Acceptance matrix for the next owner session

- 1 core / 2 GB and 2 cores / 4 GB, real low-end Windows and Linux storage; agent
  idle, one connected launcher, console activity, file transfer and backup index.
  Record CPU, private/RSS, disk writes, wakeups, request latency and queue depth.
- Latency/loss, brief disconnect, server restart, expired authentication and vault
  outage; recover existing subscriptions and preserve drafts without duplicate
  mutations, retry storms or permanently stale errors.
- Start/stop and console navigation during saturated uploads/downloads; cancel a
  stalled download before commit and race cancellation against publication.
- Sleep enable/disable, port conflict, empty-player evidence, Minecraft join
  wake, repeated pings, crash-loop refusal and a slow game startup. Check both
  configured and effective state, with an actual Java client.
- Disk full during metadata save, backup promotion, cleanup and final download
  publication; preserve existing data, show the true outcome and recover safely.
- Restart with active/uncertain operations, simultaneous windows, closing versus
  hide-to-tray, and suspend/resume. Then build/package and verify update trust on
  supported platforms.

## Validation record

The final launcher pass ran 107 focused frontend tests successfully, with TypeScript
passing. The older reactivity harness still prints its multiple-Solid warning;
new guarded-read diagnostics are quiet. Final client checks passed ten unit,
11 management, 12 policy, five scheduler, eight native storage/credential and 23
pinned-HTTPS tests; one explicit live-profile diagnostic was intentionally ignored.
Strict client all-target Clippy and both lockfile trust gates pass. The native
rewrite check passed earlier with existing unused/dead-code warnings; there is
no new packaged or visual acceptance claim.

The server's final top-level Node checks pass 166/166; requirement-ledger and
roadmap freshness checks pass. Nine requirement entries had claimed tests from
the discarded launcher prototype. Those references are now historical evidence;
entries with no remaining current test source are marked not verified. The
validator also now accepts actual setup/probe evidence rather than requiring
every partial result to reference the oldest archive. Historical files were
preserved; no other repository was changed for this cleanup.

The supervisor-crash integration test exposed an overly narrow error assertion:
shutdown can observe the dead channel during game observation or offline-listener
release. Both errors predate this review. The test accepts those exact outcomes,
explicitly checks that shutdown leaves the orphan alive, and continues checking
duplicate-launch refusal and eventual deliberate game exit. All three supervisor
end-to-end tests pass after the correction; no production recovery rule changed.

Fresh non-agent/non-transport workspace tests pass, including all 924 control
unit tests, backup/recovery integrations, 168 installer tests, 16 setup tests,
state migrations, process ownership, supervisor suites and doctests. Agent unit/
CLI checks passed in the earlier broad run; its corrected three-test supervisor
suite passes separately. All 42 transport unit and 111 HTTPS tests pass. Final
strict workspace Clippy, formatting and `git diff --check` pass. See the ledger
for the exact split commands, avoiding an unnecessary second full TLS run.

Earlier failed runs are retained in the ledger rather than rewritten as successes.
In particular,
a stale debug schema artifact caused invitation failures; rebuilding workspace
sources resolved them and a fresh full transport suite passed all 111 tests.
The final progress change also exposed a misplaced slow-stream fixture; moving
it to the download route restored meaningful cancellation coverage.
