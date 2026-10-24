# Development rebuild with coordinated restart

Implementation checkpoint: `3e60218`. This extends the temporary development
shell only, not the signed update/activation workflow.

`vesta-rebuild --restart <id|name>` stages a complete successful build first,
then restarts exactly one registered server. It uses protected local management,
graceful shutdown, agent/runtime/supervisor ownership locks, unchanged host/launch
documents and preserved recovery markers. The replacement PID is checked over
local IPC before optionally starting Minecraft. A previously stopped game stays
stopped. Uncertain effects are not retried and no process is force-killed.

Current agents report their PID, listener mode and working directory in local
JSON status. Older agents require an explicit listener choice before shutdown;
`--configured-listener` is the one-time choice for the existing TLS demo.
Human status output remains unchanged. The first use requires a fresh `cargo dev`
session so its helper and shell function include the new option.

Validation with Rust 1.96.0:

- Windows and Ubuntu: eight development-tool tests pass; one opt-in live test is
  ignored in the default suite. Tests cover staging failure, explicit scope,
  ambiguous/malformed registry identities, stable game observations, listener
  preservation, live ownership locks and retained recovery evidence.
- Ubuntu 24.04 WSL: the opt-in real lifecycle test passes all four combinations
  of running/stopped game and in-process/separate supervision. It verifies new
  agent PIDs, new running generations, preserved local-only mode and orderly
  cleanup. The game is an owned inert test executable, not Minecraft.
- Both systems: protected control-channel regression passes, including status
  metadata, lifecycle control and shutdown. Scoped all-target strict Clippy and
  formatting pass.
- Actual PowerShell and Bash shell checks pass argument forwarding and failure
  behavior. PowerShell now updates PATH consistently even when a native rebuild/
  restart command returns an error under the shell's strict error preference.
- Windows live detached-agent test is blocked by the tool job's independent
  process creation restriction (OS error 5). No containment safeguard was relaxed.

Evidence: `V:\tmp\dev-restart-*.log` and
`~/.local/share/vesta-linux-test-20261024/logs/dev-restart-*.log`.
Initial feature validation left the existing demo untouched.

## Legacy demo migration follow-up

The still-running 23 October agent (PID 4388) predated the shutdown RPC added in
`6cec785`. A new helper therefore reached it successfully but received an invalid
request response; rebuilding again could not add a handler to the old process.
The client now distinguishes unsupported requests from refusal/unknown delivery.
The helper also checks independent launch capability before stopping a host, and
can bring up only the agent after an explicit listener choice and positive
quiescence/recovery checks. The WSL live test now covers this offline recovery.

For this one-time migration, live status proved Minecraft stopped. A temporary
Windows helper pinned the exact process image, checked that its console contained
only that agent and the interrupt helper, then delivered ordinary Ctrl+C. The
agent exited with code 0. No force-kill or recovery-marker removal was performed.
The rebuilt staged agent came online as PID 7692 on port 8443, preserving game
data and the stopped state. The authenticated API requires a session as expected;
the TLS probe used the existing local CA rather than disabling verification.
