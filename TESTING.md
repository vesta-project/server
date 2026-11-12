# Testing and merge checks

Run from this repository with rustup and Node 22 or later. The checked-in
rust-toolchain.toml selects Rust 1.96.0, matching CI.

~~~text
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo test --workspace --locked -- --test-threads=1
cargo clippy --workspace --all-targets --locked -- -D warnings
node --test scripts/*.test.mjs
node --test scripts/bootstrap/*.test.mjs
node --test scripts/reachability-probe/*.test.mjs
node scripts/requirements-ledger.mjs --check
node scripts/build-roadmap-html.mjs --check
~~~

CI runs the Rust gates on Windows and Ubuntu. Serial execution matches CI and
avoids contention between timing-sensitive owned-process fixtures. Set
CARGO_INCREMENTAL=0 to match CI and limit persistent build-cache growth. Node's
Bash checks run on Linux and are explicitly skipped on Windows.

Ordinary builds/tests need no JDK, Gradle, Minecraft installation, private Java
repository, production signing key or GitHub release credential. Process tests
use owned inert fixtures. Signing tests create temporary test keys. They never
update an existing development server or accept the Minecraft EULA.

## Live platform verification

Fixtures do not establish live Minecraft/loader compatibility, independent Linux
boot/service behaviour, deployment isolation, external network reachability or
low-end host performance. See [verification reports](docs/reviews/) for each run's
environment and limits. Keep integration tests in a dedicated installation
outside source directories; preserve worlds and credentials.

## Historical migration evidence

The Java migration inventory, network-surface map and accounting scripts preserve
the old comparison baseline. The optional comparison command is documented in the
[repository boundary](docs/operations/repository-split.md). It requires an exact
external Java checkout; it is not a merge prerequisite or active CI workflow.
These historical records do not certify every product requirement or old Java
behaviour as implemented.

## Change-aware CI

CI runs once per feature PR, on main pushes, and on manual dispatch. Node contracts always run. `scripts/change-scope.mjs` selects setup/update package checks for isolated changes and full workspace checks for shared/configuration or unknown changes. Bootstrap shell and documentation changes need no Rust build; both required Rust job names report that explicitly. Missing/manual diff baselines select full checks, and a failed diff fails the job. Formatting, check, test and strict Clippy remain required for each selected Rust scope on Windows and Linux.

Only successful main pushes save trusted Rust caches; PRs may restore them and cache failures cannot skip validation. Release builds remain separate. Run classifier/workflow regressions with `node --test scripts/change-scope.test.mjs scripts/ci-scope-contract.test.mjs scripts/bootstrap/nightly-workflow.test.mjs`.

The opt-in public release-catalog smoke fetches only bounded official metadata and a signed platform manifest, never installs binaries: set `VESTA_PUBLIC_RELEASE_SMOKE=1`, then run `cargo test --locked -p vesta-update --lib release::official::tests::public_catalog_and_manifest_smoke -- --ignored --exact --nocapture`. The ordinary suite leaves this network check ignored.
