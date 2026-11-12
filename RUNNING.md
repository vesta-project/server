# Running the Rust server agent

For development, [`./dev.ps1`](docs/operations/dev-session.md) builds the tools,
creates a missing default `dev-server` or updates its agent, then returns to your terminal.
Server data lives in the Git-ignored `.dev/servers` directory in this checkout;
only build staging uses Temp. Minecraft's
running/stopped state is preserved. Use `./dev.ps1 restart <name>` for another
server or `./dev.ps1 shell` for an optional shell with the tools on PATH.

Build the agent and supervisor from this repository:

```powershell
cargo build --locked -p vesta-agent
cargo run --locked -p vesta-agent --bin vesta-agent -- --help
```

Use CLI help for setup, configuration, local operator and serve commands. The
[guided installer](docs/operations/host-setup.md) prepares a host for launcher
onboarding. [Signed release bundles](docs/operations/bootstrap-release.md) install
the four native tools; [update commands](docs/operations/signed-updates.md) verify
and stage later releases without activating them.

## Process ownership

`vesta` and `vesta-agent` currently compile the same command implementation under
two executable names. A management invocation is a short-lived local client (or
the interactive `manage` terminal). An invocation of `agent serve <host.toml>` is
the long-running agent for that server: launcher HTTPS, authentication, state,
tasks, console streaming, sleep and other management logic live there. Exiting a
management terminal does not stop the agent.

Minecraft always runs in its own Java process. In the default `in-process`
supervision mode, the agent owns the Java process and its stdin/stdout handling;
the words refer to the supervisor logic being in the agent, not Minecraft being
inside the agent process. Game commands can still wait for Minecraft's main
thread even while the agent's network listener is responsive. CPU/RAM contention
on the host can affect both processes.

With `[process] supervision = "separate"` in `host.toml`, there is one additional
`vesta-supervisor` process per server. The agent starts or attaches to it. It owns
Java, drains output, writes console commands, and records process identity. It has
protected same-account local IPC, not a public network listener. Agent restart
can reattach to the same game generation; it does not restart Minecraft. The
supervisor can also enforce the configured opt-in crash restart policy. Rebuilding
on disk does not upgrade a supervisor that is already running. See the
[supervisor contract](docs/operations/supervisor.md) for recovery and ownership
rules. Separate supervision is opt-in, not the current default.

`vesta-setup` is the installer/onboarding tool. Before activation its `host`
command runs the small pending setup endpoint; after activation it starts the
installed agent. That pending endpoint is distinct from the normal agent and its
management API. The setup tool is not a second Minecraft supervisor.

Operational contracts:

- [Local operator](docs/operations/local-operator-design.md)
- [Supervisor](docs/operations/supervisor.md)
- [Management API](docs/operations/management-api-contract.md)
- [Sleep and MOTD](docs/operations/sleep-and-motd.md)

Optional Minecraft connectors and archived Java wrapper sources belong to
[vesta-project/server-java](https://github.com/vesta-project/server-java). They
are independent of the native agent build.
