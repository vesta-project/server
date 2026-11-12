# Managed server agent boot registration

`vesta_setup::server_startup` controls only the managed server agent. It never starts Minecraft, accepts an EULA, or changes Minecraft autostart.

New installations choosing boot startup use the existing separate game supervisor,
with Minecraft startup explicitly set to `stay_stopped`. This lets the agent
recover after an abrupt exit without bypassing the in-process orphan guard.
The supervisor is another small process while the agent is active; Minecraft is
still started only by its separate policy or an explicit command. Existing
in-process installations must be deliberately stopped and configured for separate
supervision before enabling boot startup. An unresolved old session is preserved,
not automatically cleared or reclassified.

API:

- `prepare(id: &str, directory: &Path, executable: &Path) -> Result<Registration, String>` validates a full `srv_` plus 32 lowercase hexadecimal ID and resolves existing absolute server and executable paths.
- `Registration` is a serde descriptor containing `server_id`, `server_dir`, `executable`, `unit_directory`, and `principal`. Persist it durably in the managed server record before calling `enable`. Reuse this original descriptor during uninstall, even if the CLI executable path changes. Preserve the receipt on failures for retry/recovery.
- `enable(&Registration) -> Result<(), String>` registers the agent for future boots. It does not start the agent immediately.
- `status(&Registration) -> Result<Status, String>` returns `Enabled` or `Disabled`; inspection/session/ownership failures are errors.
- `disable(&Registration) -> Result<(), String>` removes an exact owned registration. The caller must first prove that the agent and game are quiescent. It does not stop services or kill processes. An absent registration is idempotent.

Linux uses the current user's systemd service manager and explicitly enables account linger with `loginctl enable-linger <uid>`. A working user manager and permission to enable linger are required. Errors do not silently substitute login startup. Uninstall leaves account-wide linger enabled because other applications may rely on it. The receipt pins the current UID and user unit directory. The entire unit must exactly match generated content; symlinked units, differing enablement links and local service drop-ins are refused. Operator changes must be reconciled manually. As with other per-user configuration, the current account is trusted; concurrent same-account edits are outside the ownership guarantee.

The full ID determines `vesta-agent-srv_<32hex>.service`. ExecStart uses absolute executable and host.toml paths, systemd argument quoting, doubled percent and dollar characters, and `agent serve <host.toml> --allow-local`. WorkingDirectory is `/`. Restarts are limited to three starts per 60 seconds with a five second delay. SIGINT requests graceful agent shutdown; `KillMode=process` and `SendSIGKILL=no` prevent forced termination of the game/supervisor family.

Windows machine-boot registration currently returns an explicit unsupported error. No task, password, SYSTEM identity, or login trigger is created. Microsoft's [TaskFolder.RegisterTask documentation](https://learn.microsoft.com/en-us/windows/win32/taskschd/taskfolder-registertask) states that creating boot-trigger tasks requires membership in Administrators. Its [task security documentation](https://learn.microsoft.com/en-us/windows/win32/taskschd/security-contexts-for-running-tasks) also requires batch logon rights for S4U. A future Windows implementation needs an elevated registration path with exact action, current-account limited principal, S4U, boot trigger, task path and description validation before replacement or deletion. This module deliberately does not claim that login startup satisfies machine boot.

Platform commands have a 20 second deadline and at most 8 KiB captured output. Pure tests validate IDs, escaping, agent-only command generation, receipt serialization, and foreign-account rejection. Tests never register services or enable linger.

Additional integration hooks: `available() -> Result<(), String>` performs a read-only preflight before installation/download work. On Linux it probes the current user manager and logind; it cannot promise the caller has permission to enable linger, which enable checks separately. Windows immediately returns unsupported. `start(&Registration) -> Result<(), String>` starts only an exact owned enabled registration, verifies its loaded fragment path and absence of loaded drop-ins, and must run after the durable registry/config publication and release of the registry lock. It starts the agent only; the published configuration controls Minecraft autostart separately.

The canonical server directory basename must equal its full server ID. Linux honors a nonempty absolute `XDG_CONFIG_HOME`, otherwise `HOME/.config`, and stores the chosen unit directory in the descriptor. Changing either environment path between enable and uninstall causes a clear ownership mismatch instead of modifying a different registration.
