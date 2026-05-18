# Vesta Server Sync — Architecture

## Overview

Client-server sync for Minecraft. Players enter their server IP into the Vesta launcher, and the app automatically knows what files are needed to join.

Two deployment modes:
- **Wrapper** — standalone sidecar process, runs 24/7, provides HTTP file serving + TCP console/control
- **Mod/Plugin** — in-game mod/plugin that enforces access control at player join time

## Project Layout

```
V:\server\
├── settings.gradle.kts          # Root: stonecutter create() + core/wrapper includes
├── stonecutter.gradle.kts       # Active version, loader constants, shared plugins
├── build.gradle.kts             # Minimal — just java plugin
├── gradle.properties            # Shared properties
├── gradlew / gradlew.bat
├── gradle\wrapper\
│
├── build.fabric.gradle.kts      # Split buildscript: Fabric (fabric-loom, ≤1.21.x)
├── build.fabric26.gradle.kts    # Split buildscript: Fabric 26.x (net.fabricmc.fabric-loom, unobfuscated)
├── build.quilt.gradle.kts       # Split buildscript: Quilt (org.quiltmc.loom)
├── build.neoforge.gradle.kts    # Split buildscript: NeoForge (neoforge moddev)
├── build.bukkit.gradle.kts      # Split buildscript: Bukkit (spigot-api)
├── build.paper.gradle.kts       # Split buildscript: Paper (paper-api)
│
├── versions\                    # Per-version per-loader gradle.properties
│   ├── 1.21.4-fabric\gradle.properties
│   └── ...
│
├── buildSrc\                    # Convention plugins
│   └── src\main\kotlin\
│       └── vesta-core.gradle.kts
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
└── loader\                      # Single source tree, compiled per-loader
    └── src\main\java\com\vesta\loader\
        ├── VestaMod.java        # Common init
        ├── ManifestGenerator.java
        ├── JoinValidator.java
        ├── FileServer.java
        └── platform\            # Loader-specific entrypoints
            ├── FabricEntrypoint.java
            ├── QuiltEntrypoint.java
            ├── NeoForgeEntrypoint.java
            ├── BukkitEntrypoint.java
            └── PaperEntrypoint.java
```

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

## Supported Loaders

| Loader | Build plugin | MC versions |
|---|---|---|
| Fabric (legacy) | fabric-loom | 1.20.1, 1.20.4, 1.20.6, 1.21.1, 1.21.4 |
| Fabric (26.x) | net.fabricmc.fabric-loom (unobfuscated) | 26.1, 26.1.1, 26.1.2 |
| Quilt | org.quiltmc.loom | 1.20.1, 1.20.4, 1.20.6, 1.21.1, 1.21.4 |
| NeoForge | net.neoforged.moddev | 1.20.6, 1.21.1, 1.21.4, 26.1, 26.1.1, 26.1.2 |
| Bukkit | java-library + spigot-api | 1.20.1, 1.20.4 |
| Paper | java-library + paper-api | 1.20.6, 1.21.1, 1.21.4, 26.1, 26.1.1, 26.1.2 |

Forge deferred due to MDK complexity.

## Protocol

### Modpack Discovery (REST — no auth required)

- `GET /vesta/manifest.json` → modpack metadata
- `GET /vesta/files/{sha256}` → file binary
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
