# Server-scoped local CLI

The `vesta` executable and compatibility name `vesta-agent` share one entrypoint.
New release bundles include both. The installer still accepts previously signed
three-component bundles; the only permitted fourth artifact is the signed short
CLI executable. Existing scripts using legacy commands continue to work.

For source builds, [the development shell](dev-session.md) builds all four server commands
and puts them on PATH in a temporary terminal session. `vesta-rebuild` selects a
fresh build there without restarting running servers.

## Choose an installation and server

```sh
vesta list
vesta list --json
vesta list --install-root /srv/vesta/servers
vesta manage srv_0123456789abcdef0123456789abcdef
vesta manage my-server
```

The install root contains `names/` and `instances/`. Select it with `--install-root`
or `VESTA_SERVER_ROOT`; the fallback remains `./vesta-servers`. Listing reads that
registry without opening databases, probing every server or creating directories.
Local installation IDs (`srv_...`) are distinct from an exported server identity
UUID. A name is an alias. The management session binds both to one instance and
refuses commands if the alias is redirected to another instance.

Raw standalone `host.toml` configurations remain supported through `vesta agent`
commands. This change does not silently adopt unregistered directories or move
existing servers into a new registry.

## Agent startup

Create the registry directory before the first `vesta create`. Interactive creation
asks whether this server's agent should start at boot. Scripts must choose
`--agent-startup manual` or `--agent-startup boot` explicitly. This choice controls
the agent, not Minecraft, and never accepts the Minecraft EULA.

```sh
vesta manage my-server agent startup status
vesta manage my-server agent startup boot
vesta manage my-server agent startup manual
vesta manage my-server agent start
vesta manage my-server agent stop
```

Linux boot startup uses a per-user systemd service and account linger, so the
agent can run without a sign-in. Enabling boot on an existing server registers
future startup; `agent start` starts it now. Changing to manual requires a stopped
agent and game. Windows boot registration is currently unsupported; choose manual.
See [managed agent startup](managed-agent-startup.md) for ownership and removal rules.

Manual startup accepts a disabled network listener, allowing local setup before
pairing. The equivalent direct command is
`vesta-agent agent serve /absolute/server/host.toml --allow-local`. Unlike
`--local-only`, `--allow-local` preserves a configured remote listener. Java paths
such as `/usr/bin/java` are resolved through alternatives before validation.

## Interactive management

```text
VESTA / my-server
srv_0123456789abcdef0123456789abcdef

my-server [89abcdef]> status
my-server [89abcdef]> player_list
my-server [89abcdef]> pair --role viewer
my-server [89abcdef]> stop
my-server [89abcdef]> exit
```

The terminal uses a line editor only when attached to a TTY, with a scoped prompt
and compact banner. `NO_COLOR` or `TERM=dumb` disables the banner's color. It is a
structured command interface; it never invokes an operating-system shell.

Data commands show readable sections and labeled fields when stdout is a terminal.
Status keeps unknown player counts explicit; a failed player query is never shown
as an empty server. Device/account lists, access settings and pairing results also
use this presentation. Stored names cannot inject terminal control sequences.
Append `--json` to a data command, for example `status --json` or
`player_list --json`, to request its original JSON response. This override applies
only to that command. Redirected/piped output retains the existing machine format;
legacy top-level commands keep their previous output.

`help` lists the server commands. `exit`, `quit`, `back`, EOF or Ctrl+C while
editing leave the interface. Supported line-editor terminals accept Ctrl+D;
`TERM=dumb` uses ordinary OS input/EOF behavior (Windows: Ctrl+Z then Enter).
Entering or leaving the interface does not start,
stop or terminate the agent or Minecraft. An explicit lifecycle command has its
ordinary effect. There is no automatic replay after losing a command's response.
Interrupting a client that is waiting for an admitted command does not undo that
command; inspect its outcome before deciding whether to retry.

There is no persisted command history. Paths with spaces must be quoted; both
single and double quotes group arguments, and backslashes remain literal so
Windows paths are preserved. Shell expansions, pipes, redirection, command
substitution and switching the install root inside a session are not supported.
Piped input is bounded to 8 KiB per command; EOF exits, and a failed command is
retained as a nonzero process exit status even if the script later says `exit`.

## One-shot commands

```sh
vesta manage my-server status
vesta manage my-server player_list
vesta manage my-server status --json
vesta manage my-server start
vesta manage my-server devices list
vesta manage my-server backup init
vesta manage my-server recovery export --output '/srv/exports/recovery.json'
vesta manage my-server delete --confirm srv_0123456789abcdef0123456789abcdef
```

Direct commands use the same scope validation, parser and execution path as the
terminal, and return normal command exit codes. Destructive archive/delete and
launch-intent resolution require the selected installation ID as confirmation;
the existing recovery-import command still requires the exported identity UUID.
Deletion preserves the existing recoverable-move behavior and ownership checks.

`status` uses the running agent's protected native channel. `player_list` uses the
same bounded, serialized, generation-bound query engine as the management API,
with a two-second local wait. It reports `available: false` and a fixed reason
when stopped, timed out or otherwise unavailable; `players: null` is not an empty
server. These reads require an agent rebuilt with this interface. No HTTP route
receives a local owner grant; remote player queries retain their subject and
capability checks at admission, write and observation.

## Agent commands

```sh
vesta agent serve /srv/vesta/servers/instances/srv_.../host.toml
vesta agent check-config /srv/vesta/servers/instances/srv_.../host.toml
vesta agent status /srv/vesta/servers/instances/srv_.../host.toml
vesta create --help
vesta version
```

`agent status <path>` retains the existing quiescent/offline database inspector;
`manage <id> status` is the live observation. The scoped terminal is a client of
the existing sole process owner, not a new daemon or process supervisor.
