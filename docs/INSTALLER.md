# Vesta Server One-Liner Installer

Amp-style installers that scaffold a server directory, download the latest
wrapper from the GitHub **`dist`** branch, optionally download a Minecraft jar,
coach port forwarding, and register an OS autostart service.

The multi-instance registry contract used by installers and future CLI tooling is
defined in [`INSTANCE_REGISTRY.md`](./INSTANCE_REGISTRY.md).

**The installer never installs the Vesta connector mod/plugin.** Vanilla (and
unknown) servers are supported with wrapper-only. For non-vanilla servers,
install the connector later from GitHub Releases or a future panel/launcher
one-click. `connector.auto-update` defaults to `false`.

## One-liners

Linux / macOS:

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/vesta-project/server/dist/install.sh)
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/vesta-project/server/dist/install.ps1 | iex
```

## Flags

| Flag | Meaning |
|------|---------|
| `--root` / `-Root` | Server directory (created if missing) |
| `--port` / `-Port` | Vesta HTTP port (default `9090`) |
| `--minecraft` / `-Minecraft` | `none`, `vanilla`, `paper`, `fabric`, `forge`, `neoforge` |
| `--skip-port-check` / `-SkipPortCheck` | Skip confirmation loop |
| `--no-service` / `-NoService` | Do not register systemd / launchd / scheduled task |
| `--yes` / `-Yes` | Non-interactive defaults |

Example (local packaging smoke test):

```powershell
.\scripts\package-dist.ps1
$env:VESTA_DIST_BASE = "file:///" + ((Resolve-Path .\build\dist).Path -replace '\\','/')
.\scripts\install.ps1 -Root .\build\smoke-server -Minecraft none -SkipPortCheck -NoService -Yes
```

```bash
./scripts/package-dist.sh
VESTA_DIST_BASE="file://$(pwd)/build/dist" ./scripts/install.sh \
  --root ./build/smoke-server --minecraft none --skip-port-check --no-service --yes
```

## Dist branch layout

Published by [`.github/workflows/publish-dist.yml`](../.github/workflows/publish-dist.yml)
or locally via `./scripts/package-dist.sh` / `.\scripts\package-dist.ps1`:

```
dist/
├── install.sh
├── install.ps1
├── manifest.json          # wrapperSha256, builtAt, gitSha
├── defaults/
│   └── wrapper-config.toml
└── wrapper/
    └── latest.zip         # :wrapper:installDist contents
```

Installers verify `wrapperSha256` before extracting.

## Services

| OS | Mechanism |
|----|-----------|
| Linux (root) | `systemd` unit `/etc/systemd/system/vesta-wrapper.service` |
| Linux (user) | `~/.config/systemd/user/vesta-wrapper.service` (+ linger tip) |
| macOS | `launchd` LaunchAgent / LaunchDaemon |
| Windows | Scheduled task `VestaWrapper` (SYSTEM if elevated, else AtLogOn) |

## Port forwarding

The installer prints instructions for TCP `9090` (and `25565` when a Minecraft
jar is present), then loops until you confirm `y` or `skip`. Use
`--skip-port-check` in CI.

## Dev-from-repo helpers

[`install-wrapper.ps1`](../install-wrapper.ps1) / [`install-wrapper.sh`](../install-wrapper.sh)
still install a locally built wrapper into an existing server. Prefer the
one-liners for end users.
