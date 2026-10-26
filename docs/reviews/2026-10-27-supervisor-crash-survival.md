# Preserve Minecraft through a Windows supervisor crash

The full workspace regression run found that
`agent_exit_keeps_the_game_and_a_supervisor_crash_blocks_duplicates` failed on
Windows. It failed again in isolation, at the assertion that the recorded game
remains alive after terminating its supervisor.

The shared native process tree used `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` for every
owner. That behavior is correct for installer jobs and ordinary in-process game
ownership. It conflicts with the durable supervisor's deliberate preservation of
the game: Windows closes the supervisor's native job handle on abrupt exit even
when Rust destructors are bypassed.

`GameProcess::new_surviving_owner_exit` now selects a Windows job without the
kill-on-close flag. The supervisor is the only production caller. It still owns
the job for explicit stop/kill and persists the existing generation record before
reporting a successful start. The usual Rust Drop path still kills/reaps a child;
the supervisor's existing preservation wrapper continues to avoid that Drop for a
live game. Unix process groups already survive owner exit.

Installer and ordinary game-process defaults retain kill-on-owner-exit. There is
no new remotely selectable process policy or PID-based termination path.

Validation: the existing real supervisor/inert-game end-to-end test now passes
(3/3 in its suite). Process lifecycle, ownership and adversarial tests also pass
in the subsequent workspace run. No live agent or Minecraft process was changed.
Full logs: `V:/tmp/buildtools-workspace-tests-final-20261026.log`; failing isolated
baseline: `V:/tmp/buildtools-supervisor-recheck-20261026.log`.
