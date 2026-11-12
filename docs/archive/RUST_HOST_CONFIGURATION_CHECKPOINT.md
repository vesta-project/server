# Rust-native trusted host descriptions and bounded admission

Date: **2026-10-09**. Status: **configuration/player/CLI/admission/framing implementation slice Windows-verified; whole migration unfinished**.

This follows the reviewed local file/pack checkpoint `f8a70c8`. It adds real configuration loading and a passive executable, not a functioning remote management host. Java wrapper/core/plugin sources and frozen original requirements remain available. Configuration possession, socket IP, player output and pack-access preferences grant no administration.

## Strict backend-owned configuration

`vesta_control::config::HostConfiguration` owns private validated descriptions. `parse_toml(bytes, absolute_base, source)` is pure. `load_file(source, absolute_base)` uses the shared crate-private protected local-document reader. `validate_host_paths()` observes state separation from **both** the game and every scan root through the existing `StateDirectory` constructor, without preparing or creating state.

The separate Rust-native TOML schema requires `schema-version = 1` and all four tables:

```toml
schema-version = 1
[paths]
state = "private-state"
game = "game"
scan-roots = ["game/public"]
launch-definition = "private/launch.toml"
[pack]
name = "Example pack"
version = "1.0"
revision = 1
minimum-revision = 1
minecraft-version = "1.21.1"
[pack-access]
mode = "minecraft"
[listener]
mode = "disabled"
```

Optional pack description and loader kind/version are descriptive. The password alternative is explicitly `minecraft-or-password`; it supplies no password secret or identity/role grant. TLS mode requires a literal bind IP, nonzero port, certificate/key paths and explicit bounded connection/request/timeout limits. A TLS description neither loads certificates nor binds a socket. There is no plaintext/open/automatic-certificate fallback.

Map-only private serde boundaries refuse positional-array alternatives for every table, including optional loader. Unknown, duplicate, missing and wrong-typed fields refuse. There are no server-ID, owner, role, OP, executable, argv, provider URL or plaintext password authority fields. The persisted canonical database server ID remains authoritative.

Bounds: **65,536 document bytes**, **1..8 scan roots**, **4,096 bytes/resolved path**, **255 bytes/component**, **32,768 aggregate resolved path bytes**, **256 bytes/metadata label**, **2,048 bytes/description**, positive ordered revisions through **2^53-1**, and TLS limits through **1,024 connections / 1 MiB request / 300 seconds**. Revisions are not silently clamped. TOML path spellings use forward slashes, refuse traversal/empty/dot components, ADS/device names and ambiguous trailing dot/space spellings. Paths resolve against the explicit trusted absolute base, not inferred PATH or frontend input. Native source/base paths follow current-platform syntax.

Source, launch document and TLS certificate/private-key paths are mandatory host file/pack exclusions, including while absent. Public game/scan roots may intentionally overlap; private state must not overlap them in either direction. This is observing path validation, not a lease against subsequent same-account alias replacement or proof that launch/TLS/runtime components are ready.

The local reader reuses the existing native no-follow/plain-ancestor/regular-file/hard-link/held-file adapter. It bounds initial metadata and growth while reading. It deliberately can load browser-protected config names; it is crate-private and has no remote scope or authority. Residual ancestor check-then-use races and stalled native syscall deadlines remain unresolved, as in the file-observation checkpoint.

## Thin passive executable

`apps/vesta-agent` is the sixth non-publishable workspace member. Its only substantive command is:

```sh
vesta-agent check-config <path>
```

It incrementally reads native arguments, validates the source and observed root separation, then prints `configuration_valid; runtime_and_transport_not_started`. Invalid configuration/native observations use exact fixed `host_config_*` codes, without path/input/parser/secret diagnostics. Unsupported `run`/`bootstrap-owner`, missing or extra arguments refuse with fixed usage. `--help`/`-h` are passive.

This command does **not** prepare/open a database, generate an owner or credentials, read/compile the separate launch document, start Minecraft, accept EULA, bind a listener or modify game/state data. CLI fixtures assert retained bytes, absent state/launch/EULA files and redacted failures. Full host startup/composition remains unfinished.

## Bounded player-output observations

`vesta_game_runtime::players::parse_player_list_line` parses one bounded vanilla or slash-form console line, with a genuinely bracketed log prefix and optional descriptive UUID suffix. Bounds are **8,192 input bytes / 1,024 listed tokens / 64 name bytes**. Checked decimal counts refuse overflow rather than inventing zero. Real legacy section codes, fixed-width `§x` plus six `§<hex>` pairs and complete ANSI SGR are supported; malformed/unsupported escapes refuse. Remaining control scalars refuse before trimming, so CR/LF/tabs cannot turn a multiline block into one observation. UTF-8 boundary handling is panic-free and names/debug errors are redacted.

Empty interior/trailing tokens and contradictory visible counts refuse. A server may hide some names, and advertised capacity is not an entitlement/readiness invariant. Bare UUIDs do not invent player names. This replaces Java's fabricated count repairs instead of copying them.

There is still no fixed `list` issuance, current console-read authorization, exact-generation query correlation, console horizon/gap handling or timeout/exit result. A parser does not complete `PlayersQuery`, establish readiness or prove Microsoft/Minecraft identity.

## Bounded monotonic peer admission

`vesta_control::admission` supplies a single-owner `PeerRateLimiter` and private validated `RatePolicy`, with no new dependency, listener, worker or authentication. The backend must supply the actual observed socket IP, not a client header/claim, and retain one limiter for the host lifetime.

Integer nanosecond credit preserves continuous fractional refill without floating-point rounding. IPv4-mapped IPv6 shares the IPv4 budget. Policy caps are **1,000,000 burst / 1 ms..1 hour window / 4,096 tracked peers / positive idle duration through 24 hours**. Saturation refuses new peers rather than evicting an active/depleted budget. Synchronous bounded cleanup requires both idle expiry and a fully refilled bucket; denied attempts count as activity. Backward clock observations fail closed without budget mutation. Debug reveals count, not addresses.

Admission grants no pack/session or administrative capability. There is no route integration, distributed/global rate guarantee, active worker/connection budget, durable rate persistence or transport completion claim.

## Transport-neutral framing foundation

`vesta_protocol::framing` provides a private validated `FrameLimit`, redacted owned UTF-8 `FrameBody`, and stateless `read_frame`/`write_frame` over caller-owned Rust `Read`/`Write`. The four-byte big-endian length counts bytes. Positive body limits are chosen explicitly through **16 MiB**, with a separate **1 MiB** control-channel constant. Incoming lengths refuse before body allocation/read; outgoing lengths refuse before writing a prefix. Strict UTF-8 replaces Java's replacement-character decoding.

Only EOF before the first prefix byte is a clean `None`; partial prefixes and bodies have distinct fixed errors. Interrupted IO retries; other IO errors erase diagnostics. Any error requires discarding the stream, including partial writes. Standard `Write::write_all` handles partial output without recursive stack growth, and flushing belongs to the caller. Per-frame memory bounds do not supply a global connection/queue budget or stalled-IO deadline.

The parent checked the actual Java source rather than trusting an inventory claim: TCP `readExactly` returns null for any short prefix; Control `readInt` propagates EOF even before the prefix. Rust's clean-close/truncated-prefix distinction is a deliberate safer replacement. TCP passes zero-length bodies to JSON parsing while Control rejects zero; this layer refuses zero uniformly.

This is **not** the full `TcpMessageCodec` or `ControlCodec`: no JSON DTO/type registry, routes, sequence/session enforcement, TLS/IPC, socket/thread ownership or authentication is introduced. Retained plugin wire consumers and safe host adapters still require explicit accounting; a successfully decoded frame confers no authority.

## Verification and failed-attempt history

The first integrated six-member workspace run passed **811 Rust tests across 41 result summaries**, zero failed/ignored, with workspace format/all-target check/strict Clippy. After the parent added five scoped-review regressions, fresh settled-source verification passed **816 Rust tests across 41 result summaries**, zero failed/ignored, plus workspace format/all-target check/strict Clippy. Actual output includes all five new regression names. Earlier checkpoint and helper package counts are not substituted for this final-source run.

Commands executed on native **Windows 11 / Rust 1.96.0** (not declared MSRV or non-Windows evidence):

```sh
cargo fmt --manifest-path V:/server-connections-mvp/Cargo.toml --all -- --check
CARGO_INCREMENTAL=0 cargo check --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --all-targets --offline --locked
CARGO_INCREMENTAL=0 cargo test --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --offline --locked -- --test-threads=1
CARGO_INCREMENTAL=0 cargo clippy --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --all-targets --offline --locked -- -D warnings
node --test V:/server-connections-mvp/scripts/requirements-ledger.test.mjs V:/server-connections-mvp/scripts/rust-wrapper-parity.test.mjs V:/server-connections-mvp/scripts/server-feature-protocol.test.mjs
node V:/server-connections-mvp/scripts/rust-wrapper-parity.mjs --check
node V:/server-connections-mvp/scripts/requirements-ledger.mjs --check
```

Fresh Node verification passed **159 tests**, zero failed/skipped, and both accounting checkers. Companion `piston-lib` all-target check and serial offline/locked tests passed **210 tests**, zero failed/ignored; the existing unused `tokio::task` warning remains, and companion strict Clippy was not claimed. Java/reference/frozen ledger/inventory diff checks passed. The protected launcher manifest retained SHA-256 `4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`. No real Java/game/provider/installer execution, EULA acceptance, world/demo erasure, listener/public exposure, service/firewall/router/signing changes or push/publication occurred.

Scoped independent Sol review inspected admission, protected local reading, the passive CLI, player parsing and framing without editing source or substituting review-time test claims. It found no verified new defect and recommended coverage for dropped UUID token accounting, absolute-source/CWD anchoring, extreme rate arithmetic/backward cleanup and nonblocking/body IO failures. The parent added those assertions. This is not the required final independent whole-rebuild review.

Executed history so far:

- Parent initial CLI all-target dependency check passed, then CLI tests were **3 passed / 1 failed** because the parent fixture expected `config_*` instead of the actual `host_config_*` codes. The fixture now asserts the exact invalid/missing codes; production failure handling was not loosened.
- Parent initial actual player subset was **24 passed / 3 failed**. Independent fixtures were corrected for the exact entry count/trailing comma, unknown versus malformed grammar, and real per-code section formatting. No parser grammar was broadened to make a bad fixture pass.
- Earlier Bunny standalone generated-copy/rustc surrogate results were rejected, not counted as verification. Later corrections ran only in the actual workspace.
- A later Bunny bracket-prefix fixture uniformly expected one player while containing a two-player case. It was replaced with explicit per-case golden counts/names, without changing source semantics.
- Sol's first config test compilation exposed an oversized numeric fixture inferred as `i32`; its literal type was corrected to `u64` without weakening production revision bounds.
- Parent source review found a recursive framing partial-write routine that could grow the stack per byte. It was replaced with standard `Write::write_all`, with an actual 64 KiB one-byte writer regression. Parent also corrected invalid fixture evidence: overflow-prone chunk arithmetic, a poison guard that allowed body-contact attempts, skipped failure/interrupt offsets, mismatched UTF-8/debug lengths and a missing generic `Debug` bound. None of the initial uncompiled framing claims counts as verification.
- The first actual corrected framing compilation failed with two `E0502` test reborrow errors. Hoisting the allowed slice length before the mutable borrow fixed them; the subsequent actual protocol package run passed.
- Strict Clippy exposed a config Windows match-style issue and the parent's nested admission condition. They were simplified without changing acceptance/authority or disabling lint.

## Accounting and remaining work

This is not a blanket promotion of `ConfigLoader`, `WrapperConfig`, `AgentRuntime`, `VestaWrapper`, `PlayersQuery`, `TokenBucketRateLimiter` or transport families. Full assertion-by-assertion dispositions, applicable original requirements and final independent **whole-rebuild** Sol review remain required. Frozen Java hashes, all 370 original requirement IDs/text/status/history and broad platform/product/release statuses remain unchanged.

Remaining useful-current-Java work includes durable file/content mutation effects, configured catalogue/source-registry/archive/ZIP services, genuine sync/manifest transport adapters, full host/runtime discovery and correlated player query, pack-only authentication/session/TLS/routes and bounded accepted-peer lifecycle, and remaining foundation behavior accounting. Separate unfinished product gates include OS bootstrap/device/mTLS/vault/full remote management, independent supervisor/IPC/crash continuity, process-tree isolation/readiness, durable installer activation/update/adoption/world recovery, managed Java/real-loader matrix, services/distribution and non-Windows/MSRV/release execution.
