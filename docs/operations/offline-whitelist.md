# Offline whitelist management

Implemented 2026-10-23. Minecraft must be stopped; the Vesta agent must remain reachable. Running-server whitelist actions continue to use typed Minecraft commands.

`GET /api/v3/manage/players/whitelist/offline` returns `{items, sha256}`. A null hash means the file is absent. It requires `players.view` and scoped file-read authority for `whitelist.json`.

`POST` to the same endpoint takes a device-scoped idempotency key and:

```json
{"player":"Alex","add":true,"expected_sha256":"<current SHA-256, or null for an absent file>"}
```

It requires `players.moderate`, scoped read authority, and scoped file-create or file-overwrite authority as appropriate. Names use the existing Java Edition validation. Removal uses `add:false`. A stale revision refuses with HTTP 412; null never means overwrite anything. Existing receipts replay under current authority even after Minecraft starts; they do not reapply the file effect. Reusing a key with changed input refuses.

Online-mode additions resolve the canonical name/UUID from the fixed Mojang HTTPS profile endpoint. The request has an eight-second deadline, no redirects, bounded headers and a 4-KiB response limit. Failed, malformed or mismatched lookup results refuse the change; no guessed online identity or perpetual lookup/polling is introduced. Vanilla offline-mode additions use Java's MD5-based `OfflinePlayer:<case-sensitive name>` UUID construction. This MD5 use is for Minecraft identity compatibility, not credential security. Forwarding proxies/custom authentication that use online UUIDs despite `online-mode=false` are not established by this setting; this implementation does not certify those identity schemes.

The edit takes the local startup exclusion and, for supervised games, the supervisor's stopped-game maintenance hold. The existing bounded renewal helper maintains that hold during IO. Current process facts and hold continuity are checked before the native replacement and after completion. Lost proof refuses the effect or reports recovery-required uncertainty; it is not a successful verified save. Unknown/stopping/draining/orphan/unresolved processes refuse edits. A concurrent start is refused while the exclusion is held rather than launching against an in-progress edit.

The existing single-file journal provides optimistic concurrency, atomic replacement, recoverable previous bytes, current authorization and auditing. `server.properties` is reread before the effect to detect identity-mode changes during lookup. Files are bounded to 1 MiB / 10,000 entries; duplicate name/UUID identities, duplicate required fields, malformed JSON and invalid entries refuse. Unrelated entries and extension fields are retained. The special file access is limited to the root `whitelist.json`; generic remote file access remains protected.

The launcher enables whitelist add/remove while Minecraft is stopped and the agent is reachable, then reports “Whitelist saved for the next start.” It retains the revision and intent for uncertain retries, including when Minecraft starts meanwhile, and invalidates cached list reads after a saved receipt. It does not silently convert an uncertain offline write into a console command. Bans, OP escalation and proxy MOTD behavior are outside this increment.

Tests cover real HTTPS save/remove/replay/stale-revision/running refusal, online-profile response validation, vanilla offline UUID compatibility, malformed/duplicate entries, extension preservation, the exact protected-file exception, local startup exclusion, actual supervisor hold/start refusal/lost hold, client transport/revision/cache invalidation, and launcher fixture semantics. No live Minecraft session, external Mojang request or cross-platform release certification was run.
