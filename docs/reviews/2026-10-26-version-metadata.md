# Server version metadata validation

Date: 2026-10-26. Baseline agent commit `99d1373`, launcher `f4af2e3f`.

The preceding slow-network batch did not test upstream server software catalogues.
This batch covers provider parsing, transfer failures and limits, launcher cache
reuse, and read-only live version/build queries for all eight supported providers.
It does not establish full product coverage or install any real server software.

## Reproduced and fixed

`ServerInstaller::versions` and `releases` inherited the installation job's
600-second total deadline and 30-second idle budget. The launcher has a 15-second
HTTP deadline. The agent's software service retains its single query permit
until the upstream future finishes; its spawned work can outlive the HTTP waiter.
Consequently an unresponsive provider could keep later catalogue/planning requests
busy after the launcher had timed out.

The regression test delays both version and release metadata responses by
30 seconds, with production total/idle budgets. Before the change, neither query
finished within the test's 14-second guard. After the change, both return
`InstallError::Deadline` at the 12-second catalogue deadline, without retries or
filesystem changes. A separate test proves stricter caller deadlines are retained.

The fix tightens only public version/release catalogue jobs to 12 seconds. It does
not extend caller deadlines, cache errors, change selected software, or shorten
the independent planning/installation budget. Existing limits still cover bytes,
collection counts, redirects and retries. Query permits are released when these
calls return to the software service.

## Live read-only diagnostic

Command: `cargo run --locked -p vesta-server-install --example catalogues`.
The diagnostic now times queries, selects the newest numeric Minecraft release
instead of an arbitrary provider ordering, and exits unsuccessfully if any query
fails. It does not download artifacts, probe Java or launch processes.

These timings measure this Windows machine's direct connection. They are not
3G measurements, and do not include UI/agent round trips. Each provider is queried
once, so the figures are observations rather than latency percentiles.

| Software | Minecraft versions | Versions time | Sample version | Build/loader choices | Choices time |
| --- | ---: | ---: | --- | ---: | ---: |
| Vanilla | 908 | 321 ms | 26.3 | Not applicable | No upstream request |
| Paper | 68 | 56 ms | 26.3 | 49 | 89 ms |
| Folia | 13 | 46 ms | 26.3 | 0 stable | 378 ms |
| Purpur | 41 | 64 ms | 26.3 | 21 | 53 ms |
| Fabric | 520 | 639 ms | 26.3 | 1 | 46 ms |
| Quilt | 441 | 277 ms | 26.3 | 53 | 58 ms |
| Forge | 57 | 1,181 ms | 26.3 | 13 | 991 ms |
| NeoForge | 15 | 703 ms | 26.3 | 21 | 860 ms |

The [Folia 26.3 builds endpoint][folia] was checked separately. All three returned
builds were Alpha. The catalogue deliberately offers stable builds, explaining
the empty result. Vanilla has no separate build/loader selector.

## Automated validation

- Installer transfer tests: 16 passed. Includes stalled/cancelled requests,
  absolute deadlines, bounded retries, stream byte limits, malformed data,
  redirect/SSRF restrictions and cumulative budgets.
- Provider tests: 26 passed, including the two new deadline tests. Includes
  Vanilla/Paper/Folia/Purpur metadata, strict identities, malformed/duplicate
  entries, ordering and release-choice behaviour.
- Loader tests: 28 passed. Includes Fabric/Quilt catalogues and release choices,
  bounds, metadata disagreement and existing bundle integrity cases.
- Forge catalogue regression: 1 passed, including NeoForge mirror stability
  normalization.
- Launcher request-cache tests: 12 passed. Catalogue warmup/read-sharing test:
  1 passed, without reactive diagnostics. Eight unrelated reactivity tests were
  filtered out, not executed as part of that targeted command.

Total: 84 passing tests in this batch. No ignored tests in the selected groups.
Installer strict all-target Clippy also passed. Its existing Forge test module
was moved below production items to satisfy `items_after_test_module`, with no
logic change or warning suppression; the test passed again after the move.
Full command output, the failing-before regression and the live diagnostic are
preserved in `V:/tmp/metadata-20261026/`.

## Remaining work

- The five-minute launcher cache and same-key single-flight reuse are present.
  They do not constitute an agent-side cache shared by multiple launchers.
- Forge's cold version and release requests each re-read upstream catalogue
  information. A bounded, expiring shared metadata cache is a possible next
  optimization. Installation plans must still revalidate their exact selection
  and artifact pins. Do not remove DNS/source restrictions just to reuse HTTP
  connections.
- Real upstream providers were checked on a healthy connection. Combining the
  real agent/provider path with packet-level jitter, loss and limited bandwidth
  remains separate from the fixture tests and their artificial stalled providers.
- No claim is made that every server subsystem, historical Minecraft release,
  or actual Forge/NeoForge installer has been tested by this batch.

[folia]: https://fill.papermc.io/v3/projects/folia/versions/26.3/builds
