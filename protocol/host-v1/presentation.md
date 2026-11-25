# Server list message and icon

Use GET and PUT `/v1/servers/{serverId}/manage/motd/policy` to read and edit the server list message. GET requires `config.view` and PUT requires `config.edit`. PUT accepts `{expected_revision,document}`. Send a null revision on the first write and the last read revision on subsequent writes. A stale revision returns 409 with reason `motd_policy_conflict`. The stored document limit is 32 KiB. POST `/v1/servers/{serverId}/manage/motd/preview` renders the supplied state and document under `config.view`.

Send the normal server icon in `document.normal_icon_png` as standard base64 without a data URI prefix. The asset must be a 64 × 64, non-interlaced, 8-bit RGB or RGBA PNG. Vesta checks and re-encodes the image to strip metadata. The normalized PNG budget is 17 KiB, enough for 64 × 64 RGBA pixels plus lossless PNG and zlib overhead. The temporary Java status responder has a 28 KiB outgoing JSON budget. It sends the icon as a base64 data URI in `favicon`.

Temporary status and previews choose a configured state-specific `icon_png` first, then `normal_icon_png` if the state has no icon. When the selected icon is absent, invalid, or over budget, they show the generated Vesta icon.

The Agent or Supervisor applies a valid normal icon to `server-icon.png` at the game root while stopped and before starting Minecraft, including an automatic crash restart. The icon takes effect in Minecraft on startup. State-specific icons apply only to Vesta presentation. A user-provided file at the game root takes precedence. Clearing the normal icon removes a file only when its identity and contents still match the Vesta-owned file. A protected or unavailable path defers the file update. Dedicated server pack imports skip this server-managed path.

The existing `live_apply` value, `separate_server_properties_edit_required`, describes the live MOTD text. The icon is separate from the live MOTD and from the launcher branding asset at `/v1/servers/{serverId}/manage/server/icon`.
