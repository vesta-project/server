# One-command development updates

The normal Windows command is now `./dev.ps1`. It builds the development tools,
updates the existing `dev-server` agent and returns to the terminal. Other
servers are selected with `./dev.ps1 restart <name-or-id>`. `build` and `shell`
remain explicit alternatives. Help is available without compiling anything.

## Findings and fixes

1. The former root entry only opened a shell. Updating still required another
   command, a selected session and sometimes a listener override. The new update
   action supplies the development registry/default server, uses the configured
   listener for an offline agent and preserves a live agent's reported mode.
2. Helper and agent builds previously used different Cargo package sets. Both
   now build vesta-dev, vesta-agent and vesta-setup with the same profile and
   feature unification. The second Cargo read reuses that build.
3. Every invocation previously created another staging directory. Noninteractive
   commands now share a checkout-bound session, serialize updates and reuse a
   complete byte-identical staged build. Comparison uses two bounded 64 KiB
   buffers. Running executables remain outside Cargo's output directory.
4. Restart did not set the existing VESTA_AGENT_STAY_STOPPED override. It now
   suppresses Start/Sleep startup policy for replacement agents, then explicitly
   starts Minecraft only when it was running in the initial observation.
5. The live test found a Windows terminal hang: the new agent was online, but
   the calling PowerShell pipeline remained open until that agent exited.
   Clearing inheritance on only the standard handles did not solve it. The
   detached launch adapter now uses STARTUPINFOEX and a handle allowlist containing
   only the agent log and null input. It still enforces independent job lifetime.
   This follows Microsoft's documented [handle inheritance mechanism](https://learn.microsoft.com/en-us/windows/win32/procthread/inheritance).
   Rust's corresponding [CommandExt controls](https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html#tymethod.inherit_handles)
   are unstable in the pinned toolchain, so this is an isolated Windows adapter.
6. Generic wrapper exceptions repeatedly appended force-kill commentary and hid
   the useful cause. Native failures now preserve their exit status and print
   one diagnostic. Fixed configuration/host codes are surfaced without dumping
   arbitrary logs. Startup failures include the log location. Lost shutdown
   acknowledgements are reconciled through the existing ownership fences.

The update still builds and validates configuration/launch before shutdown,
waits for agent/runtime/supervisor ownership release, preserves recovery evidence
and confirms the replacement PID through protected local management. No process
name sweep, forced termination, global installation or server configuration
rewrite was added. Legacy scratch recovery remains separate from daily updates.

## Verification

- Real `./dev.ps1` updated the configured dev-server and returned successfully.
  The final online-to-online update replaced PID 15524 with PID 22824, retained
  the configured listener and left Minecraft stopped with no game generation.
  The terminal returned while the replacement agent remained online. Separate
  redirected-output and plain script invocations were exercised.
- Repeated runs reused the same staged build directory because all four runtime
  executables were byte-identical. No old build directory was deleted.
- Windows wrapper tests passed: default/named action routing, literal arguments
  including spaces and dollar expressions, no-build help, native failure status,
  clean compiler failure reporting and exact temporary-helper cleanup.
- Dev-tool unit tests: 14 passed; the normally ignored native lifecycle test was
  then run explicitly and passed. Its owned inert processes exercise both
  in-process and separate supervision, running/stopped restoration and offline
  startup with configured on-agent-start=Start. These tests do not launch Java.
- A Windows regression creates an unrelated inheritable pipe and proves the
  detached child does not keep its writer alive. It also verifies environment
  overrides and log redirection in a working directory containing spaces.
- Agent CLI binary suites: 51 tests passed for each of the vesta and vesta-agent
  entry points. No setup/supervisor binary unit tests were present in that run.
- Clippy passed for vesta-dev, vesta-agent and vesta-setup, all targets, no deps,
  with warnings denied. Scoped formatting and diff whitespace checks passed.

Test builds used the existing Cargo target on C: to avoid consuming the limited
space on V:. The scripts continue to honor the user's Cargo target configuration.
No live Minecraft world, identity, keys, startup registration or listener config
was changed. Source changes remain uncommitted alongside earlier work.
