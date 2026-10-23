# Offline management and startup status: proposed next increment

2026-10-23. Design follow-up. Existing requirements still define temporary port ownership and explicitly exclude an always-on proxy from the baseline.

Update: offline whitelist editing is implemented; see [the API and safety contract](offline-whitelist.md). The startup proxy/MOTD options below remain proposals.

## Offline whitelist management

Use the same structured whitelist action in the launcher with explicit execution modes:

- Running: current generation-bound, permission-checked Minecraft command. Receipt means stdin delivery; independently read persisted membership without claiming it proves every plugin's in-memory state.
- Fully stopped, agent reachable: structured whitelist JSON edit under the process owner's startup exclusion. Coordinate with the separate supervisor too; an agent-local lock alone cannot block supervisor restarts. Unknown ownership, stopping or recovery-required states refuse the edit rather than guessing.
- Agent unreachable: no applied edit; preserve draft locally without claiming acceptance.

Resolve/validate identity before holding the startup gate. Online-mode additions need a trusted UUID/name mapping; offline-mode identities must use the server's configured identity rules, never silently substitute online identities. Removal should identify the existing UUID entry. Avoid an independent general-purpose identity cache without a freshness policy.

Reuse bounded parsing, current authorization, optimistic file revision, audit/idempotency and recoverable atomic file replacement. Preserve unrelated entries; refuse malformed/ambiguous input without overwriting it. Do not silently defer a running command into an offline edit after a timeout. UI distinguishes command delivered from file saved for next start.

Acceptance: running/stopped, supervised/local, concurrent start or auto-restart, stale revision, duplicate identity, failed identity lookup, corrupt file, write failure, repeated intent and uncertain process ownership. Begin with whitelist; evaluate bans/ops separately because OP escalation and expiry introduce additional semantics.

## Startup MOTD limitation

Current `Game::start_unblocked` closes and joins the temporary responder before launching Minecraft. The equivalent supervisor path owns the same handoff. The game port can therefore be unavailable during startup, and Minecraft owns status when it binds. HTTPS lifecycle presentation remains available. A configured starting template does not guarantee a public game-port response throughout startup.

Fake successful port checks do not solve listener ownership. Two independent listeners sharing an address do not provide deterministic routing between status and login traffic. Do not weaken bind/readiness proof or reuse-port options to conceal the conflict.

Options for an explicit future architecture decision:

1. Keep temporary ownership, truthfully describe the startup gap and use a compatible connector for dynamic status once its hooks are available. A connector cannot promise status before Minecraft opens its listener/hooks.
2. Opt-in persistent public listener, Minecraft on a separate private backend endpoint. Answer bounded status handshakes from canonical lifecycle state; reject starting logins with a reconnect message; relay supported login/game streams once ready with bounded buffering and backpressure. This requires compatibility/security design for original source addresses, IP bans/rate limits, modded handshakes, DNS/IPv6/tunnels and supervisor ownership. Raw TCP relay must preserve authentication bytes; a terminating proxy has different authentication/forwarding requirements. Neither should be silently substituted for the other.

A persistent listener is a deliberate networking change: private backend exposure, port configuration and an always-present game-traffic dependency. It needs owner approval before implementation, load/compatibility measurements and failure-path tests. Do not automatically introduce a Java proxy or claim zero overhead on 1–2-core / 2–4-GB hosts.

Recommendation: implement offline whitelist first; choose persistent-listener mode only if uninterrupted public startup status is a firm product requirement. Retain the existing direct mode as a supported low-overhead option.
