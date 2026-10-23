# Vesta server agent

This repository contains the Rust agent, supervisor, management API, and the
current server product requirements. Minecraft Java mods/plugins, shared Java
code, and the retained legacy Java wrapper moved to
[vesta-project/server-java](https://github.com/vesta-project/server-java).

## Development

For a session with freshly built server commands on PATH, run `cargo dev` (or
`cargo run -p vesta-dev --`). Use `vesta-rebuild` inside it and `exit` to return.
Use `cargo dev --release` when measuring runtime performance. See the
[development shell guide](docs/operations/dev-session.md) for server selection,
setup previews, and applying rebuilt binaries.

Use the CI Rust toolchain (1.96.0) and Node 22 or later:

```powershell
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
node --test scripts/*.test.mjs
node --test scripts/bootstrap/*.test.mjs
node --test scripts/reachability-probe/*.test.mjs
node scripts/requirements-ledger.mjs --check
node scripts/build-roadmap-html.mjs --check
```

See [TESTING.md](TESTING.md) for the pinned Java migration comparison and
[RUNNING.md](RUNNING.md) for the agent entry point.

The [guided host setup](docs/operations/host-setup.md) prepares a private local
host for app onboarding. See [signed release packaging](docs/operations/bootstrap-release.md)
and the [terminal screenshot gallery](docs/screenshots/setup/gallery.html).

The [requirements](docs/SERVER_SOFTWARE_REQUIREMENTS.md),
[implementation plan](docs/SERVER_SOFTWARE_IMPLEMENTATION_PLAN.md), and
[roadmap](docs/SERVER_SOFTWARE_ROADMAP.html) remain authoritative here. The
current implementation has outstanding UI, live-server, deployment, and Linux
verification gaps; the Java extraction does not promote any requirement status.

## Shared contracts

`protocol/` is the canonical source for compatibility schemas and fixtures.
The Java repository keeps an explicitly pinned snapshot so both builds work
independently. See [the repository split contract](docs/operations/repository-split.md).
