# Owned-byte hybrid Vesta ZIP checkpoint

Date: **2026-10-09**. Status: **implemented and Windows-tested; migration unfinished**.
This is one bounded library slice, not completion of the wrapper migration, safe disk
publication, authenticated serving, launcher trust or the final independent review.

## Rust-native boundary

`vesta-control::packs::build_vesta(&PreparedSnapshot, &PackMetadata, VestaBuildLimits)`
returns an immutable `BuiltVestaPack`. The private preparation holds owned snapshot
bytes; the result exposes only bytes, typed SHA-256, size and format `vesta`. Debug
shows size/count, not paths, content, URLs or fingerprints. Errors have six fixed
classifications and no nested native/JSON/ZIP diagnostics.

The builder never reopens a path, reads the live game tree, downloads a provider,
extracts an incoming archive, accepts a destination, writes a disk artifact, starts
Minecraft or grants any caller access. Its inputs are trusted backend preparation and
validated host metadata, not a transport DTO. Existing opaque local archive preparation
remains separate and never extracts incoming contents.

- Snapshot and host name/version/revision/minimum-revision must agree exactly.
  Minecraft and optional loader information derive only from validated host metadata.
- Every described file must have exactly one retained body with the advertised size,
  SHA-256 and, when present, SHA-1. Extra/missing/substituted bodies, accounting errors,
  per-file overflow and total overflow refuse without an artifact.
- Embedded and Server sources become `overrides/<portable path>` members. Platform
  sources remain reference entries in the index; their retained bodies are verified
  but never embedded. References require positive size and computed retained-byte
  SHA-1. SHA-1/CRC32 are consistency metadata, never authenticity or authority.
- Modrinth project/version IDs are exactly eight ASCII alphanumerics and require the
  exact HTTPS `cdn.modrinth.com/data/<project>/versions/<version>/<filename>` family.
  CurseForge IDs are canonical positive `u32` decimals; URL is optional, and when
  present must match `edge.forgecdn.net` or `mediafilez.forgecdn.net` and the exact
  file-ID-derived path. Explicit default HTTPS port is accepted; nondefault ports,
  credentials, queries, fragments and nested filenames are refused.
- Raw URL spellings are checked before WHATWG normalization: non-ASCII, percent
  encodings, backslashes, whitespace/control, oversized URLs and normalized dot-path
  variants refuse. This is a deliberate strict subset, not broad Java URL emulation.
- Catalogue admission remains portable ASCII-only. Duplicate/case-alias/file-directory
  namespace rejection comes from the immutable validated catalogue. No Unicode file
  authorization or namespace support is claimed by using a ZIP library.

## Bounded deterministic output

The ceilings are **1 MiB compact index**, **68 MiB ZIP**, **4,097 actual ZIP entries**,
including the root index. Provider references consume no ZIP member. Snapshot work is
independently bounded to **4,096 files**, **16 MiB per retained body** and **64 MiB total**.
Limits are positive and inclusive. Checked arithmetic and fallible bounded reservation
are used; these are output/work budgets, not a claim of an exact allocator RSS ceiling.

The existing descriptive `project_index` provides the shared-wire Vesta projection;
this builder, not that permissive projection, admits body integrity and provider
references. ZIP uses pinned `zip 8.2.0`, default features disabled, Stored compression,
fixed Unix creator, DOS 1980-01-01 timestamp, permissions `0644`, sorted override members
and the root index last. No directory entries, comments, encryption or optional extras
are requested. Semantic compatibility, not incidental Gson/JDK compressed ZIP bytes,
is the target.

Before constructing the writer, checked preflight computes a conservative minimum:
EOCD 22 plus each body's length, local header 30, central header 46 and both UTF-8
filename copies. The seekable bounded sink remains authoritative for actual bytes and
any future library overhead. It permits backpatches only within existing bytes and
latches the first failure. Failed writes/finalization cannot yield an artifact.
ZIP Drop can retry finalization and emit a diagnostic on exceptional failure; sink
errors contain only `Vesta pack buffer failed`. An intentional footer-failure fixture
exercises that behavior rather than suppressing it. Ordinary cap refusal happens before
writer creation. See [dependency intake](DEPENDENCY_DECISIONS.md#owned-byte-hybrid-vesta-zip-intake-2026-10-09).

## Independently checked assertions

`packs/build_tests.rs` contains hand-authored compact mixed/empty index expectations
and an independent manual ZIP inspector: local/central/footer signatures, extents,
member order, CRCs, Stored method, permissions, fixed timestamp, creator and absence of
hidden/trailing members. It does not depend on the ZIP reader to certify writer output.
It also covers repeatable bytes/digests, each loader, metadata/body/reference rejection,
URL adversaries, inclusive limits, maximum count/retained bytes, checked overhead,
backpatch refusal, actual footer failure and redaction.

`packs/tests.rs` additionally composes strict host metadata, configured protected launch
path, canonical database exclusion, native owned snapshot and builder. It checks the
independent frozen `core/src/test/resources/vesta-builder-v1.json` metadata/source/loader
shape. The retained Java hybrid test's literal `known` provider body independently
matches both provider fingerprints in the frozen JSON; `embedded` matches its embedded
fingerprints. The expectation changes no provider hash or other fixture metadata;
only ordinary embedded SHA-1 is omitted because it is optional. No frozen fixture bytes
are changed, and no expected hash is derived from builder output. Overwrite/deletion after preparation must
leave repeated ZIP bytes/digest unchanged; the configured secret remains untouched.
This proves fixture-level shared-wire semantics, not actual production launcher execution.
Configured multi-root enforcement and complete host composition remain separate work.

### Current-Java assertion-family disposition

Parent inspected both retained Java test methods and the production builder rather
than treating a file mapping or a nearby test count as full parity:

| Useful assertion / behavior | Rust evidence or disposition |
| --- | --- |
| `VestaPackBuilderTest#embedsUnknownFilesAndLeavesKnownSourcesExternal`: format, root index, provider omission, Server-to-embedded conversion, file count/order and source kinds | Independent mixed-index/ZIP test covers these with admitted real hashes and provider IDs, not Java's placeholder `aa`/`bb` hashes, short IDs or arbitrary URL. Native integration covers actual retained public bytes and a configured secret exclusion. |
| The same Java method: returned path is a regular disk file | Still required application work: safe staging/publication is not replaced by an in-memory result or marked passed. |
| `VestaPackInteroperabilityTest#javaBuilderMatchesTheLauncherWireFixtureIncludingProviderAliasesAndHashes`: name/version/revisions/Minecraft/loader/dependencies, provider aliases/URLs/hashes and embedded/reference membership | Native host/snapshot/build fixture and independent compact mixed index cover semantic fields. Optional embedded SHA-1 absence is explicit; no Gson/JDK byte-for-byte claim or actual Java/launcher execution. |
| Production builder: read mutable paths during ZIP writing | Safer replacement: immutable verified retained bodies; overwrite/deletion cannot change the already prepared artifact. |
| Production builder: sibling fixed `.tmp`, replacement and atomic-move fallback | Not copied. Future publication must use unique create-new staging and preserve the previous artifact on returned failures without an unsafe destructive fallback. |
| Production builder: arbitrary descriptive sources and unverified supplied hashes | Safer replacement: typed immutable catalogue, independently checked bodies and provider reference admission. Unknown registry paths alone become embedded; registry errors propagate. |

The host builder does not replace retained Java plugin consumers. Required disk effects,
whole-family requirement mapping and final independent review remain open, so this table
is scoped assertion accounting, not promotion of either Java family or the production row.

## Executed evidence and attempts

The initial offline dependency compilation passed but reported incremental-cache
permission notes. Settled checks use `CARGO_INCREMENTAL=0` on Windows 11/Rust 1.96.0.
No declared minimum or non-Windows platform is tested by that toolchain.

The first integrated compile failed (101) on the parent's incorrect `CurseForge` variant
spelling; it was corrected to the actual typed `Curseforge`. The next focused suite had
30 passes/one failure because owned `dummy` bytes were paired with unrelated frozen
provider hashes. After using independently computed literal vectors, another run had
31 passes/two failures: a Unicode path contradicted the existing ASCII admission policy,
and a supposed secret filename had not been configured as protected. The tests were
corrected to assert Unicode refusal and use the actual configured launch-secret exclusion;
production path/source admission was not relaxed. Strict Clippy chained after these
failed suites was not reached.

All **33 focused pack tests** subsequently passed, including the host integration and
intentional fixed-text ZIP Drop diagnostic. Strict Clippy then rejected six existing
source expressions under the control crate's newly explicit Rust 1.88 declaration:
collapsible conditional patterns and a manual multiple test. Equivalent let-chains and
`is_multiple_of` preserve short-circuiting, policy order and rejection behavior without
lint suppression. The next all-target check passed, but Clippy found the same conditional
pattern in the transaction-clock test; that fixture was corrected as well. Full tests
chained after those failed Clippy attempts were not reached.

An interim direct, unfiltered full run passed **881 workspace tests / 42 summaries**,
zero failed/ignored/filtered, together with format/all-target check/strict Clippy.
Parent then inspected the retained Java hybrid fixture's `known` literal and independently
confirmed it matches both frozen provider fingerprints. The integration test was strengthened
to use that vector and compare provider hash fields unchanged instead of adapting them to
`dummy` bytes. The fresh final-source run then passed **881 workspace tests / 42 result
summaries**, zero failed/ignored/filtered, with successful format/all-target compilation
and strict all-target Clippy. The intentional fixed-text footer-failure diagnostic appears
in the successful test output; it is not claimed to be a silent run.

Exact final workspace commands:

```sh
cargo fmt --manifest-path V:/server-connections-mvp/Cargo.toml --all -- --check
CARGO_INCREMENTAL=0 cargo check --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --all-targets --offline --locked
CARGO_INCREMENTAL=0 cargo clippy --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --all-targets --offline --locked -- -D warnings
CARGO_INCREMENTAL=0 cargo test --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --offline --locked -- --test-threads=1
```

The direct unfiltered shell command exited **0**; all 42 full result summaries were
counted, not a truncated/filtered tail or focused-suite total. Fresh companion
`piston-lib` all-target check and serial offline/locked tests passed **210 tests / 9
summaries**, zero failed/ignored, with its existing unused `tokio::task` warning retained.
Fresh Node contracts/accounting passed **159 tests**, zero failed/skipped, and both
`rust-wrapper-parity.mjs --check` and `requirements-ledger.mjs --check` passed after the
three partial builder/test-row mappings. Their statuses remain `not_migrated` with
`unverified` whole-row results; all Java hashes and 370 original requirement bullets
are preserved.

Final lock comparison confirmed only `crc32fast 1.5.0`, `typed-path 0.12.3` and `zip
8.2.0` were added, each matching the companion registry identity/checksum; no retained
registry package identity changed. Protected launcher manifest SHA-256 remains
`4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`.

## Remaining gates

Durable file/content effects, safe unique disk staging/publication preserving prior
artifacts on failure, configured catalogue/archive services, pack-only sessions/routes,
full host composition, useful-current-Java assertion-family accounting and separate
independent whole-rebuild Sol review remain unfinished. The ZIP source author was a
configured Sol; parent independently inspected/integrated it and owns executed checks.
That is not the final independent review. Inventory remains **26 migrated / 17 retained
Java plugin / 179 not_migrated / 0 verified**, with **370** frozen requirements unchanged.
No product, MSRV, platform, provenance, SBOM/notices, vulnerability or release gate is
promoted. No provider/Java/Minecraft/Mojang execution, EULA acceptance, services, exposure,
signing change, push, PR, publication or release occurred.
