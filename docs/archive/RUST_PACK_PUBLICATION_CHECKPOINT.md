# Protected content-addressed pack publication checkpoint

Date: **2026-10-09**. Status: **implemented and Windows-tested; migration unfinished**.
One bounded library slice. It is not catalogue selection, a download route, pack-only
sessions, launcher trust, configured host composition or the final independent review.

## Rust-native boundary

`vesta-state::StateDirectory::artifacts()` creates or verifies `<state>/artifacts`, an
owner-only flat directory inside the canonical protected state root (outside the game
tree). `ArtifactDirectory` offers only never-replacing primitives over validated flat
names (`ArtifactName`: lowercase ASCII alphanumerics, `-`, `.`, alphanumeric edges, no
`..`, at most 96 bytes): create-new owner-only files, no-replace hard links, removal of
one plain file name, single-link opens and a bounded entry count. Every primitive
re-checks the state root spelling and that the artifact directory is a plain,
non-reparse directory. Linux additionally syncs the directory after link/remove; Windows
has no portable directory sync, which is a recorded power-loss durability limitation.

`vesta-control::packs::PackArtifacts` publishes a `BuiltVestaPack` or opaque pinned
`PreparedArchive` (never parsed or extracted) under `<sha256>.<vestapack|zip|mrpack>`:

1. Re-hash the owned bytes; a disagreeing digest refuses (`pack_artifact_conflict`).
2. If the content name exists, verify it: identical bytes are idempotent success; any
   difference refuses and leaves the existing file byte-for-byte intact.
3. Refuse beyond **1,024 directory entries** (stale stages count), bounding disk work.
4. Write a unique random `stage-<128-bit hex>.tmp` create-new file, sync and close it.
5. Hard-link it to the content name without replacement. A concurrent identical winner
   is verified rather than trusted. The stage name is then retired.
6. Re-open the published single-link file and verify its size and SHA-256.

`load` checks limits before any read, refuses a missing name (`pack_artifact_not_published`),
an extra hard link/reparse point (`pack_path_linked`), and any size/digest disagreement
(`pack_artifact_conflict`). Bytes are never served from a filename merely because it has
the expected name. Every exit path, including failures after linking, drops the stage
name; earlier artifacts are never replaced, truncated or removed by later publications
or failures. Debug output omits digests, paths and bytes; errors are fixed classifications.

This replaces the Java builder's game-root `.vesta/pack.vestapack` output, fixed `.tmp`
sibling and replace/atomic-move fallback, and the Java archive model's later reread of a
mutable resolved path. It is a deliberate safer replacement, not Java emulation.

## Executed tests

Real owned temporary SQLite state and directories (`packs/publish_tests.rs`, plus the
`ArtifactName` grammar unit test): content addressing and idempotence; format as part
of identity; coexistence of later publications; refusal of a disagreeing digest without
writes; a pre-existing conflicting name left intact; injected failure before linking
(nothing published, no stage, earlier artifact intact, retry succeeds); injected failure
after linking (stage retired, published file single-link, retry idempotent); modified,
truncated, extended and deleted published bytes never served; an extra hard link refuses
loads until removed; an actual Windows junction in place of the artifact directory
refuses opening with its owned outside target untouched; a junction substituted after
opening refuses both loads and publication; the inclusive 1,024-entry bound; load limits
before reads; native archive and built-pack publication surviving later game-tree source
replacement with nothing written into the game tree; and redaction.

Focused results: **1 state test** and **14 publication tests** passed first time on
Windows 11/Rust 1.96 (`CARGO_INCREMENTAL=0`, offline/locked, serial). The first chained
full verification then failed (exit 101) at strict Clippy on a redundant `clone` of the
`Copy` digest type; it was replaced with a copy, not suppressed, and the chained tests
were not reached. The rerun passed format check, all-target check, strict all-target
Clippy and **896 workspace tests / 42 full summaries** (881 previous + 15 new), zero
failed/ignored/filtered, exit 0. Fresh **159 Node tests** and both frozen accounting
checkers passed; no manifest, lockfile or Java/reference bytes changed, and the protected
launcher manifest SHA-256 remains
`4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`. The companion was not
rerun because nothing it builds changed; its last fresh result is 210 tests at `d7d1aff`.

Three inventory rows gain scoped evidence: `VestaPackBuilder` (safer publication
replacement), `VestaPackBuilderTest` (its regular-file assertion now maps to verified
single-link publication) and `LocalArchivePack` (partial mapping: pinning plus
publication; archive-mode manifest/catalogue/route and configured selection remain
gaps). All remain `not_migrated` with whole-row results `unverified`.

## Residual limitations and remaining gates

- Path checks are same-account hygiene, not race-free confinement. An ancestor can be
  raced between inspection and use; content addressing plus verification on every load
  prevents serving different bytes under a published identity, not denial of service.
- A crash can leave a stale stage name. There is no stage or artifact pruning API yet;
  safe pruning needs catalogue reference tracking. Stale names count against the bound.
- If a stage name cannot be retired after linking, the published name stays two-link
  and loads refuse until explicit reconciliation; no automatic destructive repair.
- Catalogue selection/revision records, authenticated pack-only sessions and download
  routes, full host composition, remaining useful-current-Java family/requirement
  accounting and the separate final independent whole-rebuild review remain unfinished.

No Java/Minecraft/provider/Mojang execution, network listener, service, exposure,
signing change, push, PR or publication outside owned temporary fixtures occurred.
