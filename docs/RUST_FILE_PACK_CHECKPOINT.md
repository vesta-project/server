# Rust-native file observations and pack reconciliation

Date: **2026-10-09**. Status: **implemented bounded library slice; corrected-source Windows verification and scoped file review completed; whole migration unfinished**.

This is actual implementation following `746bd64`, not another planning checkpoint. Java wrapper/core/plugin sources and frozen requirements remain intact. The implementation reuses canonical identity, protected SQLite transactions and the existing workspace rather than recreating Java service classes. No transport, privileged bootstrap, installer activation or file mutation route is supplied by this slice.

## Canonically authorized file observations

`vesta_control::files` provides:

- `RelativePath::parse`: a private owned nonempty ASCII path, forward slashes only, using canonical `Resource` validation and the stricter portable COM/LPT0 refusal. Traversal, drive/UNC/ADS/device names, controls, ambiguous separators, trailing dots/spaces and invariant protected names at any relative depth refuse. Root listing is selected explicitly with `None`, never an empty writable path. Paths/errors/contents have redacted `Debug`.
- `FileBrowser::open(db, root, protected)`: trusted host composition over an existing directory, with no creation/listener/authority. Caller-selected protected roots/files must be absolute; the canonical state directory is always excluded. Missing configured secrets retain their raw spelling and the longest existing ancestor's canonical alias plus the missing suffix. Windows drive/verbatim prefixes and ASCII case variants are accounted for. Known exclusions refuse before existence probes, both for the entire request and each child under its now-canonical parent.
- `list(&Subject, Option<&RelativePath>)`: reload current canonical principal/device/policy in a database snapshot; authorize both requested and actual on-disk resources and every returned child. Return deterministic relative names/type/size/modified-time metadata only. Hidden/nonportable/protected/linked entries do not bypass the independent **4,000 scanned / 1,000 returned** entry bounds; over-budget work refuses rather than silently returning an incomplete inventory.
- `read(&Subject, &RelativePath, limit)`: limit must be positive and at most **1 MiB**. Fresh policy checks cover the requested spelling, canonical on-disk case/alias and actual opened leaf. Return owned bounded binary bytes, their actual SHA-256 and observed metadata, not an absolute path or reusable read permit. A later disk replacement is not pinned by this response.

Native adapters use already-pinned `windows-sys`/`libc`, adding only target dependency edges to `vesta-control`; no packages were added or repinned. Windows uses a no-reparse leaf open, disk/regular-file/hard-link count inspection and actual handle path. Read sharing excludes concurrent write/delete sharing while held. Unix uses no-follow/nonblocking/cloexec leaf opening, regular-file/link count and native file identity checks. More-than-one-link files refuse reads and are omitted from listing rather than exposing an external secret through a hard-link alias.

**Important limits:** ancestor inspection/canonicalization remains check-then-use. The root binds to the configured canonical path, not the original directory's native identity: an owned regression explicitly observes a plain root renamed away and replaced at the same pathname. Existing protected-ancestor aliases are resolved at open; later same-account creation/replacement of aliases is not isolated. These are not handle-relative directory walks, native sandboxing or same-account race-free isolation. Bounds limit bytes/entries, not a deadline for a stalled filesystem syscall. Unix branches are source implementations, not executed platform evidence. ASCII scope/path support is intentionally conservative; Unicode files cannot acquire implicit authorization. An actively written Windows file may be unavailable under the stricter read-sharing rule. Management `Subject`s are trusted lookup keys, not proof supplied by a network caller.

This accounts for useful listing/read/protected-path behavior from `ServerPathPolicy` and `FileBrowserService`, **not the full Java families**: durable write/overwrite/delete/content-install, staging/publication, refresh integration, authenticated routes and coherent Unicode scope support remain unfinished. No public injectable permission callback, raw native-path escape, arbitrary handler registry, recursive deletion or world reset/restore was added.

## Validated pure pack catalogue and sync proposals

`vesta_protocol::pack::{catalogue,sync}` adds private validated application values independently of the existing data-only `BuilderManifest`/`VestaIndexV1` projection:

- `ManifestMetadata`, `Manifest`, `PackFile`, `FileSource`, `PlatformPack`, `ArchivePack`, typed `RelativePath`/`Sha256` and immutable observations.
- Strict separate schema-1 `Manifest::decode` and `InstalledState::decode`, with byte limits before decoding, a streaming duplicate-key refusal (including duplicate nullable fields), closed typed schemas and bounded array/string/aggregate budgets.
- Portable ASCII lexical paths with collision rejection at every directory/file level. Duplicate entries, case aliases, mixed directory spelling and file/directory conflicts refuse within each catalogue and across desired/installed namespaces. Lexical validation is not native confinement.
- Complete deterministic add/update/keep/remove proposals for custom catalogues, preserving old hash preconditions and desired descriptive provenance. Installed hashes are not proof of current disk bytes; a removal proposal is not deletion authority.
- Positive target/minimum metadata (`1 <= minimum <= target`) and bounded JSON-safe revisions. Unknown installed revision remains `Unverified`; installed zero/below minimum requires update; behind-but-compatible revisions require advancement; exact versions/revisions do not hide hash drift; ahead-of-target refuses implicit rollback.
- Platform/archive content remains descriptive and explicitly unsupported for per-file reconciliation, never an accidental empty-catalogue remove-all plan.

Budgets: 1 MiB JSON/aggregate text, 4,096 files, 1,024 path bytes, 128 bytes/component, 32 components, 256-byte display/version labels, 128-byte platform IDs, 2,048-byte descriptive URLs, 8 GiB/file and 128 GiB aggregate desired bytes. URL validity never authorizes a network request, including a private-network URL; no resolver/downloader is invoked.

The new JSON is an explicitly **Rust-native application schema**, not a fabricated drop-in Java/plugin/launcher wire replacement. Existing index projection/models/golden contracts are unchanged. Backslash repair, invalid-map-entry skipping, null manifest fallback, last-write-wins collisions and JDK ordering are not reproduced. Full server manifest/discovery metadata, source registry/resolution, configured filesystem catalogue, archive pinning/ZIP construction, safe sync transport and activation remain unfinished.

## Foundation assertion coverage and review corrections

Bunny's bounded accounting pass identified missing direct assertions in existing state/audit/event tests. Its new `crates/vesta-state/tests/foundation_bounds.rs` drives real owned SQLite fixtures through existing APIs:

- Foreign-server audit row, stored-hash tampering and previous-link tampering; corrupt chain opening refuses.
- Inclusive audit field limits, one-over refusal and duplicate metadata key rejection.
- Audit paging clamps, including the actual upper cap and continuation tail.
- Inclusive settings value bound, supplementary-scalar unit accounting and no orphan state/audit rows after refusal.
- Monotonic retention watermark and exact partial-prune bounds/gap/remaining cursors.

Tampering/trigger removal and the one fixture-only foreign-key relaxation are confined to owned temporary databases; production constraints are unchanged. The high-watermark case is an intentionally planted inconsistent fixture to exercise the monotonic update guard, not support for production corruption adoption. Existing persisted audit-v1/string-unit semantics are observed rather than extended into new Java emulation.

Parent review strengthened Bunny's paging assertion: a three-row chain cannot prove a 500-row cap. The fixture now appends **MAX_PAGE + 1** rows in one supplied writer, asserts the exact cap for oversized requests and verifies the next-page tail. The parent also replaced a negative planted watermark with a nonnegative lower watermark. No production invariant was relaxed to force tests green.

Parent review of the Sol-owned catalogue corrected COM0/LPT0 acceptance and zero target/minimum revision acceptance. Installed zero remains a valid descriptive below-minimum state. The final focused tests retain strict rejection instead of silently repairing input.

## Executed evidence and failed attempts

Native Windows 11 / Rust 1.96 only. No real Java/Minecraft/provider/installer/Mojang execution, EULA acceptance, world/demo removal, production exposure, service/firewall/router/signing changes or publication.

- Parent focused file observations: **16 passed, 0 failed/ignored**, including an actual owned Windows junction, hard-link alias, actual on-disk case checks, missing protected-secret spelling, state-root exclusion, scan/return/read budgets and canonical denial/revocation. Strict all-target control Clippy passed afterward.
- Sol-owned initial pack check correctly failed on `--locked` while the parent's new target dependency edges had not yet updated the lockfile. No surrogate standalone harness or helper-owned lock update was used. After the parent update, the actual protocol workspace commands passed.
- Sol-owned pack focused tests: **23 passed, 0 failed/ignored**, after device/revision corrections; strict all-target protocol Clippy and focused formatting passed. The earlier complete protocol suite passed **52 tests**, but that earlier source result does not substitute for final integrated verification after corrections.
- Bunny-owned new foundation target: **10 passed, 0 failed/ignored**, focused strict Clippy/format passed before the parent's upper-cap assertion correction. The first run was **9 passed / 1 failed** because an independent expected byte length for 2,048 supplementary scalars was wrong (8,192, not 16,384 bytes); the arithmetic was corrected without weakening the bound. A fresh parent integrated run must include the strengthened cap test.

The first fresh integrated run passed **707 workspace Rust tests across 39 result summaries**, zero failed/ignored, including the strengthened 10-test state target. Whole-workspace formatting, all-target check and strict all-target Clippy passed. Fresh **159 Node tests**, both accounting checkers and preserved-reference diff checks passed. Fresh companion all-target check and **210 tests** passed, with its pre-existing unused `tokio::task` warning unchanged. The protected launcher manifest digest remains `4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`; its inherited worktree modification was not edited or staged.

Independent Sol scoped file review then found the missing configured-secret ancestor-alias exclusion gap and a missing-versus-present existence oracle. The parent now resolves the longest existing ancestor and appends all missing components, refuses dangling unresolved aliases, and checks exclusions before both full-request and per-child native probes. The per-child guard also closes the source-proven 8.3-ancestor oracle after an earlier component canonicalizes; **no native 8.3 fixture was executed**. A real owned Windows junction regression covers multiple missing suffix components, before/after secret creation, real-path listing omission and a positively readable public sibling. Windows protected normal-component spelling validation refuses trailing dots/spaces/colon before native normalization. An owned root-replacement regression makes path binding explicit. Sol's read-only correction reviews confirm source closure; they are not the required whole-rebuild review.

An intermediate corrected focused file run passed **19 tests**, zero failed/ignored, and strict all-target control Clippy. A subsequent final verification attempt passed all-target check but failed **before tests** at the Windows linker: `LNK1104: cannot open ... vesta_control-0234294665ca7e24.exe`, while the previous workspace run was still executing the same test executable. Sharing that native output between overlapping runs was the parent's orchestration error, not passing source/test evidence. After the previous command actually finished, the fresh final post-per-child/preflight run passed **710 workspace Rust tests across 39 result summaries**, zero failed/ignored, plus whole-workspace format, all-target check and strict all-target Clippy (combined exit 0). Fresh Node/accounting/preservation checks also passed after the documentation updates. No ignored assertion or pending result is counted as a pass.

Final workspace command sequence (native Windows, Rust 1.96):

```sh
cargo fmt --manifest-path V:/server-connections-mvp/Cargo.toml --all -- --check
CARGO_INCREMENTAL=0 cargo check --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --all-targets --offline --locked
CARGO_INCREMENTAL=0 cargo test --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --offline --locked -- --test-threads=1
CARGO_INCREMENTAL=0 cargo clippy --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --all-targets --offline --locked -- -D warnings
```

Additional executed commands: `node --test --test-reporter=dot scripts/*.test.mjs`, `node scripts/rust-wrapper-parity.mjs --check`, `node scripts/requirements-ledger.mjs --check`, and preserved-byte `git diff --exit-code HEAD -- wrapper core/src docs/SERVER_SOFTWARE_REQUIREMENTS.md docs/REQUIREMENT_TRACEABILITY.json`. Companion all-target check and serial tests used its `crates/piston-lib/Cargo.toml`, `CARGO_INCREMENTAL=0`, `--offline --locked`. Source/index whitespace checks passed. These are scoped development checks, not release or whole-rebuild certification.

## Accounting and remaining gates

This slice does not blanket-promote Java file/content/manifest/transport classes, clear original requirements or mark the final independent review complete. Existing foundation accounting candidates require explicit behavior/test dispositions rather than name matching; a successful test count is not complete Java parity. Frozen Java hashes, 370 requirement IDs/text/status/history and platform/release/final-review gates remain unchanged.

Remaining useful-current-Java work includes file/content mutation composition, catalogue/archive host services, full trusted configuration/protected loading/host application, runtime discovery/player query, safe pack-session/transport boundaries, remaining foundation accounting and the required independent **whole-rebuild** Sol review. Separate product gates include OS bootstrap/device/mTLS/vault, full authenticated remote management, independent supervisor/IPC/crash continuity, process-tree isolation/readiness, durable installer activation/update/adoption/world recovery, managed Java/real loader matrix, services/distribution and non-Windows/MSRV/release execution.
