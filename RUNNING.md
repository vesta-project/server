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

[modpack]
version = "1.0.0"        # bump when you change files
```

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
Vesta Server listening on port 9090 (Multiplexed: true)
HTTP REST internally on :9190 (Multiplexed on public port :9090)
```

## 4. Try it

### Browser devtools UI

With the wrapper running, open:

```
http://localhost:9090/devtools
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
Invoke-RestMethod http://localhost:9090/api/v1/modpack/info

# Full manifest (includes file hashes)
Invoke-RestMethod http://localhost:9090/api/v1/modpack/manifest

# Sync check
$body = @{
  current_version = "0.9.0"
  installed_hashes = @{}
} | ConvertTo-Json
Invoke-RestMethod -Method POST -Uri http://localhost:9090/api/v1/sync/check -Body $body -ContentType "application/json"

# Download a file by SHA-256 (from manifest)
Invoke-WebRequest http://localhost:9090/api/v1/files/<sha256hex> -OutFile downloaded.bin
```

### TCP protocol

When TCP and HTTP share port 9090, clients must send a **`0x00` byte** before each length-prefixed JSON frame. See [run.ps1](run.ps1) for an example handshake.

## 5. Run unit tests only

```powershell
.\gradlew.bat :core:test
```

## Notes

- **File store**: Scanned files are copied into `vesta-files/` (content-addressable by SHA-256).
- **Version bumps**: Change `modpack.version` in config when you update mods; clients compare this string plus file hashes.
- **Mod fallback**: If the wrapper is not running, the in-game mod serves the same HTTP API from the server JVM (no TCP). The mod detects the wrapper on `127.0.0.1:9090` and defers when present.
