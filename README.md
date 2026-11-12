# Vesta server

Rust server management for Minecraft: the Vesta agent, per-server supervisor,
local CLI, guided installer, and launcher management API. Supported release
builds target Windows x86_64 and Linux x86_64/ARM64.

Minecraft runs as a separate Java process. Building Vesta requires no JDK,
Gradle or Java repository. Optional Minecraft connectors live in
[server-java](https://github.com/vesta-project/server-java); the legacy Java
wrapper is not part of this build or execution path.

## Get started

On Linux, start the signed installer and choose the Vesta channel/version inside it:

```bash
(
  set -e
  umask 077
  installer=$(mktemp)
  trap 'rm -f -- "$installer"' EXIT
  curl --disable -fsSL --proto '=https' --proto-redir '=https' --connect-timeout 15 --max-time 60 -o "$installer" https://raw.githubusercontent.com/vesta-project/server/main/scripts/bootstrap/install.sh
  bash "$installer"
)
```

Add `--channel stable`, `--latest-nightly`, or `--tag RELEASE` to preselect the
server package. The newest independent installer is used for any of these choices.
The script detects x86_64/ARM64 and checks signed setup identity and binary integrity
before execution. It needs Bash, curl, standard Linux tools and OpenSSL; no Python,
jq or Node runtime. See [bootstrap prerequisites and trust](docs/operations/bootstrap-release.md#install-on-linux).

- [Install and onboard a host](docs/operations/host-setup.md)
- [Build and run locally](RUNNING.md)
- [Use the server-scoped CLI](docs/operations/scoped-cli.md)
- [Package signed releases](docs/operations/bootstrap-release.md)
- [Verify and stage signed updates](docs/operations/signed-updates.md)

This is unreleased software. Signed installation and update staging are available;
Vesta executable activation and health-check rollback are not yet implemented.
Same-account portable operation is not a security sandbox for untrusted plugins.
See the [requirements](docs/SERVER_SOFTWARE_REQUIREMENTS.md),
[implementation plan](docs/SERVER_SOFTWARE_IMPLEMENTATION_PLAN.md), and
[roadmap](docs/SERVER_SOFTWARE_ROADMAP.html) for scope and verification limits.

## Development

Install Rust through rustup and Node 22 or later. The rust-toolchain.toml file selects
Rust 1.96.0, matching CI, including rustfmt and Clippy.

On Windows, run `./dev.ps1` from PowerShell to create or update `dev-server`.
Its persistent data lives in `.dev/servers` in this checkout, excluded from Git.
First-run setup leaves Minecraft stopped and the EULA unaccepted.
The command returns once its updated agent is online and preserves whether
Minecraft was running. Use `./dev.ps1 restart <name>` for another server,
`./dev.ps1 build` to build only, or `./dev.ps1 shell` for an optional shell
with the tools on PATH. On Linux, use `cargo dev update [name]`.
Add `--release`
when measuring performance. See the [development guide](docs/operations/dev-session.md)
and [test/merge checks](TESTING.md).

## Repository layout

| Path | Contents |
| --- | --- |
| apps/vesta-agent | Agent executable and server-scoped CLI |
| apps/vesta-setup | Guided installation and local update staging |
| crates/ | State, protocol, control, transport, game runtime, installer, supervisor and update libraries |
| tools/vesta-dev | Development builds, agent updates and optional shell |
| protocol/ | Canonical compatibility schemas and fixtures |
| scripts/ | Release/bootstrap, development and validation tools |
| docs/ | [Documentation index](docs/README.md), requirements and verification evidence |

Historical Java migration inventories and comparison scripts remain as audit
evidence, not runtime/build dependencies. See the
[repository boundary](docs/operations/repository-split.md).
