# Testing the Rust server agent

With Rust 1.96.0 and Node 22 or later, run from this repository:

```powershell
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
node --test scripts/*.test.mjs
node --test scripts/bootstrap/*.test.mjs
node scripts/requirements-ledger.mjs --check
node scripts/build-roadmap-html.mjs --check
```

Rust compilation and ordinary tests need no JDK, Gradle, or Java repository.
Protocol fixtures remain checked in here. CI runs Rust checks on Windows and
Linux. Fixture/process tests do not certify live Minecraft clients, all loaders,
deployment security, or unfinished product requirements.

## Java migration evidence

Clone `https://github.com/vesta-project/server-java` beside this checkout as
`server-java` and check out `javaSource.revision` from
`docs/RUST_WRAPPER_PARITY.json`. Then run:

```powershell
node scripts/rust-wrapper-parity.mjs
```

For another location, set `VESTA_JAVA_REPO`. This explicit comparison requires
the pinned checkout and verifies real Java files and hashes. It validates
inventory structure; it does not execute Java tests or certify migration completion.
Standalone Node tests exercise the checker with synthetic fixtures.

The manual `Pinned Java migration comparison` workflow requires the
`SERVER_JAVA_READ_TOKEN` repository secret with read access to the private Java
repository. It fails explicitly if the credential is absent. Ordinary Rust CI
does not claim this external evidence check ran.

Java core/wrapper tests and connector builds now belong to the Java repository.
See [the split contract](docs/operations/repository-split.md).
