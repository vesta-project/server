# Performance capture and operational alerts

## Implemented retained-telemetry export

Authenticated `GET /api/v3/manage/metrics/capture` accepts the same closed range
query as `/metrics`, for example:

```text
/api/v3/manage/metrics/capture?series=game_cpu_percent,game_memory_bytes,tps,mspt&from=<milliseconds>&to=<milliseconds>&resolution=auto&max_points=2000
```

It returns the ordinary JSON envelope as an attachment named
`vesta-performance-capture.json`, with `Cache-Control: no-store`. `metrics.view`
is checked again in the same database snapshot as current and historical data.
The report includes schema, server ID, capture time, exact request, current
sample/status, history, effective resolution, retention gaps and tick provenance.
The current sample may be outside the requested history window; its own timestamp
is retained. Empty or unknown measurements remain empty/null, never invented zero.
This is an export of already retained telemetry, not a new timed recording.
`history_point_limit_reached` conservatively marks a projection that reaches the
requested limit: it is not proof that every sample in the window was exported.
Select automatic/coarser resolution or a smaller explicit window to inspect it.

Existing bounds apply: at most eight distinct known series, at most 2,000 points,
finite historical windows and the existing retention policy. A standalone capture
refuses incremental `after` and conditional `If-None-Match`; it cannot silently
export only changes or a 304. Other range/body/conditional requests refuse under
the existing metrics contract. No files, jobs, game processes, console probes or
profilers are created. Sampling policy is unchanged. The export includes no player
identities, raw console, world seed, JVM arguments, filesystem paths or secrets.
The response still reveals server identity and operational telemetry; sharing it
is a separate user action. No third-party upload is performed.

This backend endpoint has local TLS fixture evidence. Launcher onboarding,
download controls and UI are deferred. Existing console-polling consent still
governs the independent sampler; exporting does not enable or disable it.

## Operational alerts through automations

Use the existing persisted automation engine and inbox rather than another alert
service. Current host composition publishes startup, game start/stop/crash and
backup completion/failure events. Crash-loop suspension and queued whitelist
failures already produce notifications. Workflow `Notify` actions retain current
principal/device authorization, bounded output and existing inbox retention.
No new default schedules or outbound notification channel is enabled here.

Next implementation should add explicit metric-condition triggers with dwell,
hysteresis, cooldown and generation-aware deduplication. Treat missing samples,
clock gaps and permission loss as unknown, not healthy or threshold crossings.
Recommended initial conditions are sustained low TPS/high MSPT, low disk headroom
and repeated backup failure; do not silently turn these into restarts, captures or
updates. Thresholds and actions must be saved in each server's workflow policy.

Startup update discovery currently belongs to an installation, whereas workflow
authority belongs to one server. An `UpdateAvailable` event type already exists,
but no installation-to-recorded-server bridge or updater action is wired. A future
bridge must bind an exact installation consumer registry and candidate identity,
emit once per candidate/server, and never grant machine update rights to a remote
per-server Owner. Repeated checks must not create duplicate alerts/rollouts.

## Spark integration design; execution not implemented

Use two clear capture kinds: the implemented local retained-telemetry report, and
an optional Spark profile. Paper documents bundled Spark starting with 1.21;
actual availability still needs runtime/version/capability verification. Other
software needs an explicitly installed compatible plugin/mod; never infer this
from an untrusted status brand or download a profiler automatically. Folia's
region clocks must remain distinct from a fabricated global tick figure.
[Paper profiling](https://docs.papermc.io/paper/profiling/).

A future profile job should bind server, actor, exact running generation, Spark
version/capabilities and a finite caller-selected duration. Admit through normal
durable job/console authority; recheck before commands and artifact retrieval.
Refuse an already-running profiler rather than stopping another user's capture.
Do not expose arbitrary profiler flags/shell commands. Keep read-only metrics
permission separate from command/profiling authority and install consent.

Spark documents timeout/status/stop/cancel controls and local file saving. The
local-only path must be proven for the installed version before automatic use:
do not assume a timeout combined with a later stop prevents uploading, or that
current docs certify historical Spark. A safe adapter needs a supported local-save
start/stop lifecycle, bounded owned output, permission/generation checks and
reopen/reconciliation tests. Unsupported versions should offer manual guidance,
not guessed commands. Vesta must never fetch a console-supplied arbitrary URL.
[Spark command reference](https://spark.lucko.me/docs/Command-Usage).

Report upload requires separate explicit consent and a verified destination.
Spark data may include JVM arguments, configuration, plugin/mod details and world
statistics; its known-setting filtering is not a guarantee that custom settings
contain no secrets. No heap dump, allocation capture or automatic report upload
is enabled. Native profiler engine/OS/CPU availability and Java fallback require
real runtime evidence, not a catalog match.
[Spark privacy policy](https://spark.lucko.me/docs/legal/Privacy).

Sources checked 2026-11-09 UTC. No Spark executable was installed or launched,
and no upload or real-game profiling is claimed by this batch.
