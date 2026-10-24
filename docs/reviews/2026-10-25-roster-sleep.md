# Roster command and sleep investigation

The original dev instance was already stopped, with no agent or Java process,
when this investigation began. Its latest.log contained 14,948 list responses
and a clean Minecraft stop at 20:03:29. The saved sleep policy was revision 3,
enabled, with 60 idle seconds and a 30-second warning. There were no unfinished
operations. No original game files, database rows or recovery markers were changed.

## Issuer and failure

The original dev binaries were dated 23 October 11:36, before the 14:06
`ea5d5f3` roster/parser/pacing changes already present in source. The older source
(`7b8c5a3`) called `query_management_players` from sleep, rejected the modern
`System chat:` list envelope, and cleared `sampled` inside `Coordinator::reset`.
Thus each two-second query timeout removed the nominal ten-second pacing gate;
the next 250 ms coordinator tick issued another list. It never established a
zero-player evidence window. This explains both the 2–3 second cadence and the
failure to warn/sleep after the saved idle period.

The staged restart helper refused the original instance's
`operator-session-pending` marker (`shutdown-not-proven`). The marker was preserved.
Reproduction therefore used a disposable copy of that instance's game, software
and state at `%TEMP%/vesta-roster-repro-20261025`, on game port 25586 and TLS 8444.
The copy's original software-activation binding was removed from the copy only,
because its original absolute launch paths correctly caused a maintenance hold.
The original instance was never started.

Temporary traces recorded query backtraces, unrecognized response cursors and
sleep-query errors. Two temporary compatibility branches restored the older
parser and sample-reset behavior; they were used only for reproduction and were
removed afterward. With no launcher connected and only start/status local
requests, the sleep coordinator reproduced list responses at 08:04:27, :30, :32,
:34, :37 and :39. Every wrapped list line was unrecognized, followed by
`SLEEP_TRACE roster_failed=players_query_timeout`. Native trace symbols in the
staged copy were unresolved; the explicit sleep failure trace and isolated caller
setup identify the issuer independently of those symbols.

Metrics has no list-write path. HTTP players reads now use the shared roster.
The launcher store subscribes to the native roster owner instead of polling the
players route. A separate native worker bug did allow older agents' null status
count to alternate with the worker's synthesized count and retrigger reads; a
successful unknown roster also did not latch its triggering status. The worker
now remembers the authoritative triggering generation/count/revision for either
result, and leaves status publication to the status owner. Transport failures
retain bounded retry backoff.

## Current-source corrections

The previous roster cache retained only successful seeds: a failed seed could
become repeated list writes through subsequent HTTP/sleep reads, and any roster
advance error was treated as permission to reseed. Failed seeds are now retained
per generation. Further reads scan for a late response without writing. Status
observation also completes late seeds passively, so an unknown roster need not
cause periodic players-route reads merely to discover new evidence. Only an
actual console gap permits a reseed; transient/unavailable observations remain
unknown. Cursor holes at the console tail and between lines are recognized.

The original log also records the real player's join at 10:24:16 and leave at
10:28:32 as server INFO `System chat:` messages. The current activity parser did
not strip that wrapper, although the list parser already did. This lost both
presence transitions. The activity parser now accepts that anchored envelope and
still rejects chat impersonation and other threads.

Sleep policy and authenticated status now expose `sleep_blocker`, shared through
the client, native snapshot/IPC types and fixtures. Reasons and compatibility are
documented in `docs/operations/sleep-and-motd.md`. The diagnostic follows actual
coordinator branches, including readiness, execution authority, players, holds,
operations, idle countdown and acknowledged warning countdown. Policy revisions
invalidate old diagnostics. Lifecycle/state representation and UI pages are unchanged.

## Real Minecraft verification

On the same disposable Minecraft 26.3 copy with the fixed behavior and the saved
60/30 policy, one list was emitted at 08:11:01. Minecraft's own
`Server empty for 60 seconds, pausing` line at 08:11:53 conservatively reset the
evidence window once. The warning was emitted at 08:13:03, followed by the normal
stop/save sequence at 08:13:33. Temporary diagnostics progressed through
`counting_down`, `warning_sent`, `operation_in_progress` and `not_ready`.
There was exactly one list in the entire fixed generation. The copy's agent was
then shut down normally. All temporary production tracing/compatibility code was
removed; clean commands were staged afterward.

No real Minecraft client was joined during verification. Tests inject the same
anchored modern console format into the owned runtime capture: empty → join →
one online → leave → empty; equal-count name replacements; a timed-out seed with
repeated reads; late-response recovery; and one reseed after a real gap. These
tests assert the exact list-write count. Sleep tests cover warning timing, joins,
unknown counts and holds.

## Validation

Agent validation passed 124 game-runtime tests, 22 player-query tests, nine sleep
tests, the activity-envelope regression and the sleep-policy HTTP regression.
The final passive late-seed refinement passed both roster regressions and all nine
sleep tests again. Relevant scoped Clippy with
`-D warnings` passed. The launcher client HTTPS/presence/management/policy tests
passed; client all-target Clippy with `-D warnings` passed. Native server tests
passed 30 with one pre-existing ignored live fixture, using the documented
Common Controls v6 manifest in the generated test executable.

Desktop TypeScript checking passed. The requested full Bun selection ran 219
tests: 218 passed; the unchanged console batch reactivity check failed its 8 ms
`HOT_SCOPE_TIME` budget, including on an isolated rerun. No diagnostics were
suppressed and no console/UI implementation was changed for this task.

Full launcher native all-target Clippy was attempted but remains blocked by
existing warnings in unrelated files (including the Unix-only `tokio::task`
import in jre_manager.rs). Temporarily platform-scoping the unused items exposed
33 library / 39 test-target lint errors in the existing launcher code, including
argument counts, checked divisions, collapsible conditions and test-module order.
Those unrelated temporary edits were reverted. A broader client unit run also
hit the existing staging TTL test's `Instant::now() - one_hour` underflow on this
short-uptime Windows host; the relevant client integration selections passed.

Parallel native fixture tests initially exceeded their process-start deadline;
the complete 22-player-test selection passed with two test threads. The workspace
disk also filled during compilation. Generated test symbols and the failed
control incremental cache were preserved under
`%TEMP%/vesta-roster-validation-symbols-20261025`, and final agent compilation used
`CARGO_INCREMENTAL=0`. Reproduction logs and helper scripts remain in the private
reproduction directory; it contains copied server state and is not a publishable
artifact.
