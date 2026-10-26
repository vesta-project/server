# Player join automations

`player_first_joined` means a player's first recorded visit to this server. The event includes `player` and `uuid`; the trigger has no parameters. Visit history is server-scoped and survives agent restarts, name changes, world replacements, and automation run-history cleanup. Enabling a workflow later does not greet players already recorded.

The host observes its existing local console capture. It sends no player-list commands, queries no identity service, and creates no extra network polling or background thread. UUID evidence must be an anchored server INFO message or the vanilla authenticator-thread UUID message. The corresponding join must arrive within 120 seconds of that evidence. Chat, warning lines, malformed UUIDs, truncated lines and username guesses do not establish identity.

Before processing joins for a Minecraft run, existing UUID filenames in the selected world's `playerdata` and `stats` folders seed durable visit history. No player-data contents are loaded. Work is incremental: at most 128 directory entries per host pulse, 200,000 examined entries, 100,000 historical UUIDs, and 128 imported IDs per database transaction. Linked, unreadable or unsupported world paths suppress first-visit actions instead of classifying old visitors as new. The history table is service-owned and initialized transactionally when Automation opens.

Console processing is bounded to four pages of 128 lines per pulse. The temporary identity cache holds at most 4,096 names, while observed online membership holds at most 10,000 names. A gap or missing/stale UUID evidence pauses first-visit classification for the Minecraft run. Known UUIDs may still be recorded conservatively. The next run rebuilds its historical baseline. Such uncertainty produces an operator notification; it never invents a first visit.

Recording a new UUID and admitting all matching workflow runs share one transaction. Multiple new players queue separately, up to the existing ten pending runs per workflow. Capacity or persistence failures leave the observation cursor in place for retry; a subsequent console-retention gap suppresses ambiguous events. Visit identities are never evicted to make a returning player appear new; the durable history bound is 100,000 UUIDs.

The separate `first_player_joined` trigger means an empty server becoming occupied. `player_count_zero` means the last observed player leaving. Duplicate login/join and disconnect/leave messages are deduplicated by membership. When attaching to a running game, occupancy is initially unknown; a cached authoritative empty roster can re-arm it without executing a query. Lost continuity suppresses occupancy transitions until a new run.

The launcher offers "When a player joins for the first time" and the "Welcome new players" template. Run history identifies the player. Actions retain their existing literal-input behavior; this does not introduce command placeholder substitution.

Tests use disposable worlds, SQLite and synthetic console messages. They cover historical players, duplicates, renames, reconnects, restart persistence, stale and absent identity evidence, gaps, bounded scans, occupancy transitions and transactional queue pressure. No live Minecraft session was restarted for this change.
