# Pack planning performance

Measured against CurseForge Echoes of the Sift Fabric (`1714524` / `9024763`),
Minecraft 26.3, Fabric 0.19.5, in an owned temporary game/database fixture on
Windows. The same debug build profile and full verification path were used.
Every run selected 15 server files and skipped four client-only files. No live
server files were changed and Minecraft was not started.

## Measurements

Times below are cumulative seconds from entering `install_plan`, excluding
fixture setup and Rust compilation. These are live network measurements rather
than a latency guarantee.

| Stage | Before | Compact storage + metadata batches | Final live run |
| --- | ---: | ---: | ---: |
| Archive parsed | 3.866 | 2.371 | 3.378 |
| Provider metadata resolved | 9.691 | 2.417 | 3.427 |
| Downloads verified | 18.438 | 14.897 | 7.534 |
| File review complete | 20.341 | 16.099 | 8.775 |
| Client projection complete | 20.350 | 16.110 | 8.779 |
| Plan saved | 31.583 | 18.409 | 10.922 |

Earlier optimized runs measured 13.696 and 14.557 seconds, including one while
other fixtures were running. The observed reduction is approximately 55–65%.
The final run made one metadata GET and four batch POSTs, instead of the previous
39 individual metadata GETs. Provider latency and link speed can change results.

## Changes

- Use bounded CurseForge file/project batches to populate the existing per-job
  metadata cache. Normal resolution still binds project/file IDs and verifies
  source policy and hashes. Missing or unavailable batch results fall back to
  individual reads; batch attempts are remembered only for the current job.
- Reuse HTTPS connections within the job, with at most two idle connections per
  pool and per-request deadlines. No provider credential reaches a CDN request.
- Download at most two candidate files together under the unchanged global
  two-transfer ceiling. A scoped helper always joins, including on failure, and
  local contention waits without busy polling. Thread creation failure falls
  back to sequential work.
- Enforce selected-byte/file budgets while collecting results, retain only one
  pair of incoming download buffers, and hash reviewed bytes once rather than
  twice.
- Encode saved binary data compactly, remove unused duplicate blob copies, and
  serialize before taking the database write transaction. Read old numeric plans
  directly without allocating a generic serde value for each byte. The final
  transaction still rechecks the actor's authority.

The public plan/apply contract and the existing 128-file / 64-MiB staging limits
are unchanged. Larger pack staging remains separate work.

## Regression coverage

Coverage includes reordered/missing/wrong-project bulk rows; per-job cache and
batch-attempt bounds; overlapping downloads with stable manifest order; joining
failed transfers without partial results; admission release and deadline expiry;
incremental size/file caps; legacy and compact storage round-trips; reopened
legacy/compact plan application; config preservation and idempotency; and the
real provider-verified Echoes review.

Run using a target directory on a drive with room and the configured provider key:

```powershell
cargo test --locked -p vesta-control --lib packs::
cargo test --locked -p vesta-control --lib content::
cargo test --locked -p vesta-control --lib live_echoes_of_the_sift_review_does_not_stage_files -- --ignored --nocapture
cargo test --locked -p vesta-transport --lib
cargo clippy --locked -p vesta-control -p vesta-transport --lib -- -D warnings
```

Only the opt-in live review requires provider access. Stage timings and request
counts are compiled into tests only, not production logging.

Final scoped results: 145 pack/resource-pack tests passed (live review opt-in),
38 content tests passed (standalone live-key probe opt-in), the explicit live
Echoes review passed, and 50 transport unit tests passed. Clippy for control and
transport libraries passed with `-D warnings`. The existing live review covers
actual API authentication and checksum-verified CDN downloads; no live modpack
apply or Minecraft launch was performed.
