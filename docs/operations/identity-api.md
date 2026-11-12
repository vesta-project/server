# Rust identity increment — frozen interfaces and evidence

Date: **2026-10-08**. Supersedes the 2026-10-07 draft contract in place.

This is the executed record of the Rust-native canonical-identity increment: the interfaces now
implemented, the commands and behaviours they admit, the deliberate replacements for Java-shaped
design, and the behaviour that remains an explicit gap. It is not parity evidence for the whole
rebuild, a replacement requirement ledger, a release assessment, or a security certification. The
original **370** requirements and their statuses are unchanged. Java wrapper/core, Gradle builds and
the Minecraft plugin/mod remain available as behavioral references.

`vesta-control` is now an **admitted workspace member** with executed tests. This supersedes the
earlier statement that it was unadmitted and uncompiled.

## Boundary this crate does not cross

`vesta-control` is trusted, in-process policy. It does not authenticate a transport, expose a
listener, enroll a device, own a session or vault, or turn Minecraft identity, a pack password, OP,
connector possession or loopback location into administrative authority. Production local authority
is deliberately unavailable: `IdentityService::new` always selects `UnavailableAuthority`, and no
production verifier-injection seam, privileged boolean, proof deserializer or Cargo test-support
feature exists. In-crate `#[cfg(test)]` fixtures do inject a `TestAuthority`; that test-only seam does
not exist in production.

## Transaction and audit contract

Every canonical-identity mutation owns **one** immediate transaction obtained from `StateDatabase::write_with`:

- policy resolution, mutation and the **success** audit share that transaction, so an audit
  append failure rolls the authority change back rather than reporting an unrecorded mutation;
- a returned domain `Err` rolls back. There is no `Ok(Err(...))` sentinel bridge and no
  `transaction.rs` shim;
- a **clean** domain rejection rolls back first and is only then recorded, in a separate
  transaction, as a refusal with outcome `denied` for `forbidden` and `rejected` otherwise, and
  `reason` as its only metadata key;
- failure to write that refusal is `ControlError::AuditUnavailable` — a real error, never a
  successful denial;
- `State`/rollback/nested-call failures describe infrastructure, not a decision, so they produce no
  refusal record;
- `Repository` never opens, commits or rolls back a transaction; it borrows the caller's connection.
  There is no user-controlled commit/rollback and no escaped transaction lifetime.

`ControlError` exposes `code()`, `reason()`, `primary()` and `secondary()` so callers can report why
work was refused even when a second failure was layered on top. `Debug` never prints SQL, paths,
codes, nonces, `os_actor` values or proof text.

**Error-code namespaces do not overlap.** Every `vesta_state::StateError` code is prefixed
`state_` (`state_io`, `state_invalid_request`, `state_nested_transaction`, `state_nested_call`,
`state_close_in_callback`, …) and every `IdentityErrorCode` is a bare domain code (`forbidden`,
`not_found`, `conflict`, `invalid_request`, `role_invalid`, `code_invalid`, `bootstrap_closed`,
`local_authority_required`). Before this was fixed, an audit-metadata rejection surfaced as
`invalid_request` and was indistinguishable from a domain `invalid_request`; a caller filtering on
the `state_` prefix would also have misread a nesting failure as a decision. A prefix-based
classification is now safe, and `every_domain_rejection_is_a_typed_code_and_never_an_infrastructure_rollback`
asserts the separation.

## Deliberate divergences from the Java reference

Recorded so they are not mistaken for accidental drift. Each fails closed or is presentation-only:

| Behaviour | Rust | Java reference |
| --- | --- | --- |
| Override target state | must be **active** | only had to **exist** |
| Device key collision | `COLLATE NOCASE`, across servers | case-sensitive |
| Nonce / OS actor bounds | UTF-8 bytes and Unicode scalars | UTF-16 `String.length()` units |
| Bootstrap code input | never trimmed; a different-case Base64URL spelling is non-canonical | trimmed, case-insensitive |
| Refused-redemption audit action | `identity.owner.paired` (same name as success) | `identity.owner.pair` (appears to be a typo) |
| Custom role rank ≥ Administrator | rejected by `CustomRank::new` at construction | relied on a schema `CHECK` |
| Path/scope text | rejected if ambiguous or unrecordable | trimmed, separator-rewritten, cased |

These are also recorded per row in `gaps[]` of [the parity inventory](../RUST_WRAPPER_PARITY.json).

## Typed application commands

Commands name identity effects, never SQL, caller-selected audit text or authority.

```rust
IdentityService::new(StateDatabase) -> control::Result<Self>

// Local authority: the only path to bootstrap, recovery, and a local Owner/Administrator grant.
apply_local(Option<&LocalProof>, LocalCommand) -> Result<LocalOutcome>
LocalCommand::Bootstrap(CodeLifetime)
LocalCommand::Recover { lifetime: CodeLifetime, action: RecoveryAction }
LocalCommand::AssignRole { principal: PrincipalId, role: RoleId, expiry: Expiry }
RecoveryAction::KeepAccess | RecoveryAction::RevokeAdministrativeAccess

// Single-use code redemption. Does NOT prove possession of the device key.
redeem_owner(RedeemOwner) -> Result<OwnerAdded>
RedeemOwner { code: CodeInput, name: DisplayName, device_label: DeviceLabel, fingerprint: Fingerprint }

// Transport-supplied identifiers are lookup keys only.
apply_remote(&Subject, RemoteCommand) -> Result<AuthorityChange>
RemoteCommand::AssignRole  { principal, role, expiry }
RemoteCommand::SetOverride { principal, grant: RoleGrant, effect: Effect, expiry }
RemoteCommand::RevokeAssignment(AssignmentId)
RemoteCommand::RevokeOverride(OverrideId)
RemoteCommand::RevokeDevice(DeviceId)
RemoteCommand::CreateRole(CustomRole)

// Observations, never reusable authority.
owner_exists() -> Result<bool>
subject_record(&PrincipalId) -> Result<Option<SubjectRecord>>
current_policy(Option<&Subject>) -> Result<PolicySnapshot>
sweep_expired() -> Result<usize>
```

`LocalOutcome::Code(BootstrapCode)` exposes the secret **once** through `expose_secret()`; only a
SHA-256 digest is persisted. `BootstrapCode` reports `expires_at_ms()` for persistence precision and
`expires_at() -> SystemTime` for presentation. There is no JDK `Instant` rendering anywhere.

**Validated value types.** `PrincipalId`, `DeviceId`, `RoleId`, `AssignmentId`, `OverrideId`,
`DisplayName`, `DeviceLabel`, `RoleName`, `Fingerprint`, `CodeInput`, `CodeLifetime`, `CustomRank`,
`RoleGrant`, `CustomRole` and `ResourceScope` are all constructed through a validating entry point.
Nothing takes a bare `&str` identifier. Lifetimes and expiries are `Duration`; relative expiry is
resolved **inside** the mutation transaction, with the expiry clock sampled only after transaction
admission, so command construction and queueing time do not consume the requested lifetime.
`CustomRank::new` rejects any rank at or above the Administrator tier, so an invalid-rank custom role
structurally cannot reach the service — a deliberate replacement for Java's reliance on a schema
`CHECK`. Intervals that are negative, zero-length or sub-millisecond are rejected rather than
rounded or truncated.

**No grant-taking policy constructor.** `PolicySnapshot` has private fields, a redacted `Debug`
implementation, and exactly one construction path: the module-private `PolicySnapshot::from_resolved`,
reachable only from the repository after it reads current authority inside the caller's transaction.
Commands never accept a snapshot, so a caller can neither forge nor replay one. `current_policy`
returns an observation for a future handler; it is not an authorization input.

## Authorization behaviour

Preserved from the accepted model and now executed by tests:

- deny wins everywhere, including against Owner;
- `local_only` (`owner.recovery`, `agent.install`), `owner_reserved` (`backups.restore`,
  `recovery.export`) and `admin_reserved` (`credentials.manage`, `security.manage`) require active
  built-in Owner/Administrator membership **independently of numeric rank**; a custom role row
  containing a reserved capability never becomes effective;
- remote delegation needs the exact capability, a strictly lower role **and** target rank, no
  self-target, and full containment of every granted capability and scope inside the delegator;
  Owner and Administrator tiers are local-grant-only;
- Device revocation checks the exact capability first, so an explicit deny wins before any hierarchy
  decision. **Only Owner** bypasses the strictly-lower-rank and no-self-target requirements;
  Administrator is not exempt and, like every non-Owner actor, must be strictly higher rank than the
  target and cannot target themself;
- expiry at the exact deadline stops applying immediately, independently of any sweep;
- `sweep_expired` only records markers and audit evidence, bounded per table, and a failure leaves
  every marker unrecorded so the same expiry is reported once later.

The stable **51** capability IDs and **six** built-in role presets are unchanged and are compared
against the independent constants fixture `protocol/identity-v1/fixtures/capabilities-and-roles.json`.

## Resource scopes — conservative replacement for Java text emulation

The Java-shaped text emulation is **removed**: `identity/java_text.rs`, its missing case-table
module, and the JDK `trim`/`isBlank`/UTF-16 `length`/`toLowerCase`/`Instant` behaviour they carried
are gone. `protocol/text-v1/` and `scripts/JavaTextOracle.java` remain only as historical
comparison material, not production code and not an active JDK-compatibility gate.

Scopes and resources are canonical `/`-separated **portable ASCII** with explicit segment
boundaries:

- accepted: root (`""`), single and nested components, dots/dashes inside a component, mixed case,
  interior spaces;
- rejected, rather than trimmed, cased or rewritten: empty components, `.` and `..`, control
  characters, `<>:"/\|?*`, leading space, trailing space or dot, non-ASCII, components over 255
  bytes, and every Win32 device alias (`CON`, `PRN`, `AUX`, `NUL`, `CLOCK$`, `CONIN$`, `CONOUT$`,
  `COM1`-`COM9`, `LPT1`-`LPT9`, including `con.txt`, `NUL.json` and space-obscured basenames);
- a `Resource` may be up to 4096 bytes; a `ResourceScope` may be up to
  `vesta_state::audit::MAX_METADATA_VALUE_UNITS` (256) bytes of text;
- scope text is `*` (all), an exact path, or a path ending in `/**`; `*` is only ever the whole
  string, and a root path scope is not constructible;
- **allows are case-sensitive, denies are ASCII case-insensitive.** The asymmetry only ever fails
  closed: anything an allow matches, the corresponding deny also matches.

**Why a scope is bounded more tightly than a resource.** A scope is recorded verbatim in the audit
record of every grant that carries it, and `AuditRecord::new` refuses a metadata value over
`MAX_METADATA_VALUE_UNITS`. Without a bound, a legitimately long scope would parse, insert its
override row, and only then fail its own success audit — rolling back with an infrastructure error
for an input the caller was entitled to send. That was a real defect, found in review and fixed: the
bound is applied in `ResourceScope::parse`, where it is a plain typed validation error, and it is
asserted by `an_unrecordable_scope_is_refused_before_any_row_or_audit_is_attempted`. A `Resource` is
not bound this way, because it only names the thing being authorized and is never recorded.

**Unsupported or corrupt stored scopes fail closed.** An unreadable stored *allow* cannot grant and
is dropped; an unreadable stored *deny* is widened to the whole capability, so it blocks rather than
disappears; an unknown stored *capability id* in a deny fails the entire policy resolution closed
(`state_identity_invalid`). No stored row is rewritten and no data is erased. Lexical scope matching
is **not** filesystem confinement against links, junctions or races.

**Recorded gap — Unicode file scopes.** `Resource::parse` rejects any non-ASCII path, so a
legitimate non-ASCII scope such as `世界/ファイル` cannot be represented at all. This is unresolved
required behaviour for the file API, is explicitly not a finished capability, and is asserted as a
gap by `recorded_gap_a_non_ascii_resource_scope_cannot_be_represented_at_all`. This conservative
increment is not the final file API and does not claim full scope equivalence.

## Later bounded password and Minecraft components

[The password/state checkpoint](../archive/RUST_PASSWORD_STATE_CHECKPOINT.md) records the later typed
`PackPasswords` service and bounded Argon2id/process-wide KDF gate. Password matching is only
pack-access evidence and creates no principal/device/grant/session/management authority. KDF work
runs outside database transactions; current policy and expected revisions are rechecked in the
final writer with atomic audit.

[The Minecraft checkpoint](../archive/RUST_MINECRAFT_IDENTITY_CHECKPOINT.md) records the later
`identity::minecraft` one-time online-profile challenge/hasJoined module, private fixed HTTPS
adapter and bounded process-wide admission. A private non-Clone verified-profile receipt is
observation only, with no Subject/device/session/role/grant conversion or canonical persistence.
Authenticated device composition and external-claim linking remain deferred. Controlled provider,
actual body-reader and configuration fixtures are not real Mojang/TLS/DNS evidence; dependency
Trace query redaction and hard DNS cancellation remain gaps. These follow-ups do not promote
original requirements or satisfy the pending final whole-rebuild review.

## Test families

Five behavioural families plus a fixture contract, all in-crate `#[cfg(test)]` unit tests over real
temporary SQLite, real transactions, the real audit chain, a controlled clock and the production
service. Test fixtures inject the `#[cfg(test)]`-only `TestAuthority`; this is not a production
authority seam. The tests use no fake service, audit shim or stubbed production type.

| Module | Tests | Covers |
| --- | ---: | --- |
| `tests/bootstrap.rs` | 22 | Proof purpose/nonce/actor/freshness/lifetime bounds, absent and foreign proof refusal, nonce single use, code single use and rotation, exact-deadline expiry, concurrent redemption of one code, recovery revocation and retry after audit fault, refusal-audit failure |
| `tests/policy.rs` | 22 | Unresolved identities, foreign server id, deny-wins, unreadable stored allow/deny, reserved tiers, primary-role replacement, expiry before sweep, built-in immutability, constants fixture comparison, sweep marker atomicity |
| `tests/scopes.rs` | 21 | Component rules, device aliases, byte bounds, scope form, **segment-boundary matching**, case asymmetry, `covers`/`overlaps`, delegation widening refusal, stored fail-closed, unrecordable-scope regression, recorded Unicode gap |
| `tests/delegation.rs` | 17 | Strictly-lower/no-self/exact-capability delegation, custom role creation and containment, override and revocation rules, device revocation hierarchy, authority reloaded per transaction, snapshot is observation-only |
| `tests/atomicity.rs` | 19 | Audit-fault rollback and retry for every mutation, post-rollback refusal audit, `audit_unavailable` with preserved primary, typed domain vs infrastructure errors, no leaked transaction or secret in any rendering, no nesting |
| `tests/fixture_contract.rs` | 8 | The shared fixture and typed values themselves, so broken scaffolding cannot read as passing coverage |

`tests/support.rs` is shared scaffolding and is **not** executed parity evidence.

Reference: the Java source of these behaviours is
`wrapper/src/main/java/com/vesta/wrapper/identity/` and
`wrapper/src/test/java/com/vesta/wrapper/identity/IdentityAuthorizationTest.java`, which remains the
behavioral reference — not an API blueprint.

## Historical verification (before the 2026-10-08 review fixes)

The following run and counts are retained as historical evidence only. They predate the review fixes
recorded below and are **not** fresh post-fix verification. Later post-fix Windows evidence is
recorded in [the password/state checkpoint](../archive/RUST_PASSWORD_STATE_CHECKPOINT.md); no requirement
or whole-platform status is promoted by either bounded run.

Run from the server repository on **Windows 11**, cargo/rustc **1.96.0**, **2026-10-08**:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
node scripts/rust-wrapper-parity.mjs
node --test scripts/requirements-ledger.test.mjs scripts/server-feature-protocol.test.mjs scripts/rust-wrapper-parity.test.mjs
```

All six passed. **192 tests** passed (190 integration/unit targets plus 2 `compile_fail`
doctests), zero failed, zero ignored. The workspace now has three admitted members:
`vesta-protocol`, `vesta-state` and `vesta-control`, of which `vesta-control` contributes **109**
tests across the six modules above. Node accounting suites: **159 passed**, zero failures.

Each behavioural family was additionally mutation-checked in a throwaway copy of the tree outside
the repository, confirming the assertions are non-vacuous: inclusive-vs-exclusive expiry boundaries,
removed nonce bounds, removed `os_actor` bound, deny-loses-to-allow, swallowed audit-append failure,
skipped refusal audit, a refusal audit failure misreported as a successful denial, marker/audit
split across transactions, and a changed capability id were each caught by at least one test.

## 2026-10-08 review-fix note — later evidence recorded separately

The review follow-up clarifies that relative-expiry time is sampled **after admission to the write
transaction**. Time spent waiting for admission therefore does not consume the requested lifetime;
resolving expiry after admission also keeps the decision and mutation on the transaction's current
clock sample. The corrected device-revocation rule is capability-deny-first, with only Owner exempt
from the lower-rank/no-self checks; Administrator is not exempt. The tests use a `#[cfg(test)]`
`TestAuthority`, while production still selects `UnavailableAuthority` and has no verifier injection
seam. The linked password/state checkpoint records fresh post-fix verification. The 192-test and
159-Node-test figures above remain the pre-fix historical run, not evidence for these corrections.

## Still unfinished and explicitly not claimed

Independent supervisor, protected IPC and crash continuity; OS-authenticated bootstrap and local
authority proof; device enrollment, mTLS, sessions, invitations, vault; canonical remote handlers and
any authenticated transport; durable operations, confirmations, runner and replay; full device-bound
pack grants and runtime password composition; game/runtime, protected files, content and pack publication; the extracted
installer; `apps/vesta-agent`. Pairing codes are **not** rate limited, so
`identity-pairing-and-authority-010` is **not** satisfied. `roles`, `role_capabilities` and
`local_authority_uses` are not server-scoped, so `deployment-and-instance-scope-002` is **not**
satisfied. `PolicyOperationAuthority`-style operation authority is not implemented.

Per-row accounting, honest gap lists and Rust evidence live in
[the parity inventory](../RUST_WRAPPER_PARITY.json). The pre-password identity checkpoint had 222 Java
rows, 14 `migrated`, 17 `retained_java_plugin`, 191 `not_migrated`, and **0 `verified`**; the linked
password/state checkpoint records the later bounded mappings. No original requirement status is
promoted. Linux, macOS and non-Windows execution remain unverified; `rust-version = 1.85` is a
declared floor and not verified MSRV evidence; WSL presence is not platform evidence.

## Review status: final full-rebuild review remains pending

`finalIndependentSolReview.status` is **`not_run`**, and it must stay that way until the actual
full rebuild receives its mandatory independent **GPT-6.1 Sol** review. At the earlier identity
checkpoint Sol was unavailable, and no model was silently substituted. Sol later became available
and reviewed the password/state corrections and strengthened regression sources, recorded in
[the later checkpoint](../archive/RUST_PASSWORD_STATE_CHECKPOINT.md). Those scoped source reviews do not
satisfy the final whole-rebuild gate. No Anthropic-model helper/fallback is permitted for this migration.

What *was* run is an **interim** independent review on a different, non-Anthropic model, recorded
as evidence above and in the parity manifest. It read all listed production and test files and the
Java reference, re-ran the suites, and found:

- **no authority bypass** — `Subject` is lookup-only, no command accepts a `PolicySnapshot`,
  reserved tiers need live built-in membership independent of rank, corrupt and unknown stored rows
  fail closed;
- **no audit/mutation atomicity break** — the success audit shares the mutation transaction,
  refusals are separate post-rollback transactions surfaced as errors, and nothing nests a
  database call;
- **one medium finding**, now fixed and regression-tested: a scope longer than one audit metadata
  value could insert its row and then fail its own success audit;
- **documentation fixes**: an overstated claim about `from_resolved` reachability was corrected,
  and the seven Java divergences above were recorded in `gaps[]`.

Those interim/scoped reviews are not final sign-off: **the rebuild is not complete.** Once the
approved rebuild is actually implemented, independently review its entire Rust source, behavioral
inventory, original requirements and security boundaries with GPT-6.1 Sol; verify findings with
regressions and fresh full suites. Only that completed final review can set
`finalIndependentSolReview.status` to `passed`.
