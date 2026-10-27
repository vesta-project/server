# Resource-pack hierarchy implementation

Implemented the backend ordered builder in `crates/vesta-control/src/resource_packs`.
The user approved priority resolution instead of blocking ordinary overlapping
assets. Inputs are ordered low to high; the higher file wins and the resolution
is recorded for inspection. No launcher screens were changed.

Language key precedence, sound append/replacement/subtitle rules, font fallback
ordering and sequential atlas sources preserve the verified stack behavior.
Mojang's official 1.21.4 client and mappings were downloaded as data with verified
manifest hashes. The installed JDK's `javap` inspected bytecode. The game was not
executed. Font provider order was checked algebraically across both the per-file
reverse and final aggregate reverse; the merged JSON uses higher arrays first.

Review also caught two archive/format edge cases. ZIP library indexing can hide
duplicate member names, so the raw central directory is validated before parser
allocation. Modern resource-pack metadata forbids `supported_formats` for
modern-only ranges, and scalar maximum formats permit all minor versions of that
major; generated metadata and compatibility checks now cover both rules. See
[Mojang's pack metadata changes](https://www.minecraft.net/en-us/article/minecraft-java-edition-1-21-9).

Builds stream media through fixed buffers into owner-only staging. Input digests
are checked before/after copying, and retained stages are rechecked before
content addressing. The existing ArtifactDirectory primitives provide no-replace
publication. A corrupt existing artifact is preserved/refused rather than replaced.
The module never edits active releases, Minecraft settings or live game files.

## Verification

- 21 focused tests passed on Windows and Ubuntu 24.04, no failures or ignored tests.
- Strict Clippy for the control library and tests passed on both platforms.
- The deterministic fixture produced identical SHA-256 on both platforms.
  `643f0ff6d7b8a421e3600dae14656cd1c87a5899707df0b59ccd8a5e0d5d421f`
- Tests cover reorder/winner behavior; deterministic/idempotent publication;
  language, sounds, subtitles, fonts and atlas merging; metadata compatibility;
  modern scalar/minor bounds; active filter/overlay refusal; traversal, duplicate
  ZIP paths, symlinks, CRC damage and forged directory counts; digest mismatch;
  input/output/JSON budgets; deadline handling; stage disposal; preservation of
  existing files; and tampering with a retained stage or existing artifact.
- No real resource-pack publication to players, network listener, Minecraft
  launch, EULA acceptance, live-server mutation or client rendering test occurred.

The ZIP dependency logs a finalization error while the deliberately tiny output
budget fixture aborts. The test checks the typed limit error and verifies the
private stage is removed and prior artifacts remain intact.

## Remaining integration

This is a working builder and immutable private artifact API, not public hosting.
Persisted ordered drafts/jobs, authority/revision/idempotency integration, public
HTTPS download origin/routes and guarded Minecraft configuration activation remain.
Opus retains UI ownership. Active pack-level filters/conditional overlays require
an order-aware transform; unclassified/legacy formats use whole-file priority and
need separate compatibility validation. The reference client check does not prove
all future/past Minecraft versions behave identically.

The exact API, budgets and integration obligations are in the
[builder contract](../operations/resource-pack-builder.md).
