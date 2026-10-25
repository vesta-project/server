# Shared player activity log formats

The shared activity parser now accepts the anchored INFO message
`Name[/address:port] logged in with entity id N at ([world]x, y, z)` in addition
to joined/left-the-game lines. It also accepts `Name lost connection: reason`.
There is no server software detection or Paper-only branch. Both existing INFO
envelopes are supported, including the compact Bukkit-family timestamp form.

Login parsing validates the player name, IPv4/IPv6 socket address, integer entity
ID and three finite coordinates. World labels are optional and bounded. Partial
messages, chat prefixes, wrong levels and trailing data are refused. Only names
and event kinds enter metrics; this adds no address/coordinate persistence.

The existing roster set naturally deduplicates login + join and disconnect +
leave pairs. Metrics now keep a bounded transition map, reset at process
generation changes and console gaps, so the same pairs do not inflate event
totals. No new console commands or polling are introduced.

Validation: the exact supplied login line, alternate INFO envelope, IPv6,
malformed/chat cases, duplicate transitions, reconnects and memory cap are
covered. The roster integration fixture asserts immediate count changes from
login/disconnect alone, duplicate messages, and the unchanged single initial
`list` command. No live agent or game restart is performed.
