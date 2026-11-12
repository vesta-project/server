# Agent catalogue cache implementation checkpoint

Implements the first metadata foundation from
`docs/architecture/METADATA_AND_SOFTWARE_PLAN.md`. No UI, live agent, Minecraft
world, OS networking or user credentials were changed.

## Delivered

- Agent-owned browsing cache, default memory-only for standalone installer use.
  The real host injects optional persistence through a small `CatalogueStore`
  interface. Persistence uses a service-owned table in protected state.
- Raw source documents and normalized version/release lists are retained only
  after their complete query validates. Failed/cancelled queries do not commit
  candidate data. Raw documents can serve related queries without another fetch.
- Cache keys partition source/selection, parser/policy namespace, proxy settings
  and query limits. Proxy credentials appear only inside the digest input, never
  in stored keys. The HTTP approval policy ID remains separate and random.
- Eight MiB retained memory, 64 entries, up to 64 MiB encoded persistence with
  accounting overhead. SQLite file/WAL allocation is additional; this is not a
  total process memory/disk claim. Active response and parse buffers remain
  independently bounded by existing installer limits.
- Same-key query sharing with at most 16 admitted readers and eight active keys.
  The management service keeps its single provider/plan permit, admits at most
  eight queued catalogue callers, and bounds queueing plus work to 12 seconds.
- Digest/expiry validation when loading disk records, no writes on warm cache
  hits, byte/count eviction, cancellation cleanup and bounded optional blocking
  persistence. Cache write failure does not change installation state.
- Fresh planning still reads independent provider evidence and verifies exact
  artifact pins. Neither normalized results nor cached mirror URLs authorize a
  JAR download.

Origin no-store/no-cache directives disable retention. Explicit max-age is
reduced by origin Age before applying the local five-minute cap. Without an
origin lifetime, data is a five-minute application snapshot from local
validation; this is not a claim about the HTTP representation's age. Explicit
Expires without supported lifetime parsing is conservatively not cached.
Conditional ETag revalidation is still a later optimization.

Forge's Maven catalogue explicitly sends `no-cache, no-store, max-age=0`, with no
validator observed. Instead of ignoring that policy, browsing uses the existing
launcher Modrinth Forge manifest source and caches validated loader IDs. Its URL
fields are ignored. The new source family accepts only the fixed manifest path
and metadata role. Fresh Forge plans continue to use official Maven membership,
metadata and artifact digest checks.

## Evidence

Fixtures cover request sharing/overflow, no-store/no-cache, malformed provider
responses, cache recreation/corruption, cancellation, byte eviction, clock
rollback, CDN Age/freshness, Forge source reuse, independent fresh planning,
service authority before/after queries and bounded protected persistence.

The complete installer suite passed 187 tests, with one existing opt-in local
volume test ignored. After the Forge browsing-source and freshness refinements,
17 targeted catalogue tests passed. The software-service suite passed 14 tests.
Strict installer/control all-target Clippy passed before the final equivalent SQL
read cleanup. Additional final command logs are kept under `V:/tmp/metadata-cache-*`.
No real BuildTools/Forge installer or Minecraft process was executed.

Follow-up regression coverage confirms that metadata fetched later in a query
cannot extend an earlier document's freshness lifetime. A transient provider
failure is shared for 250 ms with the current waiter wave; twenty callers cause
three total bounded upstream attempts rather than three attempts per caller.
Cancellation/invalid metadata are not stored by this short failure mechanism.
All nine catalogue-cache integration cases passed after this refinement.

One live direct-connection diagnostic run observed:

| Software | First version query | Immediate repeated query |
| --- | ---: | ---: |
| Vanilla | 712 ms | 1.21 ms |
| Paper | 37 ms | 0.17 ms |
| Folia | 40 ms | 0.17 ms |
| Purpur | 47 ms | 0.12 ms |
| Fabric | 687 ms | 0.68 ms |
| Quilt | 666 ms | 0.61 ms |
| Forge | 345 ms | 0.15 ms |
| NeoForge | 281 ms | 0.14 ms |

Forge release choices after the version query took 24 ms in this run. Results are
single observations, not p95 or 3G/low-memory measurements. The live log is
`V:/tmp/metadata-cache-live-checkpoint.log`.

## BuildTools continuation

Research alone could not prove source input binding through documented `--rev`.
Parent source inspection then retrieved the official Git repository without
executing it, at `5fc101051bb4bc32a40c8fb7d585db5ed5e97884`.
`Builder.java` and `BuildInfo.java` establish a candidate route: prepare all four
repositories with local `master` refs at the reviewed commits, use `--dont-update`
without `--rev`, and bind `bt.name`. This must be proven with isolated execution
tests and a pinned BuildTools binary. It is not yet automatic installation
support, a sandbox or proof of reproducible Maven dependencies.

The next shared adapter should support both CraftBukkit and Spigot, with common
catalogue parsing, input provenance, process ownership, publication and rollback.
Do not expose either as installable before the execution/output/host matrix is
verified. Claude's UI work remains untouched.
