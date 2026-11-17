# Shutdown implementation validation

Source: working tree in `V:/hm/server`, `feat/host-model`, based on commit
`13559a3`. No new commit was created: the configured GPG signer failed to connect
to its agent. Signing was not disabled. The tested changes remain uncommitted.

Checks used disposable data and child processes, never an installed host/profile.
Linux checks used a source snapshot under the existing Linux build target, with a
read-only reference to the pinned launcher source. The original Linux Git checkout
was not reset or modified. Native Windows checks used a disposable source copy and
`CARGO_TARGET_DIR=V:/hm/server-target`; dependency manifests and lockfiles did not
change. Temporary Windows source copies were removed after checks completed.

| Check | Result |
| --- | --- |
| Linux `cargo check -p vesta-agent -p vesta-setup -p vesta-supervisor` | Passed |
| Linux control library, filter `shutdown` | 24 passed, including real SIGTERM/SIGINT, deadlines, interrupted jobs and safe publication |
| Linux control library, filter `system_shutdown`, after endpoint-drain refinement | 6 passed, including advancing native stop while state stays open for in-flight requests |
| Linux control interrupted-backup publication regression | 1 passed |
| Linux supervisor filter `shutdown` | 1 library test and 1 integration test passed; exact final receipt, stale/mismatched/unknown refusal |
| Linux supervisor ordinary agent-disconnect/handoff regression | 1 passed |
| Linux setup filter `linux_unit` | 2 passed |
| Linux setup filter `server_startup` | 6 passed |
| Linux ignored `linux_generated_startup_unit_parser` | 1 passed; systemd-analyze syntax validation only, no unit registration/start |
| Linux agent `--test host_shutdown` | 1 passed; real disposable host exits cleanly after both SIGTERM and SIGINT, state reopens |
| Linux setup-wrapper exact-child signal forwarding regression | 1 passed; both TERM and INT forwarded to owned children |
| Native Windows `cargo check --locked --tests -p vesta-agent -p vesta-setup -p vesta-supervisor` | Passed |
| Native Windows control library filter `shutdown_signal` | 4 passed, including real hidden-window registration and simulated query/cancel/confirmed callback behavior |
| Native Windows control library filter `process_shutdown` | 4 passed, including deadline enforcement with blocked stderr |
| `git diff --check` | Passed |

The Linux shell-template check initially exposed CRLF line endings; the script was
normalized to LF and the affected checks passed. The initial library-test compile
also exposed two borrowed-by-value test inputs, which were corrected. An existing
unused `tls::Material::config` warning remains unchanged.

Not verified: a real Windows logout/restart, a real Linux systemd reboot/service
stop against installed data, or installed-unit migration. No service, live install,
real game data or launcher profile was changed. Full test suites were not run.
Windows's two-second cleanup budget is bounded best effort: a stalled/large save
may require unclean recovery; the implementation does not claim it completed.
