# Running Vesta Server Sync (Wrapper)

The wrapper scans `mods/` and `config/` from a server directory, builds a manifest, and serves sync over **one public port** (default **9090**) with TCP + HTTP multiplexed.

## Prerequisites

- **JDK 17+** (JDK 21 recommended for Gradle)
- A Minecraft server directory (or any folder with `mods/` and `config/`)

## 1. Build

From `V:\server`:

```powershell
.\gradlew.bat :core:test :wrapper:installDist
```

Or install into an existing server (requires backup ack):

```powershell
.\install-wrapper.ps1 -ServerRoot "V:\path\to\mc-server" -BackupAck
```


The wrapper is the primary product. Optional tiered mod JARs are documented in [`docs/PUBLISHING.md`](docs/PUBLISHING.md). Product roadmap (auth, remote install, launcher plans, tests): [`docs/PRODUCT_ROADMAP.md`](docs/PRODUCT_ROADMAP.md).

## 2. Prepare a test server layout

Run the wrapper from your Minecraft server root (or point `scan-root` at it):

```
your-server/
├── mods/
│   └── some-mod.jar
├── config/
│   └── some-config.json
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

Use it to view status/info/manifest, obtain a bearer token (password or link code), run sync-check, and upload/remove a test file.

### PowerShell smoke test

With the wrapper running:

```powershell
cd V:\server
.\run.ps1
```

### HTTP endpoints

```powershell
# Modpack metadata
Invoke-RestMethod https://localhost:9090/api/v1/modpack/info

# Full manifest (includes file hashes)
Invoke-RestMethod https://localhost:9090/api/v1/modpack/manifest

# Sync check
$body = @{
  current_version = "0.9.0"
  installed_hashes = @{}
} | ConvertTo-Json
Invoke-RestMethod -Method POST -Uri https://localhost:9090/api/v1/sync/check -Body $body -ContentType "application/json"

# Download a file by SHA-256 (from manifest)
Invoke-WebRequest https://localhost:9090/api/v1/files/<sha256hex> -OutFile downloaded.bin
```

### TCP protocol

When TCP and HTTP share port 9090, clients must send a **`0x00` byte** before each length-prefixed JSON frame. See [run.ps1](run.ps1) for an example handshake.

## 5. Run unit tests only

```powershell
.\gradlew.bat :core:test
```

## Notes

- **Files**: Scanned from `mods/` and `config/` directly (no `vesta-files` duplicate). Known MR/CF entries go in `.vesta-known.json`.
- **Private files**: Listed at `/api/v1/private/files` when authenticated and permitted.
