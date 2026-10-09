# Rust Minecraft identity challenge increment

Date: **2026-10-08**. Status: **bounded implementation and Windows verification passed; product composition incomplete**.

This follows the [password/state checkpoint](RUST_PASSWORD_STATE_CHECKPOINT.md), not a new
product specification or a Java-class reconstruction. The 222 Java inventory entries and
370 original normative requirements remain frozen. Java source, tests and Gradle builds
remain available for behavior comparison. No whole requirement, platform gate or final
independent-review status is promoted.

## Implemented domain boundary

The cohesive `crates/vesta-control/src/identity/minecraft/` module verifies a one-time online
Minecraft profile against the provider's `hasJoined` result. It has no canonical database
mutation, device possession check, session issuance, role/grant creation, administrative bootstrap
or pack admission policy. A verified profile is observation/evidence, never a `Subject` or
management credential. Production OS/local authority remains unavailable.

The application interface is server-bound:

```rust
MinecraftChallenges::new(ServerUuid) -> Result<Self, MinecraftError>
MinecraftChallenges::issue(MinecraftUuid, MinecraftName) -> Result<IssuedChallenge, MinecraftError>
MinecraftChallenges::complete(&ChallengeId) -> Result<Option<VerifiedMinecraftProfile>, MinecraftError>
```

Production construction privately selects the fixed provider and one process-wide state/admission
boundary. There is no public provider, clock, capacity or lifetime injection. Private test factories
are `cfg(test)` only. Value types have validating entry points and private fields. Profile receipts
have private construction, no `Clone`, no deserializer and no authority conversion. Compile-fail
doctests cover external receipt construction and caller-selected server-hash parsing.

Implemented limits for this bounded increment, not new normative product limits:

- Fixed **60-second** lifetime with monotonic expiry and a post-provider expiry check.
- At most **1,024 pending plus in-flight** challenges and **2** active verifications per process,
  shared across production factories/servers/clones. No expensive-work waiting queue.
- Separate issue and verification buckets: **60-token bursts, one token/second refill**.
  Capacity refusals spend the corresponding token. Fractional refill time is preserved; idle time
  cannot accumulate more than a burst, and backward samples cannot refill.
- Challenge IDs are locally generated **32-byte** OS-random nonces in canonical **64-character
  lowercase hex**. UUID input accepts compact-32 or standard-hyphenated-36 ASCII hex; no nil,
  whitespace trimming, alternate UUID forms or invented version restriction.
- Names are **1..16 ASCII alphanumeric/underscore** bytes. Matching uses `eq_ignore_ascii_case`
  only after validation, paired with exact UUID equality; the provider's canonical name is
  returned. No JDK Unicode/casing/trim emulation.
- Completion removes the entry atomically before server/expiry/provider/admission checks.
  Wrong-server routing, replay, expiry, mismatch, busy/rate refusal and provider failure never
  restore it. Verification does not hold the state mutex. RAII ownership survives until real
  provider return/unwinding, not merely an observer timeout.
- Provider operational errors remain errors even if the failed call crosses the challenge's
  expiry deadline; a successful provider response arriving at/after expiry creates no receipt.

## Provider and genuine wire contract

The only endpoint is
`https://sessionserver.mojang.com/session/minecraft/hasJoined`.
Only a validated stored username and locally generated server hash enter its query. Vesta does
not receive or forward a Minecraft access token. Clients cannot select the host/URL, IP binding,
proxy, TLS roots or headers. The private adapter configures verified Rustls/WebPKI HTTPS, no
ambient proxy, no redirects, no pooled-socket retries/cookies/compression, **16 KiB headers** and
**64 KiB complete body**. Nominal global/connect/body timeouts are 10/5/5 seconds.

Provider identity fields must be unique strings in a root-object UTF-8 JSON document. Root fields
are capped at 32, decoded key bytes at 128, nesting at 32, and trailing documents are rejected.
Provider UUIDs must be compact non-nil 32-character ASCII hex. Bounded unknown properties are
accepted as inert data, not credentials or grants. Non-200/204 statuses are fixed operational
errors; 204 is no joined profile. These statuses do not read a body. Unsupported content encoding
is refused. The ureq reader allows one overflow byte so an exact 64 KiB body can reach the EOF
probe; the shared decoder still rejects every complete body over 64 KiB.

The `serverId` is a 40-character lowercase hex value:

```text
lowerhex(SHA256(b"vesta-minecraft-session-v1\0" || server_uuid_bytes || nonce)[0..20])
```

The retained Java comment explains that join/hasJoined need the same unpredictable value at most
40 hex characters, not a specific SHA-1 computation. This uses existing maintained SHA-256/hex
primitives, not a new SHA-1 dependency, legacy digest equivalence or Minecraft game-handshake
implementation. **Live Mojang acceptance is not established** by that source interpretation.

The reviewed exact `ureq = 3.3.0` default resolver can spawn timed DNS workers that outlive a
request. The private supported resolver instead resolves only the fixed host synchronously,
retaining verification ownership while checking the supplied timeout before/after resolution.
**OS DNS can still overrun the nominal timeout or hang. No hard DNS cancellation or hard
wall-clock completion bound is claimed.** Saturated/stuck work occupies the fixed slots rather
than allowing detached lookups to accumulate. Its `unversioned` resolver interface is coupled
to the reviewed exact dependency version. See [dependency intake](../DEPENDENCY_DECISIONS.md).

**Dependency logging gap:** module-owned errors and `Debug` output redact identities, challenge
IDs, hashes and native causes. However, exact ureq 3.3.0 exposes the username/serverId query and
Location under dependency **Trace** logging. End-to-end transport-log redaction is not established.
Future runtime logging composition/upstream correction remains required; no global logger,
security setting, vendored/cached dependency or signing configuration was modified to mask this.

## Review corrections and actual verification

Independent **GPT-6.1 Sol scoped source/test review** found two production defects:

1. A provider error arriving after expiry was converted into expired `None`. Error propagation
   now precedes the post-provider expiry check; a controlled late-error regression covers it.
2. The original ureq reader cap rejected a valid exact-64-KiB body at its EOF probe, while a
   Cursor-only decoder fixture passed. The shared production `decode_http_response` now uses
   64 KiB + 1 and its actual `ureq::Body` regression requires exact-bound success and overflow
   rejection.

Parent also fixed test closure lifetime inference, matching-name cleanup fixtures, and three
strict-Clippy test idioms without suppressions. Review-driven assertions require **two simultaneous
replacement verifications** after one error and one panic, followed by a third Busy refusal;
one success would not prove both slots were released. Actual production factory/clone Arc sharing
is inspected by a test that never calls the real provider. Blocked fixtures require explicit
release, propagate timeout failure, and measure store progress before provider return. Decoder
tests pair valid-root depth 32/33, field 32/33 and key-byte 128/129 boundaries, duplicate unknown
keys, ordinary nested Mojang properties, strict wire identity negatives and no-source fixed errors.

An independent follow-up initially failed with Sol HTTP 429; no fallback was substituted. Subsequent
same-context scoped reviews completed, verified the two fixes and the strengthened assertions,
and found no additional confirmed production defect. This is **not** the mandatory final
whole-rebuild independent review, whose status remains **`not_run`**.

Historical failed attempts are retained as failures:

- First integrated sequence stopped at test compilation (closure reference lifetime inference).
- Corrected sequence passed **276 Rust tests**, then strict Clippy failed on three test warnings.

Fresh final sequence on **native Windows 11 / Rust and Cargo 1.96.0**, 2026-10-08:

```sh
cargo fmt --all
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo check --workspace --all-targets --offline --locked
CARGO_INCREMENTAL=0 cargo test --workspace --offline --locked
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --offline --locked -- -D warnings
```

All five commands passed. **279 Rust tests passed, zero failed/ignored**: 186 control unit tests
(including 40 Minecraft tests), 3 control integration tests, 29 protocol integration tests,
1 state unit test, 56 state integration tests and 4 compile-fail doctests. This is controlled
provider/reader/configuration and real retained SQLite/native regression evidence, **not real
HTTP/TLS/DNS/certificate or Mojang execution**. No local HTTP/TLS fixture server or real provider
request was run. Java was retained, not rerun for this bounded non-byte-equivalent increment.

The accounting follow-up maps only the challenge service, provider adapter and six-family Java
challenge test reference into the cohesive Rust module. They remain **migrated, not verified**,
with explicit gaps. The legacy HTTP bearer route and optional in-game linking are not mapped.
The original requirement statuses/digests and all 222 Java byte hashes remain unchanged.
Stale schema/identity inventory gaps that still said the later password services did not exist
were corrected to distinguish the historical schema-only evidence from the subsequent bounded
password implementation; no additional status or requirement was promoted.

Accounting follow-up commands also passed:

```sh
node --test scripts/requirements-ledger.test.mjs scripts/server-feature-protocol.test.mjs scripts/rust-wrapper-parity.test.mjs
node scripts/rust-wrapper-parity.mjs
node scripts/requirements-ledger.mjs --check
git diff --check
```

**159 Node tests passed**, zero failed/cancelled/skipped. The checker confirmed **222 Java files /
370 frozen requirements** and the ledger confirmed **370 baseline bullets**. Current inventory:
**24 migrated, 17 retained Java plugin, 181 not migrated, 0 verified**. Checker success validates
structure/hashes, not tests, security or full requirement satisfaction. The protected launcher
`vesta-launcher/src-tauri/Cargo.toml` SHA-256 was rechecked unchanged:
`4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`.

## Remaining product and requirement gaps

`identity-pairing-and-authority-004` additionally requires paired devices;
`unified-principal-model-001` requires attaching claims to the canonical server-local principal.
Those authenticated-device/linking adapters remain absent. Minecraft proof cannot satisfy the local
administration boundary in `identity-pairing-and-authority-006` or turn pack/join access into management
in `pack-access-versus-management-access-001`. Full `minecraft-policy` also needs real whitelist/ban
admission evaluation. Online UUID proof does not invent an offline-player identity and does not enable
Mojang admission policy for an offline-mode server. No in-game linking prerequisite is added.

Collision retry/exhaustion/no-overwrite, RNG/allocation failures and poisoned-store admission have
source inspection only, not deterministic fault execution. Random freshness draws are not collision
branch evidence. The extreme root-array rejection is not evidence for the depth-32 limit; the paired
valid-root boundary fixtures provide that evidence.

Pairing-code rate limiting, endpoint/per-IP/per-principal controls, mTLS/sessions/vault, invitation
composition, canonical remote handlers, durable operations/runner/replay and executable runtime
remain incomplete. Linux/macOS/other architectures, declared Rust 1.85 execution, real transport,
native packaging, provenance/vulnerability/SBOM/notices and final whole-rebuild Sol review remain
unverified. This checkpoint is not the full Microsoft authentication/device flow or release approval.

No commits/push/PR/publication, real provider/game execution, EULA acceptance, world/demo erasure,
service/firewall/router/public exposure, signing/security-setting changes or automatic world restore
occurred. The protected launcher manifest remains outside the edit scope. This is not complete-migration,
secure-product, public-deployment or release-readiness approval.
