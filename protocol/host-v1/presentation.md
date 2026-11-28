# Server list message and icon

Use GET and PUT `/v1/servers/{serverId}/manage/motd/policy` to read and edit the server list message. GET requires `config.view` and PUT requires `config.edit`. PUT accepts `{expected_revision,document}`. Send a null revision on the first write and the last read revision on subsequent writes. A stale revision returns 409 with reason `motd_policy_conflict`. The stored document limit is 32 KiB. POST `/v1/servers/{serverId}/manage/motd/preview` renders the supplied state and document under `config.view`.

Preview renders the requested state using the saved server name and the configured pack and loader metadata. It does not ping Minecraft. Actual player counts and transition details are not included in a preview.

The temporary Minecraft-port responder serves Sleeping and Waking when offline status is enabled and the game port is free. Online, Offline, Starting, Stopping, Crash Loop and Recovery Required are available to the HTTP status presentation. Maintenance is a preview option without a runtime transition. Minecraft serves the Running text from `server.properties` after it starts. The policy reports `live_apply: "separate_server_properties_edit_required"` for this text.

## Server icon

PUT `/v1/servers/{serverId}/manage/server/icon` uploads PNG branding. The same write stages a 64 by 64 PNG for Minecraft. Vesta Server or the Supervisor applies it to the game-root `server-icon.png` while stopped and before launch, including an automatic crash restart. Minecraft reads the file on startup.

An explicit branding upload preserves an existing user-supplied icon as `server-icon.vesta-backup.png` before replacing it. If that name is taken, Vesta adds a unique suffix. A subsequently added user icon takes precedence until the next upload. Clearing the branding icon removes only a file still owned by the projection. Backups are retained.

Read `/manage/server/game-icon` first and use `/manage/server/icon` when it has no user-supplied file. Both reads require `status.view`. The game-icon route returns a 64 by 64 PNG up to 256 KiB with an SHA-256 ETag and supports `If-None-Match`. Host inventory exposes `gameIconEtag` and `iconEtag`. `icon_source` and `iconSource` are retained for compatibility with older launchers.

The temporary responder uses the same icon when suitable for its 28 KiB JSON budget, with any configured state-specific icon taking precedence. A legacy `document.normal_icon_png` is supported until the branding icon is set or cleared.
