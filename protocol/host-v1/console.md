# Console history

When `GET /v1/host` advertises `console-history`, call
`GET /v1/servers/{serverId}/manage/console` (API v3 `GET /api/v3/manage/console`)
with the existing `console.read` permission. An omitted cursor returns the
retained tail. `?before=<opaque>&limit=50` returns up to that many older lines,
strictly before the cursor, in chronological order. The limit is 1 to 200.
Use `cursor` for forward polling and `before` for history, one at a time.

The API v3 management envelope contains
`data: {items:[{cursor,text,truncated}],next_cursor,latest_cursor,previous_cursor}`.
Use `previous_cursor` for the next older page. Null means no older lines were
found. Keep the forward cursor from the live view while paging back. History
cursors for disk lines are valid only as `before` values. A cursor whose retained
line was evicted or whose file was replaced or removed returns
`409 cursor_gap`. Reload the tail to start a fresh history view.

History uses the retained lines first and then standard Minecraft logs. When
the oldest retained line matches a line in `logs/latest.log`, earlier lines
in that file are eligible. Without a match, the server uses rotated archives
from days before the oldest captured line. With no retained lines, begin with
the latest log's complete lines. Archives use the default Minecraft
`logs/YYYY-MM-DD-N.log.gz` names and are served newest first by file, with
chronological order within each page. Missing, unreadable or unsupported logs
can limit available history. Every returned line uses the existing console
redaction. Console command and WebSocket contracts are unchanged.
