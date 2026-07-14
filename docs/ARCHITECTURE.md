# Vesta Server Sync — Architecture

## Overview

Client-server sync for Minecraft. Players enter their server IP into the Vesta launcher, and the app automatically knows what files are needed to join.

Two deployment modes (wrapper-first):
- **Wrapper** (primary) — standalone sidecar process, runs 24/7, MC-agnostic HTTP file serving + TCP sync
- **Mod/Plugin** (optional) — in-game fallback when wrapper is absent; future auth and join control

Product decisions, auth/IPC roadmap, postponed launcher UI, and test gates: **[`PRODUCT_ROADMAP.md`](PRODUCT_ROADMAP.md)**.

## Project Layout

```
V:\server\
├── settings.gradle.kts          # Stonecutter release lines from versions-matrix.toml
├── versions-matrix.toml         # Tiered mod release lines (single source of truth)
├── stonecutter.gradle.kts       # Active release line, loader constants
├── build.gradle.kts             # Minimal — just java plugin
├── gradle.properties            # Shared mod + loader dependency pins
├── gradlew / gradlew.bat
├── gradle\wrapper\
│
├── build.fabric.gradle.kts      # Split buildscript: Fabric (fabric-loom, ≤1.21.x)
├── build.fabric26.gradle.kts    # Split buildscript: Fabric 26.x (unobfuscated)
├── build.quilt.gradle.kts       # Split buildscript: Quilt
├── build.neoforge.gradle.kts    # Split buildscript: NeoForge
├── build.bukkit.gradle.kts      # Split buildscript: Bukkit
├── build.paper.gradle.kts       # Split buildscript: Paper
│
├── versions\                    # Generated at Gradle sync (gitignored properties)
│   └── {release-id}\            # e.g. fabric-1.21
│
├── buildSrc\                    # Convention plugins (vesta-core)
│
├── docs\PUBLISHING.md           # Wrapper vs mod build targets
│
├── core\                        # MC-agnostic library (Java 8 bytecode target)
│   ├── build.gradle.kts
│   └── src\main\java\com\vesta\sync\
│       ├── manifest\            # Manifest.java, FileEntry.java, FilePolicy.java
│       ├── hashing\             # FileHasher.java, HashIndex.java
│       ├── protocol\            # ProtocolVersion.java, AuthToken.java
│       └── util\                # PathSanitizer.java, ManifestDiff.java
│
├── wrapper\                     # Standalone sidecar (Java 17+ target)
│   ├── build.gradle.kts
│   └── src\main\java\com\vesta\wrapper\
│       ├── VestaWrapper.java    # CLI entry
│       ├── config\              # WrapperConfig.java
│       ├── server\              # HttpServer, ManifestHandler, FileHandler
│       ├── console\             # ConsoleBridge, ConsoleSession
│       ├── control\             # ServerProcess, ServerStatus
│       ├── auth\                # TokenStore, AccessControl
│       └── tcp\                 # TcpControlServer (future)
│
└── src\                         # Single mod source tree, compiled per release line
    └── main\java\com\vesta\loader\
        ├── VestaMod.java
        ├── VestaSyncManager.java
        ├── server\ModHttpServer.java
        └── platform\            # Loader-specific entrypoints
            ├── FabricEntrypoint.java
            ├── QuiltEntrypoint.java
            ├── NeoForgeEntrypoint.java
            ├── BukkitEntrypoint.java
            └── PaperEntrypoint.java
```

## Build versioning (wrapper-first + tiered mods)

- **Wrapper** has no MC version matrix — one build serves all servers.
- **Mod** ships **release lines** (~12 JARs), not per-patch builds. Each line covers an MC generation (e.g. `fabric-1.21` supports 1.21.1–1.21.4).
- Release lines are defined in [`versions-matrix.toml`](../versions-matrix.toml). Gradle sync generates `versions/{id}/gradle.properties` (gitignored).
- JARs are named `vesta-sync-{version}+{release-id}.jar` and declare MC ranges in mod metadata.
- See [`docs/PUBLISHING.md`](PUBLISHING.md) for build commands.

## Technology

| Layer | Choice |
|---|---|
| Language | Java (Kotlin for Gradle build logic) |
| Build system | Gradle 9.x + stonecutter (split buildscripts) |
| Core library target | Java 8 bytecode (server compatibility) |
| Wrapper target | Java 17+ (standalone, can bundle JRE) |
| HTTP server (wrapper) | Jetty 12 |
| TLS | BouncyCastle (self-signed cert generation) |
| Token hashing | BCrypt |
| JSON | Gson |

## Supported Loaders (release lines)

| Release line | Loader | Supported MC range |
|---|---|---|
| `fabric-1.20` | Fabric | 1.20.1 – 1.20.4 |
| `fabric-1.21` | Fabric | 1.21.1 – 1.21.4 |
| `fabric-26` | Fabric (26.x) | 26.1+ |
| `quilt-1.20` | Quilt | 1.20.1 – 1.20.4 |
| `quilt-1.21` | Quilt | 1.21.1 – 1.21.4 |
| `neoforge-1.20` | NeoForge | 1.20.6 |
| `neoforge-1.21` | NeoForge | 1.21.1 – 1.21.4 |
| `neoforge-26` | NeoForge | 26.1+ |
| `bukkit-1.20` | Bukkit | 1.20.1 – 1.20.4 |
| `paper-1.20` | Paper | 1.20.6 |
| `paper-1.21` | Paper | 1.21.1 – 1.21.4 |
| `paper-26` | Paper | 26.1+ |

Forge deferred due to MDK complexity.

## Protocol

### Modpack Discovery (REST — no auth required)

- `GET /api/v1/modpack/info` → modpack metadata
- `GET /api/v1/modpack/manifest` → full manifest
- `GET /api/v1/files/{sha256}` → file binary
- Server listens on `:9090` (default)
- Hash-based file addressing prevents path traversal

### Console & Control (TCP — authenticated — Phase 2)

- TLS 1.3 TCP on `:9091`
- JSON-newline messages
- Roles: admin, console, sync
- Pre-shared tokens with BCrypt storage

## Security

| Layer | Mechanism |
|---|---|
| Transport | HTTPS (Jetty self-signed cert, TOFU fingerprint) |
| File integrity | SHA256 per file, client verifies every download |
| Path safety | PathSanitizer — rejects `..`, absolute paths, symlink escapes |
| Token storage | BCrypt-hashed, never in manifest JSON |
| Downloads | Quarantine temp dir → verify hash → atomic move |

## Access Control

Server owner configures via `wrapper-config.toml` / `vesta-config.toml`:
- `enforce = "exact"` | `"required-only"` | `"custom"`
- Per-file policy: `required`, `optional`, `block_if_present`
- Manifest carries `FilePolicy` enum per file

## Vesta Launcher Integration (Phase 3)

New module in `V:\launcher\vesta-launcher\src-tauri\src\sync\`:
- `ServerSyncManager` — Tauri state, manages server connections
- `manifest.rs` — fetch/parse manifest over HTTP
- `download.rs` — queue files through existing ResourceManager pipeline
- `commands/sync.rs` — Tauri IPC: `connect_server`, `sync_files`, `disconnect_server`
- `stores/server-sync.ts` — SolidJS reactive store, diff view, progress

Reuses: ResourceManager, TaskManager, Instance model, NetworkManager.

## Phase Roadmap

| Phase | Deliverable |
|---|---|
| 1 | Gradle skeleton, core library, wrapper HTTP server |
| 2 | Wrapper: manifest generation, file serving, TLS, token auth |
| 3 | Launcher integration: ServerSyncManager, Tauri commands, frontend |
| 4 | loader-core + loader/fabric |
| 5 | Remaining loaders (neoforge, bukkit, paper) |
| 6 | Wrapper TCP console/control server |
| 7 | Launcher: console viewer, server control UI |
