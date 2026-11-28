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

The CI workflow runs for feature PRs and manual validation. The main-push release workflow calls it once. Node contracts run alongside the Rust jobs after a separate job determines Rust scope. `scripts/change-scope.mjs` unions the current change scope with pending-release scope, so a docs-only push cannot publish untested earlier Rust changes. Bootstrap shell and documentation-only changes need no Rust build. A failed diff fails validation.

For PRs, reviewed portable protocol paths run tests on Linux, and reviewed native adapters route tests to their OS. Shared Rust source and test changes run on both platforms. Source changes include downstream Cargo packages. CI changes and unknown paths keep both platforms and use the existing Rust scope. Both PR runners still check formatting and compile all selected targets with strict Clippy. Manual and release validation keep the full set of existing checks on both platforms for the selected Rust scope and test the exact commit.

PRs that fall back to the whole workspace build test binaries once per OS. Eight isolated runners per OS execute the resulting exact-name partitions, with one test thread per runner. The build jobs run doctests. Both required Rust checks wait for every build and applicable shard.

Only successful main pushes save trusted Rust build caches. PRs may restore them, and every selected test runs fresh after restore. Cache keys separate OS image, architecture, Rust toolchain, build settings, Cargo inputs, and the pinned launcher revision. Run classifier/workflow regressions with `node --test scripts/change-scope.test.mjs scripts/ci-scope-contract.test.mjs scripts/bootstrap/nightly-workflow.test.mjs`.

The opt-in public release-catalog smoke fetches only bounded official metadata and a signed platform manifest, never installs binaries: set `VESTA_PUBLIC_RELEASE_SMOKE=1`, then run `cargo test --locked -p vesta-update --lib release::official::tests::public_catalog_and_manifest_smoke -- --ignored --exact --nocapture`. The ordinary suite leaves this network check ignored.
