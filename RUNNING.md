# Running Vesta Server Sync (Wrapper)

The wrapper scans `mods/` and `config/` from a server directory, builds a manifest, and serves sync over **one public port** (default **9090**) with TCP + HTTP multiplexed.

## Quick install (recommended)

One-liners download the latest wrapper from the GitHub `dist` branch, scaffold
dirs, write config, optionally register a service, and coach port forwarding.
They **do not** install the Vesta connector mod/plugin (vanilla-safe).

Linux / macOS:

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/vesta-project/server/dist/install.sh)
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/vesta-project/server/dist/install.ps1 | iex
```

Details, flags, `dist` layout, and the instance registry schema contract:
[`docs/INSTALLER.md`](docs/INSTALLER.md), [`docs/INSTANCE_REGISTRY.md`](docs/INSTANCE_REGISTRY.md).

## Prerequisites

- **JDK 17+** (JDK 21 recommended for Gradle)
- A Minecraft server directory (or any folder — the installer can create one)

## 1. Build (developers)

From the repo root:

```powershell
.\gradlew.bat :core:test :wrapper:installDist
```

Package one-liner artifacts locally:

```powershell
.\scripts\package-dist.ps1
```

Or install a locally built wrapper into an existing server (requires backup ack):

```powershell
.\install-wrapper.ps1 -ServerRoot "V:\path\to\mc-server" -BackupAck
```


The wrapper is the primary product. Optional tiered mod JARs are documented in [`docs/PUBLISHING.md`](docs/PUBLISHING.md). Product roadmap (auth, remote install, launcher plans, tests): [`docs/PRODUCT_ROADMAP.md`](docs/PRODUCT_ROADMAP.md). The in-game connector is **optional** and never auto-installed by the one-liner — see `[connector]` in `wrapper-config.toml`.

## 2. Prepare a test server layout

Run the wrapper from your Minecraft server root (or point `scan-root` at it):

```
your-server/
├── mods/
│   └── some-mod.jar
├── config/
│   └── some-config.json
├── plugins/              # optional; unused on vanilla
└── wrapper-config.toml   # optional — copy from wrapper resources
```

Copy [wrapper-config.toml](wrapper/src/main/resources/wrapper-config.toml) to the server root and edit as needed:

```toml
[server]
scan-root = "."          # directory containing mods/ and config/
scan-paths = ["mods", "config"]

[server.public]
host = "localhost"       # hostname clients use to download files
port = 9090

[server.tls]
enabled = true            # HTTPS/WSS on the Vesta port
auto-generate = true      # creates .vesta/tls.p12 on first start

[modpack]
version = "1.0.0"        # bump when you change files

[connector]
auto-update = false      # reserved/no-op; leave false (installers never auto-install the connector)
```

### Choose a pack source

Set `modpack.source` explicitly:

| Source | Meaning |
|---|---|
| `self-hosted` | Scan live `mods/` / `config/` and publish hashed files |
| `modrinth` | Reference a Modrinth project + version (launcher resolves the archive) |
| `curseforge` | Reference a CurseForge modpack + file ID (launcher resolves the archive) |
| `archive` | Host a local `.zip` / `.mrpack` as one authenticated blob (not extracted) |

```toml
[modpack]
source = "modrinth"

[modpack.platform]
project-id = "your-project-id"
version-id = "your-version-id"
download-url = ""        # optional override; Modrinth/CurseForge resolve by IDs
```

For a pre-built pack file already on disk:

```toml
[modpack]
source = "archive"

[modpack.archive]
path = "packs/my-pack.mrpack"   # relative to scan-root
```

The wrapper serves that archive at `/api/v1/modpack/archive` under the same pack
access gate. It does **not** unpack the archive into `mods/` or re-host its
contents as individual files.

### Start Minecraft with the wrapper

The wrapper can optionally own the Minecraft server process:

```toml
[minecraft]
auto-start = true
launch-jar = "fabric-server-launch.jar"
working-directory = "."
java-command = "java"
jvm-args = ["-Xms2G", "-Xmx6G"]
server-args = ["nogui"]
```

Paths are resolved from `working-directory`. When the wrapper shuts down, it
also stops the Minecraft process. Keep `auto-start = false` when another panel
or service manager owns the server process.

## 3. Start the wrapper

```powershell
cd path\to\your-server
V:\server\wrapper\build\install\wrapper\bin\wrapper.bat
```

Or with a custom config:

```powershell
wrapper.bat --config V:\path\to\wrapper-config.toml
```

You should see logs like:

```
Manifest scan complete: N mods, M overrides
Vesta Server listening on port 9092 (Multiplexed: false)
HTTPS REST/WSS on :9090
```

## 4. Try it

### Browser devtools UI

With the wrapper running, open:

```
https://localhost:9090/devtools
```

Devtools is a local debug tool (loopback-only when `[devtools].loopback_only = true`) and is intentionally not a production admin panel.
For day-to-day ops, use `vesta-wrapperctl` and the loopback-gated HTTP APIs (for example `GET/PUT /api/v1/admin/security`).

```powershell
# Multi-instance CLI (ports always read from wrapper-config.toml)
.\scripts\vesta-wrapperctl.ps1 register --config .\demo-server\wrapper-config.toml
.\scripts\vesta-wrapperctl.ps1 list
.\scripts\vesta-wrapperctl.ps1 status --instance demo-server
.\scripts\vesta-wrapperctl.ps1 api GET /api/v1/status --instance demo-server
.\scripts\vesta-wrapperctl.ps1 devtools --instance demo-server
```

```bash
./scripts/vesta-wrapperctl.sh register --config ./demo-server/wrapper-config.toml
./scripts/vesta-wrapperctl.sh status --instance demo-server
```

See [`docs/INSTALLER.md`](docs/INSTALLER.md) and [`docs/INSTANCE_REGISTRY.md`](docs/INSTANCE_REGISTRY.md).

### PowerShell smoke test

With the wrapper running:

```powershell
cd V:\server
.\run.ps1
```

### HTTP endpoints

Pack content (`GET /api/v1/modpack/manifest` and `GET /api/v1/files/{sha256}`) requires a
bearer token with `pack.fetch` in non-open security modes. Obtain a token via devtools,
`POST /api/v1/auth/password`, or the Minecraft link flow.

```powershell
$headers = @{ Authorization = "Bearer <token>" }

# Modpack metadata (public)
Invoke-RestMethod https://localhost:9090/api/v1/modpack/info

# Full manifest (mods + overrides, includes file hashes)
Invoke-RestMethod https://localhost:9090/api/v1/modpack/manifest -Headers $headers

# Sync check
$body = @{
  current_version = "0.9.0"
  installed_hashes = @{}
} | ConvertTo-Json
Invoke-RestMethod -Method POST -Uri https://localhost:9090/api/v1/sync/check -Body $body -ContentType "application/json"

# Download a file by SHA-256 (from manifest)
Invoke-WebRequest https://localhost:9090/api/v1/files/<sha256hex> -Headers $headers -OutFile downloaded.bin

# Console snapshot (requires console.read, level 2+)
Invoke-RestMethod https://localhost:9090/api/v1/console -Headers $headers

# Run a server command (requires console.execute, level 4)
Invoke-RestMethod -Method POST -Uri https://localhost:9090/api/v1/console `
  -Headers $headers -ContentType "application/json" -Body '{"command":"list"}'

# Browse server files (requires files.read, level 3+)
Invoke-RestMethod "https://localhost:9090/api/v1/fs?path=" -Headers $headers
Invoke-WebRequest https://localhost:9090/api/v1/fs/server.properties -Headers $headers -OutFile server.properties

# Online players (requires stats.read; manageable=false when no launch-jar)
Invoke-RestMethod https://localhost:9090/api/v1/players -Headers $headers

# Write / delete under the Minecraft working directory (requires files.write)
Invoke-RestMethod -Method PUT -Uri https://localhost:9090/api/v1/fs/notes.txt `
  -Headers $headers -ContentType "text/plain" -Body "hello"
Invoke-RestMethod -Method DELETE -Uri https://localhost:9090/api/v1/fs/notes.txt -Headers $headers
```

### End-to-end smoke (`scripts/smoke-wrapper.*`)

Automated Phase 3 checks against `demo-server` (ephemeral ports, auto tear-down):

```powershell
# Full (starts Paper briefly for console POST + cursor progression)
.\scripts\smoke-wrapper.ps1

# Wrapper-only (skips Minecraft start; console POST skipped)
.\scripts\smoke-wrapper.ps1 -SkipMinecraft
```

```bash
./scripts/smoke-wrapper.sh
./scripts/smoke-wrapper.sh --skip-minecraft
```

Covers: `GET /status`, `POST /sync/check`, descriptor `capabilities`/`endpoints`, fs list/write/read/delete,
console GET (+ POST when a paper jar is present), `GET /players`, `[rate_limits]` presence, and
`vesta-wrapperctl status`. Realtime `console_line` over WebSocket needs a linked bearer ticket — the
open-mode smoke verifies REST cursor progression instead.

### TCP protocol

When TCP and HTTP share port 9090, clients must send a **`0x00` byte** before each length-prefixed JSON frame. See [run.ps1](run.ps1) for an example handshake.

## 5. Run unit tests only

```powershell
.\gradlew.bat :core:test :wrapper:test
```

## Notes

- **Files**: Scanned from `mods/` and `config/` directly (no `vesta-files` duplicate). Known MR/CF entries go in `.vesta-known.json`.
- **Rate limits**: configure `[rate_limits]` in `wrapper-config.toml` (`enabled`, `http_requests_per_minute`, `websocket_messages_per_second`, `auth_attempts_per_minute`). Mutating HTTP + auth routes return `429` + `Retry-After` when exceeded.
- **Deprecated**: `GET /api/v1/private/files` returns `410 Gone`. Use the gated manifest and `GET /api/v1/files/{sha256}` instead.
