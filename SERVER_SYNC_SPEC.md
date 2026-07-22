# Vesta Server-Sync Protocol

Enables a Vesta launcher instance to synchronise modpack state with a remote server. The server provides both a **persistent TCP control connection** and an **HTTP REST API** for data transfer.

---

## 1. Architecture

```
┌──────────────────┐    TCP (control, push)    ┌──────────────────┐
│                  │◄─────────────────────────►│                  │
│   Vesta Client   │    HTTP REST (API data)    │   Sync Server    │
│   (launcher)     │◄─────────────────────────►│   (your server)   │
│                  │    HTTP (file downloads)   │                  │
└──────────────────┘◄─────────────────────────►└──────────────────┘
```

| Channel     | Purpose                                                    |
|-------------|------------------------------------------------------------|
| TCP         | Authentication, session management, sync coordination, server push notifications |
| HTTP API    | Manifest retrieval, sync-check, version listing, file metadata |
| HTTP Files  | Bulk file downloads (content-addressable, cache-friendly)  |

### 1.1 Deployment Modes

The Sync Server can be deployed in three topologies:

| Mode | Process | TCP | HTTP | Push | Description |
|------|---------|-----|------|------|-------------|
| **Wrapper** | Standalone sidecar (Java) | ✓ | ✓ | ✓ | Runs 24/7 alongside the Minecraft server. Full protocol. Recommended for all production servers. |
| **Mod/Plugin** | Inside Minecraft server JVM | — | ✓ | — | Embedded HTTP-only server. Launcher polls for updates. Lightweight, no separate process. |
| **Hybrid** | Both installed | Wrapper | Wrapper | ✓ | Mod defers all sync serving to the wrapper. Mod only handles player-join access control. |

#### Wrapper (Standalone)

The wrapper is a Java process that runs as a daemon alongside the Minecraft server. It accepts TCP connections from launcher clients for sync coordination and serves the REST API + file downloads over HTTP. This is the only mode that supports server-initiated push notifications, since the connection is persistent.

When both wrapper and mod/plugin are installed, the wrapper is the authority for sync. The mod will detect the wrapper is present and skip starting its own HTTP server.

#### Mod/Plugin (Embedded)

When no wrapper is available, the mod or plugin can serve a minimal HTTP API directly from the Minecraft server process. This provides manifest retrieval and file downloads, but no TCP session — clients must use the `POST /api/v1/sync/check` REST endpoint to poll for updates.

The mod/plugin does **not** implement the TCP protocol. If the launcher connects on the TCP port and gets no response, it falls back to HTTP polling.

#### Hybrid (Both)

When both components are installed on the same machine:
- The **wrapper** handles all protocol traffic (TCP + HTTP)
- The **mod/plugin** only validates player mods at join time and forwards any sync requests to the wrapper
- The launcher connects to the wrapper's ports; the mod's embedded server is never started

This allows push updates (via TCP) while keeping the in-game enforcement that the mod provides.

---

## 2. Modpack Identification

The server can serve modpacks in **two modes**:

### Mode A — Platform Reference

The server only stores a pointer to a known platform modpack. The client resolves files and versions from Modrinth / CurseForge as it does today.

```json
{
  "mode": "platform",
  "source": "modrinth" | "curseforge",
  "modpack_id": "abc123",
  "version_id": "v1.2.3",
  "minecraft_version": "1.21",
  "modloader": { "type": "fabric", "version": "0.16.0" }
}
```

The server acts as **version coordinator** — it tells the client "you should be on this version". The client fetches the manifest from the platform and runs the normal delta update.

### Mode B — Custom Manifest

The server provides its own full file manifest (see §5). The client treats it identically to a platform manifest internally, running the same three-way diff logic (`$O$` vs `$C$` vs `$N$`).

```json
{
  "mode": "custom",
  "name": "My Private Pack",
  "version": "2.0.0",
  "minecraft_version": "1.21",
  "modloader": { "type": "fabric", "version": "0.16.0" },
  "mods": [ /* … */ ],
  "overrides": [ /* … */ ]
}
```

---

## 3. TCP Protocol Specification

### 3.1 Connection Lifecycle

```
 Client                          Server
   │                               │
   │──── Handshake ───────────────►│
   │◄──── HandshakeResponse ──────│
   │                               │
   │──── SyncCheck ──────────────►│
   │◄──── SyncCheckResponse ──────│
   │                               │
   │  ─ ─ ─  (conversation)  ─ ─  │
   │                               │
   │◄──── Push_UpdateAvailable ───│   (any time)
   │                               │
   │──── SyncComplete ───────────►│
   │                               │
   │  ─ ─ ─  (heartbeat keepalive) │
   │                               │
   │──── Disconnect ─────────────►│
```

### 3.2 Message Format

Every message is **length-prefixed JSON** over TCP:

```
[4 bytes: payload length as big-endian u32]
[payload: UTF-8 JSON object]
```

Consecutive messages are simply concatenated on the wire. The receiver reads 4 bytes, parses the length `N`, then reads exactly `N` bytes of JSON.

### 3.3 Common Envelope

Every message contains:

| Field     | Type     | Description                                          |
|-----------|----------|------------------------------------------------------|
| `type`    | string   | Message discriminator (see §3.4)                     |
| `seq`     | u32      | Monotonically increasing sequence number per direction, used for acknowledgements / ordering |
| `session` | string   | Session UUID from the handshake response; empty before handshake    |
| `ts`      | string   | ISO-8601 timestamp                                   |

### 3.4 Message Types

#### Handshake (C→S)

Sent immediately after TCP connect.

```json
{
  "type": "handshake",
  "seq": 0,
  "session": "",
  "ts": "2026-05-20T12:00:00Z",
  "version": "1.0.0",
  "client_id": "550e8400-e29b-41d4-a716-446655440000",
  "capabilities": ["sync_v1", "delta_v1"]
}
```

| Field          | Description                                                  |
|----------------|--------------------------------------------------------------|
| `version`      | Launcher protocol version                                    |
| `client_id`    | Device / installation UUID, persisted locally                |
| `capabilities` | Feature flags the client supports (server can reject or degrade) |

#### HandshakeResponse (S→C)

```json
{
  "type": "handshake_response",
  "seq": 0,
  "session": "a1b2c3d4-...",
  "ts": "2026-05-20T12:00:01Z",
  "status": "ok" | "error",
  "error_message": null | "reason string",
  "server_version": "1.0.0",
  "session_ttl_seconds": 3600
}
```

On `status: "error"` the server SHOULD close the connection immediately.  
On `status: "ok"` the client uses `session` in all subsequent messages.

#### SyncCheck (C→S)

The client asks whether its current state needs updating. Sent at session start and periodically (every 5–15 minutes, or on user action).

```json
{
  "type": "sync_check",
  "seq": 1,
  "session": "a1b2c3d4-...",
  "ts": "2026-05-20T12:05:00Z",
  "mode": "platform" | "custom",
  "modpack_id": "abc123",
  "source": "modrinth",
  "current_version": "1.2.0",
  "installed_hashes": {
    "mods/jei.jar":   "sha256hex...",
    "config/sodium.json": "sha256hex..."
  }
}
```

| Field              | Description                                                    |
|--------------------|----------------------------------------------------------------|
| `mode`             | Which mode this instance is using                               |
| `modpack_id`       | Platform modpack ID (for mode A) or server pack ID (for mode B) |
| `source`           | `"modrinth"`, `"curseforge"`, or `"server"`                    |
| `current_version`  | Version string of the currently installed manifest              |
| `installed_hashes` | SHA-256 map of files on disk (so server can delta-diff)         |

#### SyncCheckResponse (S→C)

```json
{
  "type": "sync_check_response",
  "seq": 2,
  "session": "a1b2c3d4-...",
  "ts": "2026-05-20T12:05:01Z",
  "needs_update": true,
  "new_version": "1.3.0",
  "changelog": "## What's new\n- Added Sodium\n- Updated JEI",
  "manifest": null,
  "actions": []
}
```

| Field          | Description                                                                                         |
|----------------|-----------------------------------------------------------------------------------------------------|
| `needs_update` | Whether the client needs to sync. If `false`, the client can skip.                                  |
| `manifest`     | The full `ServerModpackManifest` (§5) for mode B. `null` for mode A (client fetches from platform). |
| `actions`      | **Server-driven action list** (§6) — optional. If present, the client executes these directly instead of running its own diff. |
| `changelog`    | Human-readable markdown changelog for the new version.                                              |

When `needs_update` is `false`, the client waits for the next poll interval or a `Push_UpdateAvailable` notification.

#### FileRequest (C→S)

If the server provides `actions` with `source: "server"`, or if the client needs to fetch a file not available on a platform, it requests the file via this message.

```json
{
  "type": "file_request",
  "seq": 5,
  "session": "a1b2c3d4-...",
  "file": "mods/custom-mod.jar",
  "sha256": "sha256hex...",
  "size": 1234567
}
```

The server responds with **either**:

```json
{
  "type": "file_response",
  "seq": 6,
  "session": "a1b2c3d4-...",
  "ts": "2026-05-20T12:05:02Z",
  "file": "mods/custom-mod.jar",
  "sha256": "sha256hex...",
  "method": "url",
  "url": "https://files.example.com/sha256hex..."
}
```

…or for small files (≤1 MB):

```json
{
  "type": "file_response",
  "method": "inline",
  "data_base64": "base64-encoded-content"
}
```

The client downloads the file (via HTTP `url` or inline) and verifies SHA-256 before using it.

#### Push_UpdateAvailable (S→C)

Server pushes this at any time when a new modpack version is published and the client's session has sync-checked with an older version.

```json
{
  "type": "push_update_available",
  "seq": 200,
  "session": "a1b2c3d4-...",
  "ts": "2026-05-20T13:00:00Z",
  "new_version": "1.4.0",
  "changelog": "## Hotfix\n- Fixed crash on startup",
  "requires_restart": false,
  "force_update": false
}
```

| Field             | Description                                                                                        |
|-------------------|----------------------------------------------------------------------------------------------------|
| `requires_restart`| Whether the client must restart the game instance after syncing                                    |
| `force_update`    | If `true`, the client should begin sync immediately without user prompting                         |

On receiving this, the client SHOULD notify the user and (if `force_update` is true) automatically initiate a sync.

#### Heartbeat (bidirectional)

Keepalive to detect dead connections. Sent every 30 seconds if no other message has been sent.

```json
{
  "type": "heartbeat",
  "seq": 10,
  "session": "a1b2c3d4-...",
  "ts": "2026-05-20T12:05:30Z"
}
```

If no message (including heartbeat) is received for 90 seconds, either side may close the connection.

#### Disconnect (bidirectional)

Clean shutdown.

```json
{
  "type": "disconnect",
  "seq": 99,
  "session": "a1b2c3d4-...",
  "ts": "2026-05-20T14:00:00Z",
  "reason": "user_logout" | "shutdown" | "reconnect"
}
```

#### SyncComplete (C→S)

Sent by the client after finishing a sync operation (initial or update).

```json
{
  "type": "sync_complete",
  "seq": 10,
  "session": "a1b2c3d4-...",
  "ts": "2026-05-20T12:10:00Z",
  "status": "success" | "partial" | "error",
  "error_message": null | "One or more files failed SHA-256 verification"
}
```

| Field          | Description                                                             |
|----------------|-------------------------------------------------------------------------|
| `status`       | `"success"` if all files verified. `"partial"` if some succeeded. `"error"` if the sync was fully aborted. |
| `error_message`| Human-readable detail when status is not `"success"`.                   |

---

## 4. REST API Specification

The HTTP REST API runs alongside the TCP listener (same origin recommended). All endpoints are prefixed with `/api/v1`.

### 4.1 Authentication

Authentication is deferred to a future version. The `session` UUID from the TCP handshake is used as a correlation identifier but is **not** verified by the server. Clients pass it as a query parameter for request tracing:

```
GET /api/v1/modpack/manifest?session=<session-uuid>
```

Servers may ignore the session parameter. Authentication with bearer tokens and role-based access will be added in a future protocol version (see §11).

### 4.2 Endpoints

#### `GET /api/v1/modpack/info`

Returns the modpack metadata the server is currently serving.

**Response** `200`:
```json
{
  "name": "My Pack",
  "version": "1.3.0",
  "mode": "platform" | "custom",
  "source": "modrinth" | "curseforge" | "server",
  "modpack_id": "abc123" | null,
  "minecraft_version": "1.21",
  "modloader": { "type": "fabric", "version": "0.16.0" },
  "file_count": 42,
  "total_size_bytes": 52428800,
  "updated_at": "2026-05-20T12:00:00Z"
}
```

**Response** `404` — no modpack configured on this server.

#### `GET /api/v1/modpack/manifest`

Returns the full `ServerModpackManifest` (§5).

**Response** `200`:
```json
{
  "mode": "custom",
  "name": "My Pack",
  "version": "1.3.0",
  "minecraft_version": "1.21",
  "modloader": { "type": "fabric", "version": "0.16.0" },
  "mods": [ /* … */ ],
  "overrides": [ /* … */ ]
}
```

#### `GET /api/v1/modpack/versions`

Lists all available versions.

**Response** `200`:
```json
{
  "versions": [
    { "version": "1.0.0", "published_at": "2026-01-15T…" },
    { "version": "1.1.0", "published_at": "2026-03-01T…" },
    { "version": "1.3.0", "published_at": "2026-05-20T…" }
  ]
}
```

#### `GET /api/v1/modpack/versions/{version}/manifest`

Returns the manifest for a specific historical version (for rollback / audit). Same schema as `/manifest`.

#### `POST /api/v1/sync/check`

Programmatic sync check (alternative to TCP message). Used by headless / polling clients.

**Request:**
```json
{
  "current_version": "1.2.0",
  "installed_hashes": { /* … */ }
}
```

**Response** `200`:
```json
{
  "needs_update": true,
  "new_version": "1.3.0",
  "actions": [ /* … */ ]
}
```

#### `GET /api/v1/files/{sha256}`

Download a file by its SHA-256 hash. The server stores files in a content-addressable store keyed by hash.

**Response** `200`: Binary file content with `Content-Type: application/octet-stream`.  
**Response** `404`: Hash unknown to this server.

This endpoint enables the client to download any file the manifest references with `source: "server"` without needing a separate TCP interaction.

#### `GET /api/v1/console`

Returns recent Minecraft server console lines captured by the wrapper when `minecraft.launch-jar` is configured.

**Query:** `since` (cursor), `limit` (max lines, default 500)  
**Capability:** `console.read`  
**Response** `200`: `{ "running", "cursor", "lines": [{ "cursor", "ts", "stream", "text" }] }`

#### `POST /api/v1/console`

Sends a command to the managed Minecraft server stdin.

**Body:** `{ "command": "say hello" }`  
**Capability:** `console.execute`  
**Response** `200`: `{ "ok": true }`

Realtime clients authenticated with a ticket that includes the `console` scope receive `console_line` events on `/api/v1/realtime`.

#### `GET /api/v1/players`

Online player list when the wrapper manages a Minecraft process (`minecraft.launch-jar`).

**Capability:** `stats.read`  
**Response** `200` (always; never `409`):

- Managed + running: `{ "manageable": true, "running": true, "online", "max"?, "players": [{ "name", "uuid"? }] }`
- Managed + stopped: `{ "manageable": true, "running": false, "online": 0, "players": [] }`
- Not managed: `{ "manageable": false, "players": [] }`

Players are collected by sending `list` to the process stdin and parsing vanilla/Paper console output.

#### `GET /api/v1/fs`

Lists files under the Minecraft server working directory.

**Query:** `path` (directory relative path, empty for server root)  
**Capability:** `files.read`  
**Response** `200`: `{ "path", "entries": [{ "name", "type", "size", "modified_at" }] }`

#### `GET /api/v1/fs/{path}`

Downloads a file by relative path (streamed, no size cap).

**Capability:** `files.read`  
**Response** `200`: `application/octet-stream`

#### `PUT /api/v1/fs/{path}`

Writes or replaces a file (raw request body).

**Capability:** `files.write`  
**Response** `200`: `{ "path", "ok": true }`

#### `DELETE /api/v1/fs/{path}`

Deletes a single file (not directories).

**Capability:** `files.write`  
**Response** `200`: `{ "path", "deleted": true }`

> **Deprecated:** `GET /api/v1/private/files` and `GET /api/v1/private/files/{path}` return `410 Gone`. Use authenticated `GET /api/v1/modpack/manifest` and `GET /api/v1/files/{sha256}` instead.

#### `GET /api/v1/server/descriptor`

Machine discovery document. Includes:

- `capabilities`: high-level feature tokens (`server_descriptor`, `modpack_sync`, `minecraft_challenge`, `realtime_ticket`, `console`, `filesystem`, `server_power`, `content_manage`, `admin_security_loopback`, `sync_check`, `players`, …)
- `endpoints`: map of REST path → capability token
- optional `tls.sha256_fingerprint` when HTTPS is enabled

#### Rate limits

Configurable via TOML (not hardcoded):

```toml
[rate_limits]
enabled = true
http_requests_per_minute = 120
websocket_messages_per_second = 30
auth_attempts_per_minute = 20
```

When enabled, mutating HTTP routes and auth endpoints are token-bucket limited per remote IP.
Exceeded requests return **`429`** with `Retry-After` and JSON `{ "error": "rate limit exceeded", "retry_after": N }`.

---

## 5. ServerModpackManifest

When the server operates in **Mode B (Custom)**, the manifest it provides is structurally close to Vesta's internal `ModpackManifest` but with a few additions.

```json
{
  "manifest_version": 1,
  "mode": "custom",
  "name": "My Private Pack",
  "version": "2.0.0",
  "minecraft_version": "1.21",
  "modloader": {
    "type": "fabric",
    "version": "0.16.0"
  },
  "mods": [
    {
      "path": "mods/sodium-fabric-0.6.0.jar",
      "sha1": "sha1hex",
      "sha256": "sha256hex",
      "size": 5242880,
      "source": {
        "type": "platform",
        "platform": "modrinth",
        "project_id": "AANobbMI",
        "version_id": "vKxJj_2R",
        "url": "https://cdn.modrinth.com/…"
      }
    },
    {
      "path": "mods/custom-utility.jar",
      "sha1": "sha1hex",
      "sha256": "sha256hex",
      "size": 1048576,
      "source": {
        "type": "server",
        "url": null
      }
    }
  ],
  "overrides": [
    {
      "path": "config/sodium-options.json",
      "sha256": "sha256hex",
      "size": 2048,
      "source": {
        "type": "server",
        "url": null
      }
    }
  ],
  "server_info": {
    "host": "play.example.com",
    "port": 25565,
    "description": "The official server for My Private Pack",
    "is_required": true
  }
}
```

### 5.1 Field Reference

| Field              | Type            | Description                                                                  |
|--------------------|-----------------|------------------------------------------------------------------------------|
| `manifest_version` | integer         | Schema version (currently `1`). Clients MUST reject unknown versions.        |
| `mode`             | `"custom"`      | Discriminator.                                                               |
| `name`             | string          | Human-readable modpack name.                                                 |
| `version`          | string          | Semantic version string. This is what `sync_check` compares.                 |
| `minecraft_version`| string          | Required Minecraft version.                                                  |
| `modloader`        | object          | Type and version of the mod loader.                                          |
| `mods`             | array           | Mod files that will be placed in `mods/`.                                    |
| `overrides`        | array           | Config, script, or resource-pack files (everything outside `mods/`).         |
| `server_info`      | object\|null    | Optional server connection info. See §5.3.                                   |

### 5.2 File Entry (`mods[]` / `overrides[]`)

Each entry describes one file the client must have on disk.

| Field      | Type            | Description                                                                               |
|------------|-----------------|-------------------------------------------------------------------------------------------|
| `path`     | string          | Relative path within the game directory (e.g. `"mods/sodium.jar"`).                       |
| `sha1`     | string\|null    | SHA-1 hash (for legacy platform matching).                                                |
| `sha256`   | string\|null    | SHA-256 hash (primary integrity check).                                                   |
| `size`     | integer         | File size in bytes.                                                                       |
| `source`   | source-object   | Where to obtain this file. See below.                                                     |

**Source object:**

| Field         | Type                | Description                                                                                          |
|---------------|---------------------|------------------------------------------------------------------------------------------------------|
| `type`        | `"platform"`, `"server"` | `"platform"` = download from Modrinth/CF (client knows how). `"server"` = download from this sync server. |
| `platform`    | string\|null        | Required when `type: "platform"`: `"modrinth"` or `"curseforge"`.                                    |
| `project_id`  | string\|null        | Platform project ID.                                                                                 |
| `version_id`  | string\|null        | Platform version ID.                                                                                 |
| `url`         | string\|null        | Direct download URL. When non-null the client can use it directly. When `null` the client must resolve from platform (for platform mods) or fetch from `GET /api/v1/files/{sha256}` (for server mods). |

### 5.3 Server Info

```json
"server_info": {
  "host": "play.example.com",
  "port": 25565,
  "description": "The official server for this pack",
  "is_required": true
}
```

| Field          | Type    | Description                                                                |
|----------------|---------|----------------------------------------------------------------------------|
| `host`         | string  | Server hostname or IP.                                                     |
| `port`         | integer | Server port (typically 25565).                                             |
| `description`  | string  | Human-readable server description.                                         |
| `is_required`  | boolean | If `true`, the client should auto-connect to this server after syncing.     |

---

## 6. Sync Actions (Server-Driven)

The server MAY return an `actions` array in the `SyncCheckResponse` (TCP) or `POST /sync/check` (REST) response. When present, the client executes these verbatim instead of running its own three-way diff.

```json
{
  "actions": [
    { "op": "add",    "path": "mods/newmod.jar",    "sha256": "hex…", "source": { /* … */ } },
    { "op": "remove", "path": "mods/oldmod.jar",    "sha256": "hex…" },
    { "op": "update", "path": "mods/changed.jar",   "sha256": "hex…", "source": { /* … */ } },
    { "op": "keep",   "path": "mods/unchanged.jar", "sha256": "hex…" }
  ]
}
```

| op       | Description                                                         |
|----------|---------------------------------------------------------------------|
| `add`    | Download and place this file. If it already exists, overwrite.      |
| `remove` | Delete this file. If missing, skip.                                 |
| `update` | Replace with new version. Same net effect as `remove`+`add`.        |
| `keep`   | Assert this file should exist with this hash. Skip if matches; error if mismatch. |

The purpose of server-driven actions is to let a smart server (e.g., one that resolves CurseForge mods to real filenames server-side) drive the exact file set, avoiding the filename-mismatch churn described in the `mods/mods/` bug.

When server-driven actions are **not** provided, the client falls back to the standard three-way diff (`$O$` vs `$C$` vs `$N$`) described in `docs/features/MODPACK_UPDATING.md`.

---

## 7. Sync Flow (End-to-End)

### 7.1 Initial Connection

```
1. Client opens TCP connection to server
2. Server accepts TCP connection
3. Client sends Handshake (with auth token)
4. Server validates auth, creates session, responds with HandshakeResponse
5. Client queries GET /api/v1/modpack/info to display metadata
6. Client sends SyncCheck (current_version = "none", empty hashes)
7. Server responds with SyncCheckResponse (needs_update = true, manifest or actions)
8. Client processes manifest/actions (downloads files via HTTP API or platform URLs)
9. Client writes files, updates manifest on disk
10. Client sends SyncComplete
```

### 7.2 Subsequent Updates

```
1. Client opens or reuses TCP connection
2. Client sends SyncCheck (current_version = "1.2.0", installed_hashes = {…})
3. Server compares against latest version:
   a. If same version → needs_update = false (done)
   b. If newer version available:
      - Server can respond with full manifest (client diffs locally)
      - Server can respond with action list (server diffs, client executes)
4. Client executes actions / runs local diff
5. Client sends SyncComplete
```

### 7.3 Server-Initiated Update

```
1. Server publishes new modpack version
2. Server iterates active TCP sessions
3. For each session known to be on the older version:
   → Send Push_UpdateAvailable
4. Client prompts user (or auto-syncs if force_update = true)
5. Client sends SyncCheck → receives new manifest / actions
6. Client executes update
7. Client sends SyncComplete
```

---

## 8. Error Handling

| Scenario                     | TCP Behaviour                                        | HTTP Status   |
|------------------------------|------------------------------------------------------|---------------|
| Unknown modpack              | `SyncCheckResponse.needs_update: false`              | `404`         |
| Corrupt manifest             | N/A                                                  | `500`         |
| File not found               | `FileResponse` with error field                      | `404`         |
| Server overloaded            | Delay response or return retry-after hint            | `429`         |
| Protocol version mismatch    | `HandshakeResponse.status: "error"`                  | —             |
| Sync error                   | `SyncComplete.status: "error"` with error_message    | —             |

On any TCP error, the client SHOULD:
1. Log the error
2. Close the connection
3. Wait with exponential backoff (1 s, 2 s, 4 s, … capped at 5 min)
4. Reconnect

---

## 9. Implementation Notes

### 9.1 Rust Types (for reference)

```rust
// ── Envelope ──
struct TcpMessage {
    type: TcpMessageType,
    seq: u32,
    session: Uuid,
    ts: DateTime<Utc>,
}

enum TcpMessageType {
    Handshake(Handshake),
    HandshakeResponse(HandshakeResponse),
    SyncCheck(SyncCheck),
    SyncCheckResponse(SyncCheckResponse),
    FileRequest(FileRequest),
    FileResponse(FileResponse),
    PushUpdateAvailable(PushUpdateAvailable),
    Heartbeat,
    SyncComplete(SyncComplete),
    Disconnect(Disconnect),
}

// ── Manifest ──
enum ModpackMode { Platform, Custom }

struct ServerModpackManifest {
    manifest_version: u32,
    mode: ModpackMode,
    name: String,
    version: String,
    minecraft_version: String,
    modloader: ModloaderInfo,
    mods: Vec<ManifestFile>,
    overrides: Vec<ManifestFile>,
    server_info: Option<ServerInfo>,
}

struct ManifestFile {
    path: String,
    sha1: Option<String>,
    sha256: Option<String>,
    size: u64,
    source: FileSource,
}

enum FileSource {
    Platform { platform: String, project_id: String, version_id: String, url: Option<String> },
    Server { url: Option<String> },
}

struct ServerInfo {
    host: String,
    port: u16,
    description: String,
    is_required: bool,
}

// ── Messages ──
struct SyncComplete {
    status: String,      // "success" | "partial" | "error"
    error_message: Option<String>,
}
```

### 9.2 Client Integration Points

The existing `sync/` module handles:

| Component       | Role in server-sync                                           |
|-----------------|---------------------------------------------------------------|
| `sync/manifest.rs` | Load `$O$` from disk; parse server manifest as `$N$`      |
| `sync/differ.rs`   | Same three-way diff works unchanged for custom manifests  |
| `sync/staging.rs`  | Unchanged — files land in staging then commit atomically  |
| `sync/action_tree.rs` | Unchanged — same SyncAction types used                 |
| `sync/safeguards.rs` | Unchanged                                              |
| `tasks/update_modpack.rs` | Needs a new code path that reads server manifest instead of ZIP |

The server-sync task would look like:

```
1. Connect TCP → handshake → get session
2. GET /api/v1/modpack/manifest → parse ServerModpackManifest
3. Convert ServerModpackManifest to internal ModpackManifest
4. Run three-way diff ($O$ vs $C$ vs $N$)
5. For each file with source.type = "server":
   → download via HTTP GET /api/v1/files/{sha256}
6. For each file with source.type = "platform":
   → download from platform (existing logic)
7. Commit staging, persist manifest, update DB
```

---

## 10. Security Considerations

| Concern                  | Mitigation                                                                 |
|--------------------------|----------------------------------------------------------------------------|
| Man-in-the-middle        | All communication over TLS (TCP+TLS for control, HTTPS for API).           |
| Malicious file injection | Client verifies SHA-256 of every downloaded file before placing on disk.   |
| Connection flood         | Server rate-limits handshake attempts per IP.                              |
| Path traversal           | Server enforces `path` values are relative and do not contain `..`.        |

## 11. Future Considerations

| Feature                       | Description                                                             |
|-------------------------------|-------------------------------------------------------------------------|
| **Authentication & access control** | Bearer token auth on handshake, role-based sessions (admin, sync, read-only), BCrypt token storage. |
| **Manifest signing**          | Server signs manifest with Ed25519; client verifies signature.           |
| **Per-file access policy**    | Server can mark files as `required`, `optional`, or `block` to control what clients must/may/must-not install. |
| **User-scoped modpacks**      | Server returns different manifests per authenticated user (e.g., beta testers see a different version). |
| **Partial file synchronization** | Only download files that changed, using the server-driven action list. |
| **Server-side diffing**       | Server computes the delta from `installed_hashes` and sends only the action list (saves client compute). |
| **File upload**               | `POST /api/v1/files/upload` so modpack authors can push updates via API. |
| **WebSocket alternative**     | Fallback to WebSocket for browser-based clients or environments where raw TCP is unavailable. |
| **Multi-pack server**         | Server serves multiple modpacks; client selects by `modpack_id`.        |
