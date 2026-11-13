# Install Vesta Server on Linux

Run on Debian/Ubuntu with systemd. Install `curl`, `python3`, `iproute2`, and `acl` using your distribution's package manager. Root privileges are required for the installation and privileged local commands.

```bash
curl -fsSL https://raw.githubusercontent.com/vesta-project/server/feat/host-model/scripts/bootstrap/install.sh -o /tmp/vesta-install.sh && sudo bash /tmp/vesta-install.sh --channel nightly
```

The branch URL is a development entry point until this work ships. A published signed package must include the new host runtime; older nightlies contain the old onboarding runtime. The bootstrap retains pinned signature/hash verification for downloaded releases. It installs root-owned executables under `/usr/local/lib/vesta`, puts `vesta` on PATH in `/usr/local/bin`, creates the non-login `vesta` account and stores data in `/var/lib/vesta`. It enables the **system** `vesta.service` for boot, starts `vesta-agent host <host.toml>`, binds IPv4 LAN port 25500 and advertises `_vesta._tcp` on the non-loopback interfaces that had a global IPv4 address at install time. Reconfigure `discovery-interfaces` in `host.toml` if network interface selection changes.

The installer prints the setup code obtained from the privileged local socket. On your PC open the launcher, Servers -> Set up a Vesta Server. To rotate an unclaimed machine's code: `sudo vesta setup-code`. Setup codes are not logged to journald. After claiming, use the linked launcher rather than issuing another setup code.

## Options

- `--port 25500`: management TCP port.
- `--root /var/lib/vesta`: absolute data directory (letters, digits, slash, dash and underscore). One system installation per machine.
- `--server-dir '/home/amp/.../Minecraft'`: add this exact permitted import root, grant read ACLs, and record it in `suggested-import.json` for preselection. The launcher preselection consumer is still pending. Does not import, modify or start the source.
- `--local-only`: loopback binding, no LAN advertisement or firewall changes.
- `--no-start`: enable boot service without starting now.
- `--no-service`: install files and account without enabling/starting a service. Run the printed command manually as `vesta`.
- `--answers answers.json [--yes]`: JSON keys `root`, `port`, `server_dir`, `local_only`, `no_start`, `no_service`; CLI overrides values. No interactive wizard is required on Linux.
- `--tag RELEASE` or `--channel stable|nightly`: use a signed release. Explicit bundle URL/directory and verification pins are also supported.

Rerunning replaces binaries atomically and preserves host.toml, TLS identity, host database, linked devices, imports and game data. Existing configuration wins: a rerun with a different `--port`/`--local-only` prints a note and keeps the current value; edit the root-owned `host.toml` and restart instead. Keep the same root on upgrades. Interrupted installs can rerun; incomplete TLS pairs fail rather than rotating identity. Back up your data before upgrading. The installer and service require no interactive user login.

## Import permission

```bash
sudo vesta import-root add '/home/amp/.ampdata/instances/Survival/Minecraft'
# Custom installation:
sudo vesta import-root add '/path/to/source' --root /srv/vesta
```

Without `--root` it uses the installed data directory. The command first prints the exact directory and ACL scope, asks for confirmation on a terminal (`--yes` skips it; `--server-dir` during install counts as confirmation), refuses directories overlapping the Vesta Server data or program directories, grants recursive `u:vesta:rX` on that directory and traverse-only `u:vesta:--x` on its parents, records it in the root-owned `import-roots.json`, then restarts the service to load the catalogue. It grants no write permission and changes no ownership. Choose the smallest Minecraft instance directory. `setfacl` must be installed. Files created in the source after the grant do not inherit it; rerun the same command (it is idempotent) right before importing. Source process inspection can still need a stopped read-only export; read ACLs do not grant permission to inspect another user's running processes. Stop the AMP instance and disable its auto-restart before importing. Permission-remediation presentation in `machine/imports` is maintained by the concurrent import package.

The initial service and game processes share one account, so mods/plugins can affect host state and other games. Separate host/worker/game accounts and credentials remain H11 hardening. Root-owned executables, unit and configuration cannot be modified by that account. The sandbox permits reading home-directory imports and Java JIT; it does not use `ProtectHome=true` or `MemoryDenyWriteExecute`.

## Firewall

If active, UFW gets allow rules for the chosen management TCP port and mDNS UDP 5353, each limited to the directly attached IPv4 networks (`from 192.168.1.0/24`, for example). Active firewalld gets the management port and mDNS service in its default zone, runtime and persistent; the zone is the scope there. `--local-only` changes no firewall. No firewall is disabled. Review that zone/network scope before exposing a public host. Game-port automation is deferred: explicitly allow the game TCP port chosen during creation/import (usually 25565). Existing rules are retained during uninstall to avoid removing administrator-managed rules.

## Development package (unsigned)

Build inside Linux/WSL, not on `/mnt/v`:

```bash
# In a clone on the Linux filesystem (for example ~/hm/server, with ../launcher beside it):
bash scripts/dev/package-linux.sh ~/vesta-linux-dev
scp ~/vesta-linux-dev.tar.gz people-server:
# On the server:
tar -xzf ~/vesta-linux-dev.tar.gz -C ~
sudo bash ~/vesta-linux-dev/install.sh --from-dir ~/vesta-linux-dev
```

This path prints **UNVERIFIED DEVELOPMENT** and intentionally bypasses release signatures. Only use binaries you built/trust. The script builds release versions of vesta, vesta-agent, vesta-setup and vesta-supervisor and produces a directory (with `DEVELOPMENT.txt` recording the commit) and a tarball containing that directory. It uses `CARGO_TARGET_DIR` if set, otherwise `./target`. All other options (`--port`, `--server-dir`, `--local-only`, `--no-start`, `--no-service`, `--answers`, `--root`) work with `--from-dir`.

## Operations and uninstall

```bash
systemctl status vesta
sudo systemctl restart vesta
sudo journalctl -u vesta
sudo vesta uninstall --keep-data
# Or permanently remove this installation's data:
sudo vesta uninstall --remove-data
# For a custom data directory add --root /srv/vesta
```

Uninstall stops/disables the system unit and removes binaries and PATH wrapper. Data removal requires an explicit choice and checks the recorded installation directory. The service account, source read ACLs and firewall rules remain for deliberate administrator review. No source import directory is deleted.

Signed-update host/worker trial health and rollback are not implemented. The update CLI refuses host roots before entering the legacy consumer/health flow. For now upgrade by reinstalling a verified package, preserving configuration/state. Host state schema rollback compatibility still needs release-specific testing.

## Verification

Exercised on Ubuntu 24.04 (WSL2, systemd running) from the development tarball: non-root refusal; fresh install creating the `vesta` system account, root-owned binaries/unit/config (the account cannot write them), enabled+active system unit running `vesta-agent host /var/lib/vesta/host.toml` as `vesta`; TLS on `0.0.0.0:25500` reachable via the eth0 address; `sudo vesta setup-code` printing a code (refused for other users) and no code in journald; `vesta import-root add` on another user's mode-700 folder granting read but not write; overlap refusal; rerun preserving identity, config, host id and import roots; uninstall refusing without a data choice, `--keep-data` then reinstall onto it, `--remove-data`.

Not established: an actual reboot (the unit is enabled for `multi-user.target`, but WSL was not restarted), mDNS reception from another machine, AMP import completion, active ufw/firewalld mutation, or a signed published-release install.
