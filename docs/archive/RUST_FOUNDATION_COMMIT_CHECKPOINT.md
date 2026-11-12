# Rust foundations — local unsigned checkpoint

Date: **2026-10-08**. This is a development source checkpoint, not a completed migration,
release, security certification or public-deployment approval.

The owner explicitly authorized continuing implementation and making **local unsigned commits**
as verified chunks are ready. The commit command uses a per-command `commit.gpgsign=false`
override, not a repository/global signing change or PIN request. Push, PR creation, publication,
release and signing/security-setting changes remain unauthorized.

## Included behavioral foundations

The Cargo workspace coexists with the retained Java/Gradle/Minecraft plugin/mod tree:

- `vesta-protocol`: bounded discovery and pack-description contracts; descriptive support is not authority.
- `vesta-state`: protected state directory, typed SQLite units of work, migrations/verified backups,
  audit/events/settings, writer/lifecycle ownership and the isolated Windows ACL adapter.
- `vesta-control`: current canonical policy and identity commands, conservative ASCII scopes,
  bounded Argon2id/pack-password lifecycle and one-time Minecraft challenge/hasJoined observation.
- Frozen comparison fixtures/oracles, checked parity inventory, dependency notes and dated evidence.
- Prospective CI while retaining Java checks; CI definitions are not executed Linux evidence.

See the [state/protocol](RUST_STATE_PROTOCOL_CHECKPOINT.md),
[identity](../operations/identity-api.md), [password/state](RUST_PASSWORD_STATE_CHECKPOINT.md)
and [Minecraft](RUST_MINECRAFT_IDENTITY_CHECKPOINT.md) records for exact scope, replacements,
scoped reviews, historical failures and pending work. The [operations kernel plan](RUST_OPERATIONS_KERNEL_PLAN.md)
is design only in this foundations checkpoint; there is no implemented operations runner or execution handler.

## Fresh pre-commit verification

Native **Windows 11 / Rust and Cargo 1.96.0**, 2026-10-08:

```sh
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo check --workspace --all-targets --offline --locked
CARGO_INCREMENTAL=0 cargo test --workspace --offline --locked
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --offline --locked -- -D warnings
node --test scripts/requirements-ledger.test.mjs scripts/server-feature-protocol.test.mjs scripts/rust-wrapper-parity.test.mjs
node scripts/rust-wrapper-parity.mjs
node scripts/requirements-ledger.mjs --check
git diff --check
```

All passed: **279 Rust tests**, zero failed/ignored; **159 Node tests**, zero failed/cancelled/skipped.
The parity checker preserves **222 Java file hashes / 370 frozen requirements** and the ledger
confirms **370 baseline bullets**. Inventory: **24 migrated, 17 retained Java-plugin, 181 not migrated,
0 verified**. This is structural and bounded execution evidence, not complete requirement parity.

The protected companion launcher `vesta-launcher/src-tauri/Cargo.toml` is not staged or edited.
Its SHA-256 is unchanged:
`4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`.

## Actual local commit outcome

The inspected 93-file foundations scope was committed successfully as
`33ffb729a618a0f80e4b94b2d2c784fd59e57193`, using the per-command unsigned override.
Git reported signature status `N`; inspection of the commit object confirmed no signature header.
The server working tree was clean immediately afterward and the protected launcher hash was
rechecked unchanged. No push or signing-configuration change occurred. The subsequently started
operations increment is not implementation evidence contained in this foundations commit.

## Still pending

Durable operations/confirmation/runner/replay; game/runtime/supervisor/IPC; protected files/content/full
packs; installer extraction/integration; agent executable/onboarding; real OS-local authority;
authenticated devices/claims/mTLS/sessions/vault and remote handlers. The Java plugin/mod remains
intentionally retained. Unknown/unsafe legacy trust routes must receive safe replacements or explicit
dispositions, not compatibility bypasses. See [the migration plan](RUST_WRAPPER_MIGRATION_PLAN.md).

Dependency Trace query redaction, hard DNS cancellation, real Mojang/TLS/DNS/certificate evidence,
stronger native isolation, declared Rust 1.85 execution, non-Windows platforms/architectures and
release dependency/packaging intake remain open. The mandatory final **whole-rebuild independent
Sol review is not_run**; earlier scoped reviews do not satisfy it.

No real provider artifact/installer/Minecraft execution, EULA acceptance, world/demo erasure,
service/firewall/router/public exposure, automatic world restore, push or publication is part of this checkpoint.
