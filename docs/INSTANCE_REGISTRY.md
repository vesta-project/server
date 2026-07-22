# Instance Registry Schema

The installer and `vesta-wrapperctl` share a per-user instance registry file.

## File location

- Windows: `%USERPROFILE%/.vesta/instances.json`
- Linux/macOS: `$HOME/.vesta/instances.json`

Parent directory `.vesta/` should be created if missing.

## Schema contract (version 1)

```json
{
  "version": 1,
  "instances": [
    {
      "name": "my-paper-server",
      "config": "C:/servers/paper/wrapper-config.toml",
      "root": "C:/servers/paper",
      "server_id": "optional-uuid-from-config",
      "service_name": "vesta-wrapper-my-paper-server",
      "registered_at": "2026-07-22T18:00:00Z"
    }
  ]
}
```

## Field definitions

- `version` (number, required): Registry schema version. Current value is `1`.
- `instances` (array, required): Registered wrapper instances.
- `name` (string, required): Human-friendly instance slug/display key.
- `config` (string, required): Absolute path to `wrapper-config.toml`.
- `root` (string, required): Absolute server root path for this instance.
- `server_id` (string, optional): Server identity UUID from wrapper config.
- `service_name` (string, required): OS lifecycle identifier for start/stop.
  - Linux/Windows: `vesta-wrapper-<instance>` (systemd unit / scheduled task name)
  - macOS: `com.vesta.wrapper.<instance>` (launchd label — must match the plist `Label`)
- `registered_at` (string, required): ISO-8601 UTC timestamp for first registration.

## Canonical identity and dedupe

- The canonical key is the absolute `config` path.
- Re-registering an existing `config` path must upsert the same entry, not create a duplicate.
- `name` can change across updates, but `config` is the stable identity.

## Port resolution rule (important)

The registry is identity/discovery only. Ports are **not** registry fields — always read
`http-port` / `control-port` from each instance's `wrapper-config.toml` at runtime.
Installer/CLI tooling must never store port numbers in registry entries (avoids drift).

## Write rules

- Writes should be atomic (write temp file, then replace).
- Unknown top-level keys should be preserved when practical to allow forward-compatible extensions.
