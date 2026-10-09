# Dependency intake decisions

Date: **2026-10-04**. This records the foundation batch's new dependency; it is not a complete license, SBOM, vulnerability, or signed-distribution audit.

## SQLite JDBC — accepted for agent-only durable state

- Coordinate: `org.xerial:sqlite-jdbc:3.53.4.0`, pinned in `wrapper/build.gradle.kts`.
- Scope: Java 17 wrapper/agent only. Java 8 shared `core` and connector dependency graphs do not acquire this JDBC dependency.
- Reason: use real SQLite transactions/WAL, foreign keys, integrity checking, schema versioning, and the driver's supported online backup facility; a hand-written JSON store cannot meet the accepted crash-safety contract.
- Alternative not chosen: copy the live database/WAL directly before migration. A supported online backup is used and independently opened/checked instead.
- License evidence: the resolved Maven POM declares **Apache License 2.0**, linking `https://www.apache.org/licenses/LICENSE-2.0.txt`; upstream is `https://github.com/xerial/sqlite-jdbc`.
- The cached JAR SHA-256 observed in this environment was `bcb1f51e36f940867e83342f9efbf5968ac44a6bef4d397bb4af7b17b45cd2fb`. This records local intake only; it is **not** a trusted signature or Gradle dependency-verification lock.
- A case-insensitive inspection of this cached JAR found no `license` or `notice` filename. Do not infer that distribution notice obligations have been satisfied from the POM alone. Release packaging must obtain and preserve applicable driver/native licenses and notices and verify the packaged artifact against trusted release provenance.

### Operational implications

- The driver includes/loads native SQLite code. Owner-controlled native extraction/loading, supported CPU/OS targets, temp-directory permissions, service identities, executable policy, and runtime native-access behavior need clean Windows/Linux packaging tests. The database's owner-only ACL does not certify the native loader or isolate a game under the same OS account.
- State must remain on a supported local filesystem outside the live game/file-browser root. Linked database/WAL/SHM/journal/backups children reject, but Java checks retain TOCTOU limitations. Native/local-filesystem enforcement and platform support remain open release gates.
- SQLite schema support is currently version 1. Newer schemas reject; migration gets an online backup and integrity checks. Tests cover rollback/corruption/reopen/foreign keys, not arbitrary future migration compatibility or downgrade support.
- Retention, reserved audit capacity, disk-full behavior across all callers, long-running executor storage, and complete backup/export/recovery are not implemented by adding JDBC.

### Pending release intake

1. Generate the full runtime/native/transitive SBOM and dependency verification metadata.
2. Verify trusted artifact provenance and vulnerabilities at the approved release cutoff; establish an update policy without silently changing pins.
3. Collect applicable driver/native/transitive notices in distributable bundles and review project-license compatibility.
4. Prove native loading and WAL/ACL/filesystem behavior under clean supported Windows and Linux service accounts, including failed permissions, disk-full, and recovery drills.
5. Repeat dependency intake for planned restic, ACME/SFTP, installer/service integration, and signed update tooling before enabling those adapters.

The existing BouncyCastle dependency already supplies Argon2id/Ed25519 primitives. This checkpoint does not add a second crypto library or implement the vault/mTLS/signature contract; existing bcrypt-backed MVP authentication remains pending replacement under P2.

## md-5 — accepted narrowly for published Purpur consistency checks (2026-10-05)

- Coordinate: `md-5 = "=0.10.6"` in companion `launcher-server-mvp:crates/piston-lib/Cargo.toml`; Rust crate name `md5`.
- Scope/reason: the official Purpur build API publishes MD5, not SHA-256. The new local server installer checks that published digest and separately records observed SHA-256. MD5 is explicitly legacy consistency evidence, **never authenticity, password hashing, signature verification or administrative trust**. SHA-1 receives the same legacy-warning/acknowledgment treatment.
- Alternatives rejected: handwritten MD5; accepting hashless artifacts; pretending a locally computed SHA-256 was published/authenticated by upstream.
- Cached crate metadata declares `MIT OR Apache-2.0`; upstream repository is `https://github.com/RustCrypto/hashes`. This is local metadata intake, not a complete distribution-license/provenance/vulnerability certification.
- Version 0.10.6 was already locked transitively and cached. The only Cargo.lock change is adding the piston-lib dependency edge; no package version/source/checksum updates or dependency fetch were needed. Existing sha1/sha2/digest 0.10 infrastructure is retained.
- Existing pinned `windows-sys 0.61.2` additionally enables `Win32_Storage_FileSystem` and `Win32_System_WindowsProgramming` for read-only `GetDriveTypeW` local-volume admission. No new crate or OS service is introduced, and this does not supply path-handle confinement or mapped-volume alias certification.
- Release intake remains open: full SBOM, trusted locked-artifact provenance/vulnerability review, licence/notice packaging and clean supported-platform behavior. See [automatic-provider checkpoint](AUTOMATIC_JAR_PROVIDER_CHECKPOINT.md) for fixture scope and remaining gates.

## Rust host-wrapper state and discovery intake (2026-10-07)

Scope: the new root Cargo workspace alongside the retained Java builds, currently
`vesta-protocol` and `vesta-state`. These are non-publishable development packages;
this intake is not a distribution, vulnerability, native-isolation or parity certification.

| Direct pin | Purpose | Licence declared by resolved crate metadata | Upstream |
| --- | --- | --- | --- |
| `rusqlite = 0.40.2` | Mature SQLite API, transactions and supported online backup | MIT | https://github.com/rusqlite/rusqlite |
| `fs2 = 0.4.3` | Lifetime-held advisory operation-runner file lock | MIT / Apache-2.0 | https://github.com/danburkert/fs2-rs |
| `uuid = 1.23.2` (`v4`) | Stable generated server UUID | Apache-2.0 OR MIT | https://github.com/uuid-rs/uuid |
| `sha2 = 0.10.9` | Existing audit hash contract; not identity/authority | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| `hex = 0.4.3` | Audit digest spelling | MIT OR Apache-2.0 | https://github.com/KokaKiwi/rust-hex |
| `serde = 1.0.228`, `serde_json = 1.0.149` | Bounded typed discovery and fixtures | MIT OR Apache-2.0 | https://github.com/serde-rs/serde ; https://github.com/serde-rs/json |
| `windows-sys = 0.61.2` | Isolated native Windows ACL/volume/path adapter | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| `libc = 0.2.183` (Linux only) | Native no-follow/directory open flags | MIT OR Apache-2.0 | https://github.com/rust-lang/libc |
| `tempfile = 3.27.0` (tests only) | Real temporary SQLite/filesystem fixtures | MIT OR Apache-2.0 | https://github.com/Stebalien/tempfile |

All direct versions are exact workspace pins, with registry source/checksums and
transitives in `Cargo.lock`. Locked dependency fetch and Windows library compilation
succeeded. They establish dependency availability/compilability, not trusted-release
provenance or absence of vulnerabilities. The initial lock contains 61 resolved packages;
not every target-specific lock entry is loaded into the native Windows runtime.

`rusqlite` disables default features and enables only `bundled` and `backup`.
The observed native Windows normal/build graph selects `libsqlite3-sys 0.38.2`
(MIT metadata, same rusqlite upstream), compiling bundled SQLite C into the binary.
Its selected `sqlite3/sqlite3.h` identifies SQLite **3.53.2**, source ID
`2026-06-03 19:12:13 d6e03d8c777cfa2d35e3b60d8ec3e0187f3e9f99d8e2ee9cac695fd6fcdf1a24`.
SQLCipher is not enabled; wasm-specific optional packages are not the native runtime.
Unlike JDBC, this does not use the Java driver's runtime native-library extraction.
It still executes native dependency/build code and requires clean supported-platform
packaging, compiler/provenance, licence/notice and update-policy review. SQLite upstream's
public-domain status does not discharge the wrapper/transitive licence obligations.

Alternatives rejected: hand-written SQLite FFI; copying a live main database/WAL as a
backup; importing Java JDBC into Rust; or accepting unrestricted filesystem storage.
The supported online backup is reserved no-clobber, checked and converted to a standalone
SQLite artifact. Schema support is now version **5** (2026-10-08 partial expiry indexes); the earlier JDBC section records
its historical version-1 checkpoint, not the current migration's supported version.
Never run Java and Rust production writers against one live database.

`fs2` is an advisory ownership mechanism, not same-account confinement. Windows native
unsafe calls are isolated to the ACL adapter; its owner-SID-only protected DACLs and
Linux modes/path checks are storage hygiene, not an independent supervisor or game
security boundary. Linux native compilation/tests and other architectures remain
unverified at this intake. Full SBOM, vulnerability/provenance verification, notices,
MSRV execution, service-account packaging, disk-full/recovery drills, signing and release
approval remain pending.

## Diesel assessment for the Rust-native state increment (2026-10-07)

Decision: retain the pinned `rusqlite = 0.40.2`; do not add Diesel for this increment.
This is a bounded read-only assessment, not a blanket claim that Diesel is unsafe or
that an ORM is categorically incompatible with the product.

Released/cached Diesel `2.3.9` offers useful typed queries and an `immediate_transaction`
that could support same-connection authority, mutation and audit work. Its released
SQLite connection setup, however, hardcodes `SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE |
SQLITE_OPEN_URI` and exposes no supported `NOFOLLOW`/open-flags configuration or existing
handle adoption. It therefore cannot replace the current protected native database open
without weakening it. Do not work around this with a precheck, invented URI option, custom
VFS, raw FFI or second application connection. PRAGMAs already provide the read-side
settings needed by this design.

There are also dependency constraints: Diesel 2.3.9's `libsqlite3-sys >=0.17.2,<0.38.0`
conflicts with current rusqlite's `libsqlite3-sys ^0.38.2` under Cargo's native
`links = "sqlite3"` resolution rule, and Diesel 2.3.9 requires Rust 1.86 while the
workspace declares 1.85. No Cargo resolution or candidate build was performed. The
released 2.3.9 API lacks `vacuum_into`; development-main source is not release evidence.
The needed online backup is already supported separately by rusqlite, so do not adopt or
downgrade SQLite dependencies merely to obtain an ORM. Reconsider only if a reviewed,
released opening API preserves protected flags and a real resolver/MSRV assessment removes
the blockers.

Source inspection (not executed feature-compatibility evidence): [Diesel 2.3.9 SQLite
connection source](https://docs.rs/diesel/2.3.9/src/diesel/sqlite/connection/raw.rs.html),
[Diesel 2.3.9 SQLite API](https://docs.rs/diesel/2.3.9/diesel/sqlite/struct.SqliteConnection.html),
and [Cargo's native `links` resolver rule](https://doc.rust-lang.org/cargo/reference/resolver.html#links).

## `vesta-control` canonical-identity increment intake (2026-10-08)

Admitting the third workspace member, `vesta-control`, added two direct pins and one
dev-dependency. Both new pins were already resolved transitively in the existing lock, so this
batches no new package versions and needs no new native build.

| Direct pin | Purpose | Licence declared by resolved crate metadata | Upstream |
| --- | --- | --- | --- |
| `base64 = 0.22.1` | Canonical bootstrap-code text and bounded password-PHC field encoding/parsing | MIT OR Apache-2.0 | https://github.com/marshallpierce/rust-base64 |
| `getrandom = 0.4.3` | OS CSPRNG for code bytes and generated identifiers | MIT OR Apache-2.0 | https://github.com/rust-random/getrandom |
| `serde_json = 1.0.149` (tests only) | Read the independent identity constants fixture in `protocol/identity-v1/fixtures/` | MIT OR Apache-2.0 | https://github.com/serde-rs/json |

- `base64` is used for bounded encoding and decoding at protocol/storage boundaries. A bootstrap
  code is 20 random bytes rendered as exact canonical Base64URL; `CodeInput::parse` re-decodes and
  re-encodes to reject non-canonical spelling. The password PHC parser also decodes bounded salt and
  output fields and re-encodes them to enforce canonical unpadded Base64. This crate supplies the
  standard codec only; it does not implement the password KDF. Alternatives rejected: hand-rolled
  Base64, and a lowercase-hex or truncated code that would reduce entropy or make case handling
  ambiguous.
- `getrandom` is the OS CSPRNG. It was already in the lock through `uuid`'s `v4` feature;
  declaring it directly removes the need to hand-roll identifier bytes. No custom RNG, no
  time-seeded value and no fallback path is used: a failure to obtain randomness is a real error.
- Rejected alternatives: adding Diesel (see the assessment above), a `time` crate for presentation
  (this increment records epoch milliseconds at the persistence boundary and performs no
  timestamp formatting), and any `test-support` Cargo feature — test-only seams stay `#[cfg(test)]`
  inside the crate.
- `sha2`/`hex` remain the digest contract for stored code fingerprints and nonce hashes; `uuid`
  remains the identifier generator. No cryptographic verification, signature, key agreement or
  vault capability is claimed by this increment.

Operational implications, unchanged and still open: `rusqlite` still compiles bundled SQLite C,
and `vesta-control` adds no new native code, driver, service or network listener. Dependency
availability and compilation are not trusted-release provenance, vulnerability review, licence or
notice packaging, MSRV execution or clean supported-platform service behaviour. Linux, macOS and
non-Windows execution remain unverified. The full SBOM, vulnerability/provenance review, notice
collection, service-account packaging, disk-full/recovery drills, signing and release approval
remain pending for every entry above.

## Rust password-KDF intake (2026-10-08)

The canonical-identity workspace now has a bounded Argon2id password primitive in
`vesta-control`. Passwords remain an alternative to the expected Microsoft/Minecraft identity flow,
not administrative authority. This intake records the dependency choices and local resolver evidence;
it is not a security certification or release approval.

| Direct pin | Enabled features | Purpose | Declared licence | Declared MSRV | Upstream |
| --- | --- | --- | --- | --- | --- |
| `argon2 = 0.6.0` (`default-features = false`) | `alloc`, `zeroize` | Argon2id KDF implementation | MIT OR Apache-2.0 | 1.85 | https://github.com/RustCrypto/password-hashes/tree/master/argon2 |
| `zeroize = 1.9.1` (`default-features = false`) | `alloc` | Wipe owned password, verifier, salt, output and working-memory buffers | Apache-2.0 OR MIT | 1.85 | https://github.com/RustCrypto/utils |
| `subtle = 2.6.1` (`default-features = false`) | none | Constant-time equality for equal-length derived-output slices | BSD-3-Clause | Not declared in cached crate manifest; MSRV unknown | https://github.com/dalek-cryptography/subtle |

`argon2` uses `hash_password_into_with_memory` with a caller-owned
`Zeroizing<Vec<Block>>` working buffer, plus zeroizing salt and output buffers. `subtle` compares the
already length-validated, equal-length output slices; this narrow use is not a claim that all of
verification or the surrounding application is constant-time. Password PHC parsing remains a small,
bounded in-crate parser, using the already-pinned `base64` codec for canonical unpadded field decode
and round-trip checks. The optional `password-hash`/`phc` APIs are not enabled or used.

The workspace pins use `default-features = false` deliberately. The selected normal/build graph
contains Argon2's `alloc` and `zeroize` path and does **not** contain optional `password-hash` or
`phc`; Argon2's `alloc` feature refers to `password-hash?/alloc` weakly and does not activate that
optional dependency. Cargo.lock gained **14 package entries** for this intake, including packages
needed by the selected cryptographic graph and lock-resolved optional entries. A locked offline
normal/build tree inspection completed successfully; it is graph/availability evidence, not a build.

Cargo's resolver selected `ctutils 0.4.2` as compatible with the workspace's declared Rust 1.85
floor based on cached registry MSRV metadata; `ctutils 0.4.3` declares Rust 1.87 and was not selected.
That resolver choice is **not** Rust 1.85 compilation or test evidence. Argon2 and zeroize declare
Rust 1.85 in their cached crate manifests. Subtle's cached manifest has no `rust-version`, so its
MSRV remains unknown.

The password intake's locked fetch and dependency-tree evidence establish local package availability
and selected feature resolution only. They do not establish trusted artifact provenance, vulnerability
status, a complete SBOM, complete license/notice review, or license-compliant release packaging.
Those release gates remain open, as do MSRV execution and supported-platform verification.

## Minecraft `hasJoined` HTTPS intake (2026-10-08)

`vesta-control` adds `ureq = "=3.3.0"` as a normal dependency, declared in the workspace with
`default-features = false` and only the `rustls` feature enabled. Existing exact workspace pins
`serde = 1.0.228` and `serde_json = 1.0.149` are also direct normal dependencies for the
bounded response model. This intake is for a fixed Minecraft identity-provider
`hasJoined` HTTPS verification call; it is not a general-purpose URL fetcher, identity
certification or release approval.

| Direct pin | Enabled features | Purpose | Declared licence | Declared MSRV | Upstream |
| --- | --- | --- | --- | --- | --- |
| `ureq = 3.3.0` | `rustls` only; default features disabled | Synchronous HTTPS client for the fixed identity-provider request | MIT OR Apache-2.0 | 1.85 | https://github.com/algesten/ureq |
| `serde = 1.0.228`, `serde_json = 1.0.149` | Workspace serde derive; `serde_json` defaults | Deserialize the bounded response | MIT OR Apache-2.0 | See cached crate metadata | https://github.com/serde-rs/serde ; https://github.com/serde-rs/json |

The cached `ureq` manifest's defaults include both `rustls` and `gzip`; explicitly disabling
default features avoids enabling gzip and selects only its `rustls` feature. The selected local
normal/build tree resolves `ureq-proto 0.6.0`, `rustls 0.23.37`, `ring 0.17.14`,
`rustls-webpki 0.103.9`, `rustls-pki-types 1.14.0`, and `webpki-roots 1.0.9`. In this
selected graph there is no `aws-lc-rs`, native TLS, platform verifier, SOCKS or async runtime.
This is resolver evidence only: ring's native C/assembly build path still requires native build
and supported-platform review. The cached manifest declares ureq MSRV 1.85, matching the
workspace declaration, but no MSRV compilation was performed.

The locked dependency fetch completed successfully; the subsequent
`cargo tree --manifest-path V:/server-connections-mvp/Cargo.toml -p vesta-control -e normal,build --offline --locked`
inspection also completed successfully. A separate offline fetch attempt had failed because
`webpki-roots 1.0.9` was not yet cached; the locked fetch made the missing package available.
These are package-availability and selected-graph observations, not build or runtime evidence. A
coordinator-reported `CARGO_INCREMENTAL=0 cargo check --manifest-path
V:/server-connections-mvp/Cargo.toml --workspace --all-targets --offline --locked` also completed
successfully on native Windows with Rust 1.96.0, compiling the selected native ring dependencies.
That check predates the Minecraft HTTPS source/tests and is not HTTPS behavior evidence, a test
result, MSRV execution or cross-platform certification.

The implemented private adapter is restricted to the fixed HTTPS host, disables redirects and
ambient proxy configuration, and uses 64 KiB complete-body / 16 KiB header limits with at most two
active verifications process-wide. A private synchronous fixed-host resolver retains the permit
until actual DNS/provider return or unwinding, avoiding the default resolver's detached DNS-timeout
worker. OS DNS may still block or overrun nominal timeouts: no hard DNS cancellation or end-to-end
duration guarantee is claimed. The exact `unversioned` resolver integration is tied to ureq 3.3.0.

Later fresh native Windows 11 / Rust 1.96.0 format/check/workspace-test/strict-Clippy commands all
passed after source review corrections: **279 Rust tests**, zero failed/ignored. In-memory actual
ureq::Body fixtures cover the shared production reader's exact 64 KiB acceptance and overflow
rejection; controlled provider fixtures cover owned work/admission/cleanup, while configuration
assertions inspect the private production agent without sending a request. These do **not** establish
real HTTPS/TLS/certificate/DNS/Mojang behavior. No local HTTP/TLS server or real Mojang request ran.
See [the Minecraft checkpoint](RUST_MINECRAFT_IDENTITY_CHECKPOINT.md) for actual scope and failures.

Exact ureq 3.3.0 dependency Trace logging exposes username/serverId queries and Location even though
module errors/Debug are redacted. Full transport-log redaction remains a runtime-composition/upstream
gap, not a verified property. No cached/vendored dependency, global logger or security setting was
modified. Native Windows ring compilation is established; non-Windows/native packaging and Rust
1.85 execution remain pending.

Release intake remains open: inspect and verify the complete locked graph's provenance, licenses,
notices and vulnerabilities; produce the full SBOM; assess ring's native build and supported
platforms; and execute MSRV and integration checks before any release claim. No trusted provenance,
complete license/notice review, vulnerability status, platform certification or production
behavior is established here.

## Extracted server-installer intake (2026-10-08)

The unpublished `vesta-server-install` package reuses the existing Rust installer, not a new
HTTP/execution subsystem. Its direct versions match the companion installer selections:

| Direct pin | Purpose / retained features | Cached manifest licence | Declared Rust minimum |
| --- | --- | --- | --- |
| `tokio = 1.50.0` | Existing async/process/filesystem/I/O, `full` | MIT | 1.71 |
| `reqwest = 0.11.27` | Existing bounded transport, `json/stream/multipart/socks`, default TLS retained | MIT OR Apache-2.0 | 1.63.0 |
| `futures = 0.3.32` | Existing async traits/utilities | MIT OR Apache-2.0 | 1.71 |
| `thiserror = 2.0.18` | Fixed domain errors | MIT OR Apache-2.0 | 1.68 |
| `sha1 = 0.10.6`, `sha2 = 0.10.9`, `md-5 = 0.10.6` | Original pins plus separately observed SHA-256; SHA-1/MD5 never authenticity | MIT OR Apache-2.0 | Not declared in these cached manifests |
| `sysinfo = 0.38.4` | Existing available-space checks | MIT | **1.88** |
| `dunce = 1.0.5` | Existing canonical-path normalization | CC0-1.0 OR MIT-0 OR Apache-2.0 | Not declared |
| `url = 2.5.8` | Existing URL policy/parser | MIT OR Apache-2.0 | 1.63 |
| `wiremock = 0.6.5` (tests only) | Owned local HTTP fixtures | MIT/Apache-2.0 (cached spelling) | Not declared |

Existing workspace exact pins for serde/serde_json/SHA-256/UUID/windows-sys/tempfile are reused;
serde additionally enables `rc`. No root direct pin was changed. The crate keeps edition 2021 and
declares **Rust 1.88**, honestly matching sysinfo; foundation declarations remain 1.85. Neither
minimum was executed. Windows/Rust 1.96 is the current evidence, not an MSRV certification.

Normal offline resolution expands the server lock from **106 to 247** package entries. All newly
selected registry entries were compared by name/version/source/checksum with the companion lock
and match. No retained registry entry changed checksum. Seven older server entries were replaced
or removed; do not describe the expanded lock as version-neutral:

- `futures-util 0.3.34` -> `0.3.32`.
- `js-sys 0.3.106` -> `0.3.91`.
- `wasm-bindgen`, `wasm-bindgen-macro`, `wasm-bindgen-macro-support`, `wasm-bindgen-shared`:
  `0.2.129` -> `0.2.114`.
- `syn 3.0.6` removed; the existing `syn 2.0.119` remains, not a repin of that package.

Historical server lock edges connect these to UUID's target-WASM js-sys/wasm-bindgen graph;
newer macro-support selects syn 3, older support syn 2. The cached UUID/reqwest manifests scope
those JS/wasm dependencies to WASM. This is not a blanket native Windows foundation repin.
However, futures-util is also used by the newly admitted native reqwest/futures graph: do not call
that package universally WASM-only. The resolver's internal selection cause was not established;
the chosen graph intentionally matches the original companion installer, rather than silently
claiming the two prior graphs were identical. Both whole-project regressions must pass with it.
The companion lock adds only the new path-package stanza and Piston dependency edge; no existing
registry version/source/checksum change is required there.

The package declares `GPL-3.0-only`, matching the actual repository LICENSE files. The companion
workspace's MIT metadata conflicts with its GPL root license and was not inherited or treated as
permission to relicense. No publication, full licence compatibility/notice review or distribution
arrangement is approved by this extraction.

Reqwest's existing native/default TLS dependencies (including target-dependent native TLS,
OpenSSL/SChannel/Security Framework and SOCKS support) are retained, not replaced by the separate
Minecraft identity adapter's Rustls stack. sysinfo and windows-sys retain platform-specific native
code. Function-scoped native allowances cover the unchanged read-only volume/file-handle calls;
they do not certify native isolation, process-tree containment or path-race resistance. No cached
crate was modified. Full SBOM, trusted provenance/vulnerability/notice review, clean-platform
packaging, supported service accounts, MSRV execution, CI sibling-checkout wiring and release
approval remain open. See [the extraction checkpoint](RUST_INSTALLER_EXTRACTION_CHECKPOINT.md).

## Trusted launch TOML intake (2026-10-09)

The existing `vesta-game-runtime` library adds exact workspace pin `toml = "=0.9.12"`,
resolved as **0.9.12+spec-1.1.0**, and the existing `serde = 1.0.228` derive dependency.
TOML defaults are disabled; only `std`, `parse` and `serde` are enabled. This parses a
bounded, launch-only trusted local document into a private validated Rust plan. It is not
an authenticated remote configuration API or a full `WrapperConfig` replacement.

- Cached TOML manifest: **MIT OR Apache-2.0**, declared Rust minimum **1.76**, upstream
  `https://github.com/toml-rs/toml`. This is metadata intake, not notice/provenance certification.
- The inspected version is deliberately pinned; it is not a claim to use the latest release.
  Cargo reported a newer `1.1.8+spec-1.1.0` as available at resolution.
- The lock adds exactly seven package entries without repinning existing packages:
  `serde_spanned 1.1.2`, `toml 0.9.12+spec-1.1.0`,
  `toml_datetime 0.7.5+spec-1.1.0`, `toml_parser 1.1.5+spec-1.1.0`,
  `toml_writer 1.1.3+spec-1.1.0`, `winnow 0.7.15`, and `winnow 1.0.4`.
- The inspected native normal runtime graph selects `serde_core`, `serde_spanned`,
  `toml_datetime`, `toml_parser`, and `winnow 0.7.15`; `toml_parser` also selects
  `winnow 1.0.4`. Lock-resolved `toml_writer` and TOML's `indexmap` edge were not
  selected in that normal graph. A lock entry is not proof that a package runs.
- The cached parser has recursion limit **80** with `unbounded` disabled. The adapter
  independently caps UTF-8 input at **65,536 bytes**, rejects unknown/cross-kind fields,
  bounds resolved paths and assembled argv, and discards raw parser diagnostics. A
  200-level nested-array negative fixture refuses with a fixed input-free error.
- Alternatives rejected: handwritten TOML, importing Java configuration classes, silently
  accepting the whole legacy document, permissive missing-config defaults, and using
  caller-selected config to manufacture authority or launch through a shell/PATH search.

Locked resolution and initial all-target compilation succeeded, followed by actual Windows
adapter/native-fixture tests. Exact commands and failed-attempt history are in
[RUST_LAUNCH_CONFIGURATION_CHECKPOINT.md](RUST_LAUNCH_CONFIGURATION_CHECKPOINT.md).
No real Java/Minecraft or argfile execution was used. The adapter introduces no native
library, service or network listener. Rust 1.96 Windows execution is not Rust 1.76, workspace
1.85/installer 1.88 MSRV evidence. Full transitive licence/notice review, SBOM, trusted
provenance/vulnerability checks, MSRV and other-platform execution, packaging and signed
release approval remain open.

## Protected file observations and validated pack reconciliation (2026-10-09)

The file-observation service adds only direct target-specific `vesta-control` edges to
already-pinned `libc = 0.2.183` on Unix and `windows-sys = 0.61.2` on Windows. The lock
diff adds only those two dependency names to the existing control-package entry; no
package/version/source/checksum or feature was added, repinned or fetched. The native
Windows adapter uses existing FileSystem features for no-reparse opens, disk/file/link
inspection and actual held-file paths. Unix uses existing libc no-follow/nonblocking/
cloexec flags; it has not been executed on the current Windows host. Narrow unsafe
allowance is isolated to the held-handle Windows adapter. This is native observation,
not a sandbox, handle-relative directory confinement or file-mutation authority.

Pure pack validation/reconciliation reuses existing `serde`, `serde_json`, `sha2` and
`hex`; no regex, archive or networking dependency is introduced. Strict Rust-native
catalogue schemas do not replace the retained Java/index wire schema. Opaque archive
metadata authorizes no download/extraction/ZIP construction. The current slice has no
file writes/deletes or network/process effects. [The scoped checkpoint](RUST_FILE_PACK_CHECKPOINT.md)
records actual tests, review fixes, alias/ancestor race limits and remaining behavior.
Licence/provenance/SBOM/notice, non-Windows/MSRV, service-account and release gates remain
open; existing package intake is not refreshed into a new release certification.
