# Dev restart launch preflight

Both reported restart logs contained `host_open_failed: game_invalid_launch`.
The saved launch and durable software state referenced Paper 26.3 build 222 at
`software-installations/instances/srv_0c683a73fe41403082245bb13cdcee90/software/paper.jar`
under the checkout. That file and its parent installation directory were absent.
The reason they disappeared was not established.

Recovered the exact artifact from Paper's official download service. Its SHA-256
matched both the durable software state and current official build metadata:
`f649771a2190214306fb0cfba22c7b2cbac5df6308b1158309593603ed07d488`.
Size: 55,250,599 bytes. No launch document, database, world or config was replaced.

`agent check-config` previously validated only host paths. It now uses shared
read-only launch validation to check the launch document, executable, working
directory and required files before the restart helper stops a running agent.
This does not execute Java, open the state database or prove artifact provenance.

Validation:

- New regression passes for valid launch, absent required JAR and absent executable;
  each check leaves state uncreated.
- Host tests: 27 passed, 1 failed. The simulated v8 database upgrade fixture fails
  with `state_migration_failed`; this change does not alter the `Host::open` path
  used by that test. Baseline reproduction was not performed.
- Updated agent binaries built successfully; real `agent check-config` passed.
- Real development restart succeeded, PID 42256, configured TLS listener on 8443.
  A subsequent independent status request confirmed online/stopped with no game
  generation. Minecraft was not started.
- `git diff --check` passed. Whole-file rustfmt check found unrelated existing
  formatting in hosted resource-pack composition; it was left unchanged.

The recovered artifact is runtime data ignored by Git, not a source addition.


Follow-up CLI verification: the former check-config success fixture omitted both
its launch definition and executable. It now supplies an inert existing native
launch and asserts the definition remains unchanged. Nothing is executed by the
preflight. Full vesta-agent tests pass (77); vesta-dev: 8 passed, 1 manual ignored.
