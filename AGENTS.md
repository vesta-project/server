# Vesta Server

Self-hosting for Minecraft. Vesta Server runs on a Host and manages Minecraft servers there; users control it remotely from Vesta Launcher.
Never compromise on: users' worlds and credentials, safe recovery after crashes and restarts, signed updates.

## How it fits together
- **Agent** (`apps/vesta-agent`): the Vesta process on the Host. It owns config, state, permissions and the API the launcher talks to.
- **Supervisor** (`crates/vesta-supervisor`): starts, watches and restarts Minecraft server processes, independently of the Agent, so games survive Agent restarts.
- **Minecraft servers**: the managed game servers. Each has its own config, software and roles.
- Long work (create, install, update, backup, restore) runs as durable operations that survive restarts and report progress.
- The launcher connects as a linked device, over the contract in `protocol/`.

## Where things are
- `apps/vesta-agent`: Agent. `crates/vesta-supervisor`: Supervisor. `crates/*`: shared logic (e.g. modpack-engine). `protocol/`: launcher contract. `scripts/`: Node contract tests.
- `docs/operations`: current. `docs/archive` and `docs/*.json` evidence: history. Don't treat them as current.

## Commands (full gates in TESTING.md)
- `cargo test -p <crate> --locked -- --test-threads=1` (process tests are timing-sensitive; run serially).
- `cargo clippy -p <crate> --all-targets --locked -- -D warnings`
- `node --test scripts/<file>.test.mjs` (Node 22 or later).

## Ways to hurt yourself
- Never touch `.dev/servers/**` or a dev install unless the task names it.
- Never accept the Minecraft EULA in code or tests.
- Never print `.env` or signing keys.
- Rebuilt binaries don't replace a running Agent. After a restart, report the binary hash and process.
- Anything that installs software must record what it installed.

## Hit every surface
- Every operation: cancel, failure, restart part-way, and a reconnecting launcher.
- Permissions: Host owner, server admin, player and public. A denied optional capability only disables its own feature.
- Resources: stopped servers use no memory. Check what's actually free at start time.
- Windows and Linux both matter (CI runs both).

## Commits
Signed, small, local. No pushes or PRs without approval.

## Glossary
- **Vesta Server**: the product.
- **Host**: a machine running Vesta Server. Owns devices.
- **Agent / Supervisor**: the control process / the process watcher.
- **Minecraft server**: one managed game server.
- **Software**: loader and version.
- **Operation**: durable long-running work.
- **Device**: a linked launcher.
- **Packs**: server resources, client pack, provider server pack and Vesta pack are distinct.
- **Backup lineage**: the chain of backups behind a restore.
