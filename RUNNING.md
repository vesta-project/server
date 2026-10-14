# Running the Rust server agent

Build the agent and supervisor from this repository:

```powershell
cargo build --locked -p vesta-agent
cargo run --locked -p vesta-agent --bin vesta-agent -- --help
```

Use CLI help for the implemented setup, configuration, local operator, and serve
commands. The hosting setup flow and production deployment packaging remain
incomplete; former Java one-liner installers are not Rust agent installers.

Operational contracts:

- [Local operator](docs/operations/local-operator-design.md)
- [Supervisor](docs/operations/supervisor.md)
- [Management API](docs/operations/management-api-contract.md)
- [Sleep and MOTD](docs/operations/sleep-and-motd.md)

The Java wrapper and its running instructions moved to
[vesta-project/server-java](https://github.com/vesta-project/server-java).
