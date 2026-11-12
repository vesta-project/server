# Hosted resource-pack backend implementation

The resource-pack builder now has a durable hosting service and real HTTPS API.
The launcher has not been changed. The [API contract](../operations/resource-pack-hosting-api.md)
lists exact requests, policy, budgets, deployment requirements and remaining work.

## Implemented behavior

- A separate revisioned draft preserves ordered resource-pack inputs and hosting
  settings. It does not copy client-modpack resources implicitly.
- Protected chunk uploads pin exact SHA-256 bytes. Provider identifiers reuse the
  canonical metadata/CDN/download/hash adapters, with authoritative resource-pack
  kind and Minecraft-version checks. Previously resolved version pins reuse their
  protected bytes; no background metadata fetching or rebuild polling occurs.
- An owned, demand-driven worker builds one admitted job at a time. It persists
  progress and fixed error codes; startup marks interrupted work instead of
  automatically replaying downloads or configuration changes.
- A completed candidate remains private. An owner-reviewed, expiring, device-bound
  apply writes the URL/SHA-1 and supported pack settings through the existing
  recoverable properties writer. It never starts or restarts Minecraft.
- The stopped/supervisor hold and shared software-maintenance exclusion remain
  owned through durable publication. An unresolved properties/publication journal
  blocks new starts while leaving management available for inspection/recovery.
- Before/after hashes reconcile interruption. Active release, public registration
  and durable replay receipt commit together. Existing applied URLs remain usable.
  An explicit owner abandonment, bound to the inspected current properties hash,
  can retire an uncertain activation without rewriting the file or deleting an
  earlier published URL. Replaying an old abandonment cannot clear a new block.
- Public GET/HEAD streams only explicitly applied hash-addressed ZIPs through the
  existing TLS transport, shared two-download capacity, fixed chunks, throughput
  deadlines and SHA-256 checking. Multiple users behind a NAT/proxy can use both
  slots. Indexed lookup avoids scanning release history on every download.
- Status reads select eight recent jobs directly in SQL. Complete path-resolution
  provenance remains in the ZIP, avoiding repeated large historical status payloads.

## Practical verification

[Terra's independent HTTPS exercise](2026-10-27-hosted-resource-pack-practical-terra.md)
used a fresh disposable localhost host and a separate CA-validating HTTPS client.
It covered upload, ordering/reordering, asynchronous build, plan/apply, replay,
properties preservation, ZIP contents, both digests, GET/HEAD, old URLs, malformed
archives, stale revisions and unauthenticated management denial.

Terra found that the first implementation exposed a completed build before apply.
That finding was fixed by moving public registration into the completed apply
transaction. A fresh practical recheck verified 404 before apply, 200 afterward
and continued availability of the previous applied URL. Both laboratory fixtures
exited cleanly. No real server, EULA, Minecraft process or public DNS/certificate
was involved.

## Automated verification

Luna ran the checks and returned concise results; no source edits were delegated
to the test runner.

- Windows: 29 resource-pack tests, 27 host tests and 15 software tests passed.
- The full HTTPS target passed 122 tests, with its one interactive laboratory
  ignored in the ordinary suite. Terra ran that laboratory explicitly, twice.
- The targeted end-to-end HTTPS test passed again on the final recovery/receipt
  changes, including the new recovery route and private-before-apply assertion.
- Strict Windows Clippy passed for both libraries and tests with `-D warnings`.
- Ubuntu 24.04: 29 resource-pack tests and strict library Clippy passed for
  `vesta-control` and `vesta-transport`.

Final recovery logs use `V:/tmp/resource-pack-hosting-recovery-final2-*`. Earlier
full HTTPS, host and software logs use `V:/tmp/resource-pack-hosting-*`. No real
Minecraft integration was run. These are focused regressions plus the full HTTPS
target, not a claim that the entire workspace/test matrix was rerun.

The native recovery test also caught a nested database call: runtime hooks perform
their own canonical reads and cannot run inside a service transaction. Hook
observations were moved outside transactions, and durable replay reads were
factored into the shared receipt helper. Test hooks now perform real database
reads so both software-target build admission and recovery preview exercise this
constraint instead of passing through a simpler in-memory substitute.

## Deliberate limits

This is backend/API work, not the finished launcher feature. Automatic target pack
format detection, native launcher IPC, workspace disable/retirement controls and
Opus's UI remain ahead. The configured public HTTPS origin needs actual operator
DNS/proxy/certificate setup and validation. Active pack filters/overlays still
refuse through the builder. Provider files retain the shared downloader's 16 MiB
limit, while direct uploads support 64 MiB.

Real Minecraft acceptance/decline behavior and public deployment were not tested.
Neither the localhost run nor the existing builder fixtures certify every client
version, multi-agent host resource scheduling or a physical low-memory machine.
