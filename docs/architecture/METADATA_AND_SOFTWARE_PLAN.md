# Metadata ownership, performance, CraftBukkit and Spigot support

Status: implementation plan, not implemented features. Date: 2026-10-26.
Baselines: agent `fed0674`, launcher `f4af2e3f`.

Implementation checkpoints:

- `d191aaf`: bounded agent browsing cache and protected persistence; fresh install
  evidence remains separate. See `docs/reviews/2026-10-26-agent-catalogue-cache.md`.
- Shared CraftBukkit/Spigot source installation now extends the discovery
  checkpoint. See `docs/operations/buildtools.md` for requirements, input binding,
  limits and failure behavior. Real-build validation is recorded separately.

## Goals and boundaries

Support comfortable management on a 1-2 core, 2-4 GiB host, leaving resources for
Minecraft. Improve catalogue loading and failure recovery without redesigning the
launcher. Keep client modpacks independent of actual server resources. Leave
Claude's unfinished UI/worktree untouched.

Each launcher owns its local metadata. Each agent owns the metadata it uses to
resolve and verify server software and content. A launcher may request an exact
selection; it cannot supply the agent's download authority. Sharing parsers and
cache primitives must not share trust, credentials or writable cache directories.

## Existing behaviour to preserve

| Area | Current implementation | Consequence |
| --- | --- | --- |
| Managed software | `SoftwarePlanRequest` contains software/version/build/loader IDs; installer resolves source URLs and published digests | No caller-selected managed JAR URL |
| Mods/plugins | `UreqProviderClient` resolves provider/project/version IDs through fixed endpoints | The agent checks provider identity and hashes itself |
| Installation evidence | Plans bind metadata revisions and artifact pins; receipts retain observed hashes; managed launch verifies artifacts | Preserve reviewed selection and verification on reuse |
| Launcher metadata | Piston `ManifestCache` has disk entries and ETag freshness; server catalogue resources share five-minute freshness and concurrent loads | Extend existing stores instead of adding another UI cache |
| Agent catalogues | No shared persistent catalogue cache; a single software query permit bounds metadata/planning | Repeated callers can repeat upstream work or receive busy |
| Catalogue timeout | Version/release jobs now cap at 12 seconds; client HTTP deadline is 15 seconds | Preserve this bound, including refresh and retries |
| Control priority | Both sides reserve urgent control capacity; bulk downloads have separate bounds | Background metadata must not occupy these lanes |

Relevant sources: `crates/vesta-server-install/src/{lib,model,transfer}.rs`,
`crates/vesta-control/src/software/mod.rs`, `crates/vesta-control/src/content/`,
launcher `crates/piston-lib/src/game/{metadata,manifest_cache}/`, and
`apps/desktop-solid/src/features/servers/{server-resources,resources}.ts`.

A matching hash proves correspondence to the selected metadata, not that a JAR
is harmless. A locally calculated SHA-256 is useful for retained-file integrity,
but does not authenticate an unknown JAR's publisher. Keep legacy checksum
acknowledgements and the explicit custom-JAR path distinct from managed software.

## 1. Separate browsing data from installation evidence

Use three kinds of record with separate lifetimes:

1. **Catalogue snapshot:** available versions, builds, loader choices, stability
   and Java requirements. Can be cached for navigation and display.
2. **Resolved artifact record:** exact provider identity, game/software/build,
   source family, approved URL, published digest algorithm/value and size when
   available; retain source-document digest, retrieval time and parser/policy
   version. Construct only from the agent's validated provider response.
3. **Installation receipt:** actual installed files, observed hashes, selected
   launch and runtime, plus evidence linking them to the approved plan. Retain
   with the installation until explicit removal, not cache eviction.

The client sends selection identifiers. The agent independently resolves them,
checks compatibility, makes the reviewed plan and verifies streamed bytes before
activation. Never accept a client-supplied URL or expected digest as provider
authority. Existing-file precondition hashes remain valid compare-and-swap input,
not evidence that the incoming artifact is trusted.

Fresh installation planning must revalidate the exact selected source record.
Conditional revalidation with an upstream validator is valid when correctly
implemented; it need not download an unchanged whole catalogue. A cached display
snapshot alone cannot approve installation. Preserve existing plan expiry,
actor/revision binding and idempotency semantics.

Cache entries are untrusted input on read. Reapply length, schema, identity and
source-policy checks. Detect truncated/corrupt records and discard only that entry.
Policy/parser changes invalidate incompatible records. An ETag is an HTTP
validator, not an artifact checksum or security signature.

## 2. Add an agent-owned metadata cache

Put a small store interface below provider catalogue resolution. Integrate it
with `ServerInstaller` and the software service before extending content metadata.
Default installer construction can remain memory-only; the owning agent injects
a protected persistent store. Writes may affect that cache only, never game files.
This preserves metadata-only catalogue semantics and existing isolated fixtures.

Initial proposed budgets, to validate before increasing them:

| Budget | Agent and launcher native metadata stores |
| --- | --- |
| In-memory hot data | 8 MiB per process, byte-accounted rather than entry count alone |
| Persistent cache | 64 MiB, at most 64 entries; no unbounded version history |
| Individual provider documents | Existing provider byte/collection limits still apply; an entry too large for RAM stays disk-backed or is not retained |
| Same-key fetch | One owned in-flight task per key; at most 16 waiting callers in total and fixed deadlines |
| Concurrent upstream metadata | One active request on agent initially; at most 8 queued distinct keys, still subject to the total waiter cap |
| Provider freshness | Five-minute starting maximum for mutable catalogues, shortened or disabled by origin directives |
| Negative results | At most 30 seconds for a validated missing selection; never cache auth/trust failures as successful data |

The cache budgets count encoded or retained bytes plus measured overhead. Active
download/parse buffers are separately bounded and measured; an 8 MiB cache cap
is not a claim that the whole process or an active 16 MiB response fits in 8 MiB.
Installed artifact receipts/pins are separate durable state and are never evicted
to make room for browsing data. Old, active and in-flight entries cannot be
silently replaced while a reader still depends on them.

Store keys include provider/source family, canonical endpoint/selection, API and
parser/policy version, and relevant transport or credential partition. No raw
credentials in keys, logs or disk records. Private provider responses must not
cross credential partitions. Server-specific client views also bind to the
connection's approved identity/trust generation, not just a display name.

Cache valid response bytes and normalized results together where useful. Derive
Forge Minecraft versions and loader releases from the same validated catalogue,
instead of downloading and parsing the same large document twice. Reuse shared
helpers for parsing, policy and byte accounting; do not make the server depend on
the whole launcher engine.

Use `If-None-Match`/ETag and Last-Modified only when supported. Accept 304 only
when the matching stored body exists and is still valid for this key/policy.
Respect no-store, no-cache and must-revalidate. Keep application display snapshots
explicitly separate from claims of a fresh HTTP response. A source that forbids
stale reuse must not be bypassed by a generic stale-while-revalidate policy.

On restart, read validated local snapshots on demand. No provider-wide boot scan,
periodic catalogue polling, or re-hashing every cached document on every hit.
Use atomic protected writes, last-known-good replacement, byte-limited LRU
eviction and no per-hit database/filesystem writes. Cache persistence failure
must not invalidate an otherwise verified provider response; retain memory data
within budget and continue without silently weakening verification.

## 3. Make speculative work cheaper than user actions

- A visible version/build request takes precedence over hover or boot warmup.
  Same-key work is shared, not duplicated. Awaiting callers are independently
  authorized before receiving a result, including cache hits.
- Different-key speculation must not occupy a slot needed for an active plan.
  Cancel or defer work that is still queued when selection changes. If an
  already-active read is safely cancelled, retain ownership until it actually
  stops. Do not create a second lane that silently doubles upstream concurrency.
- Warm only the selected software and likely Minecraft version. Do not fetch
  all eight providers on launch or every tile crossed by the pointer.
- A briefly cancelled subscriber must not leave an immortal fetch or immediately
  restart it through another component. At zero subscribers, release queued
  work; active work has a fixed deadline and bounded completion/cache ownership.
- Return known current installation data immediately. Load selectable choices
  lazily and preserve the current selection while they refresh.
- Handle a version with no supported stable builds as an unavailable choice,
  not a generic metadata error or endless spinner. Do not silently select Alpha
  builds, a different loader, or a different Minecraft version.
- Carry precise fixed error reasons and Retry-After through native/UI boundaries.
  Provider throttling should pause that provider, not unrelated providers or Stop.

These changes should fit the existing Software page. Record backend freshness and
source availability in the contract without adding technical warnings to every row.

## 4. Reduce connection, parsing and disk overhead carefully

Priority candidates, requiring before/after measurements:

| Candidate | Expected benefit | Constraint |
| --- | --- | --- |
| Reuse one validated provider document for versions and releases | Fewer downloads and repeated XML/JSON parsing, especially Forge | Never confuse providers, policy versions or credentials |
| Keep native metadata stores alive across related calls | Reuse RAM results instead of repeatedly constructing cache owners | Existing Piston disk/ETag behaviour must remain intact |
| Job/policy-scoped HTTP connection reuse | Less DNS/TLS setup for sequential provider requests | Current agent DNS pinning, proxy handling and redirect validation must not be weakened |
| Parse once per document digest/parser version | Less CPU on low-end hosts | Keep bounds before allocating/normalizing; work off async control threads |
| Conditional requests and compact client projections | Less 3G traffic and smaller IPC payloads | Never send upstream secrets, raw artifact authority or whole manifests unnecessarily |
| Retain verified artifacts by hash when already available | Avoid re-downloads and duplicate library storage | Revalidate bytes/paths and approved pins before activation; no writable links between installs |
| Latest-only telemetry and demand-based subscriptions | Less idle CPU, traffic and queue growth | Preserve player transitions, console cursor recovery and control responsiveness |

Existing reserved control lanes, bounded bodies/downloads and skipped missed ticks
already provide useful protection. Do not add worker threads, shorten polling
intervals, or tune JVM heap flags just to improve a synthetic benchmark. Report
full-agent overhead separately from Minecraft CPU/RAM.

## 5. CraftBukkit and Spigot support

Confirmed scope: both **CraftBukkit** and **Spigot** are initial targets, not a
CraftBukkit implementation with Spigot deferred. Display CraftBukkit for the
`bukkit` identifier and Spigot for `spigot`. Both use the existing Bukkit plugin
family rules, which already recognize those identifiers alongside
Paper/Purpur/Folia. Do not introduce a separate client modloader for either.

The official BuildTools route compiles CraftBukkit/Spigot. For current CraftBukkit
targets it supports `--compile craftbukkit`, and `--rev` selects the Minecraft
version. It requires Java and Git; documented Windows instructions use Git Bash.
This is a build recipe, not another URL-based downloadable server descriptor.
Do not automatically use an unofficial prebuilt-JAR mirror.

Implementation outline:

1. Add an explicit provider/recipe type for locally built software, alongside
   existing direct JAR, loader bundle and processor recipes. Use one BuildTools
   adapter with typed CraftBukkit/Spigot targets. Only target selection, output
   identity and the necessary invocation flags differ; do not duplicate pipelines.
2. Resolve official version/build metadata and a specific BuildTools revision.
   Record its source, digest, source revisions and selected toolchain. Never
   leave `latest` or `lastSuccessfulBuild` unresolved in a reviewed plan.
3. Preflight Java/JDK, Git and shell requirements plus writable staging, disk
   headroom and estimated build resources. Use a dedicated staging workspace and
   job-local Maven/Git configuration; never modify global Git settings, the
   operator's Maven cache, or the game tree to make a build succeed.
4. Reuse owned process-tree execution, cancellation, bounded logs and durable
   jobs from existing installer foundations. Pass validated fixed arguments;
   do not concatenate user input into shell commands. A necessary shell adapter
   must be independently tested for Windows spaces/metacharacters and Linux.
5. Bound one build at a time, elapsed duration and workspace growth. Do not run
   compilation automatically as background preload or compete with active
   Minecraft on a small host. Show that it is a build during the existing review.
6. Verify the expected output identity/launch structure; hash and retain the
   complete local output plus provenance. Local output hashes establish later
   integrity, not equivalence to an independently signed upstream server JAR.
   Do not claim BuildTools output is reproducible without demonstrating it.
7. Activate via the existing stopped hold/revision/launch replacement path.
   Preserve world, plugins, configuration, prior installation and rollback rules.
8. Test real official builds on Ubuntu and Windows before advertising automatic
   support. Start with an explicitly documented, measured version range.

BuildTools and its dependency processes execute trusted build logic; the existing
process runner is not a sandbox. Define source/toolchain provenance and subprocess
network behaviour before marking this installer supported. If the supported
BuildTools interfaces cannot pin required inputs, record that limitation and
keep managed automation blocked pending a concrete policy, rather than inventing
a claim of fully pinned builds. Low-end users can continue using the explicit
custom-JAR import route for builds produced on another trusted machine.

Do not add either target to the offered software list until its recipe, failure
recovery and supported host prerequisites work. The initial implementation is
backend-only; later UI integration remains a separate task for Claude.

### Reuse boundaries

Keep the shared adapter beside the Forge family, not inside it. BuildTools builds
from source; Forge processor profiles are a different recipe. Reuse their common
foundations without pretending their trust/evidence formats are interchangeable.

| Responsibility | Existing foundation to reuse |
| --- | --- |
| Metadata/download bounds and source validation | Installer `transfer::Transport`, `Job`, source-family policy and the new metadata store |
| Java selection and identity | `runtime::RuntimeBinding` and existing Java preflight |
| Owned subprocess tree, cancellation and bounded output | `execution::{Invocation, Budget, Pool}` and game-runtime process-tree ownership |
| Isolated build paths and output inventory | `execution_workspace` protected workspace primitives |
| Disk admission and publication | Installer coordinator/reservations and existing verified publication/receipt helpers |
| Stopped activation, revision checks and rollback | Software service worker and `RuntimeHooks`/`StoppedGuard` |
| Plugin placement and compatibility family | Existing content model rules for `bukkit` and `spigot` |
| Logging/progress/history | Existing durable jobs, progress reporter and audit projection |

Suggested adapter split: `providers/buildtools/{mod,catalogue,recipe,install}.rs`.
Use one typed target description with target ID, compile mode, expected outputs
and launch rules. Keep target differences declarative where possible. Introduce
new common helpers only when at least two existing consumers need them; do not
create another generic installer framework or copy Forge's entire install module.
Preserve old plan/receipt schema domains and fail closed on unsupported new ones.

One test matrix must run the same successful/failing build scenarios for both
targets: metadata identity, pinned tool inputs, missing Java/Git, output mismatch,
cancelled process trees, disk failure, activation and rollback. Add target-specific
cases only for real differences. Both targets must remain unavailable in automatic
selection until their own supported version/host matrix passes.

## Delivery order and acceptance checks

1. **Trust/cache contract:** define normalized records, key partitioning, byte
   accounting, origin freshness and receipt separation. Add tests for caller URL
   injection, poisoned disk records, credentials/identity changes, mismatched
   304, changed hashes and source-policy revisions.
2. **Agent metadata store:** same-key shared fetch, bounded persistence and
   source-document reuse. Test 20 same-key readers cause one upstream request,
   cancellation/failure frees ownership, restarts use valid disk state, and
   corruption/disk-full do not overwrite the last good entry. Excess waiters
   settle with a typed retryable result, never an unbounded queue.
3. **Client native cache and scheduling:** extend existing ManifestCache and
   server-resource owners, then active-over-speculative scheduling. Test rapid
   provider/version changes, warmup versus mounted views, trust replacement,
   offline browsing and Retry-After. Keep UI design work separate.
4. **CraftBukkit and Spigot recipes:** one shared BuildTools implementation with
   both typed targets, preflight, durable build and activation. Test missing
   prerequisites, supported historical and
   current versions, cancellation, child cleanup, bad outputs and interrupted
   publication. Test actual official builds separately from inert fixtures.
5. **Measure end to end:** one core/2 GiB and two cores/4 GiB VMs, direct and
   3G-like links, jitter/outages and slow storage. Capture cold/warm/restarted
   catalogue latency, bytes, CPU time, peak memory, disk growth and Stop latency.

Initial measurable targets:

- Warm native cache reads perform zero upstream requests or disk writes.
- A cache hit returns within 100 ms on the target VM, measured rather than assumed.
- Twenty same-key callers produce one provider fetch and bounded waiter memory;
  callers over the admission cap receive a typed retryable result, not another
  concurrent fetch or an unbounded waiter allocation.
- Starting a hover warmup never delays an already-queued explicit plan.
- No background catalogue requests while idle with no relevant subscribers.
- Cache memory/disk stay below the proposed byte caps under churn.
- In the existing simulated poor link (1,200 ms added delay, 8 KiB/s shared
  downstream), Stop completes before large read responses and remains under
  the existing four-second regression budget. Establish separate p95 results
  with a real agent; do not present fixture timings as full-system certification.
- Metadata errors/cancellation settle within the 12-second lookup budget;
  malformed provider data never turns into a trusted artifact record.

## Second-pass review

Checked against low-end hosts, failure cases and existing contracts:

- A persistent catalogue is a performance layer, not a new source of download
  authority. Cache hits cannot bypass authorization, source policies or approvals.
- Display freshness, provider HTTP validation and installed-file integrity are
  separate; no stale-cache install or ETag-as-checksum shortcut is planned.
- Proposed budgets include both bytes and concurrency. Fewer requests matter
  more than increasing parallelism, and persistent caches must not grow forever.
- Avoid a background refresh holding the only query slot while the user acts.
  Bound both admitted waiters and the underlying task after cancellation.
- Catalogue storage is protected and separate from mutable game/SFTP paths.
  Eviction cannot remove retained rollback evidence or installed artifacts.
- BuildTools compilation is materially heavier than downloading Paper. No
  promise is made that building it fits a 2 GiB machine; measure first and retain
  trusted off-host build/import as a practical path.
- No changes to live agents, network configuration, world files, unrelated
  repositories or Claude's UI are required to implement the first two phases.

## Primary references

- [SpigotMC BuildTools](https://www.spigotmc.org/wiki/buildtools/): build process,
  prerequisites, CraftBukkit selection and version flags. Its own notice warns
  that portions of the wiki may lag current versions; verify against the pinned
  tool before implementing an adapter.
- [Paper downloads service](https://docs.papermc.io/misc/downloads-service/):
  official metadata API and client-identification requirements.
- [HTTP caching, RFC 9111](https://www.rfc-editor.org/rfc/rfc9111.html): validators,
  freshness and restrictions on stale responses.
- Existing measurements: `docs/reviews/2026-10-26-version-metadata.md` and launcher
  `docs/reviews/2026-10-26-low-end-network.md`.
