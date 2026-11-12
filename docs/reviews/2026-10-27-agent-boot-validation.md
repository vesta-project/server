# Managed agent boot validation

The clean-install tester used a fresh Ubuntu 24.04 WSL distribution with no host
mounts, existing Vesta installations or repository access. Test bundles were
signed by an ephemeral local fixture authority, not published as releases.

Implemented:

- New-server creation asks whether its agent should start manually or at boot.
  Headless creation requires an explicit choice. Minecraft startup and its EULA
  remain separate.
- Linux boot-managed installations use user systemd services, explicit linger and
  the existing separate game supervisor. Minecraft initially stays stopped.
- Scoped commands inspect/change agent startup and explicitly start/stop agents.
  Existing in-process installations cannot bypass unresolved shutdown evidence.
- Startup receipts retain the original executable, server ID, account and unit
  binding for safe removal. Registration removal follows quiescence proof.
- Setup's finish screen distinguishes the pending setup endpoint from an actual
  managed Minecraft server and supplies an absolute CLI command.

The real tests found and fixed three failures: a quoted systemd WorkingDirectory
rejected by the unit parser; in-process ownership preventing safe recovery after
an abrupt agent exit; and Linux's 108-byte Unix socket address limit under normal
managed installation paths, affecting both supervisor and agent control sockets.
Long-path bind/connect now use a shared retained directory
descriptor while keeping the protected on-disk socket, UID/permission checks and
stale-socket ownership checks. No global temporary socket or relaxed ACL is used.
The control endpoint also reclaims an owner-only, unchanged stale socket only
after a refused connection proves it has no listener, under the existing agent
and runtime ownership locks. Live sockets and foreign files remain untouched.

Validation completed before the final signed-bundle retest:

- Agent/setup suites pass on Windows and Ubuntu. The ignored Linux systemd parser
  test was explicitly run and passed. Strict Clippy passed on both platforms.
- All 23 Linux supervisor lifecycle tests pass, including a real long-path socket
  bind/connect, stale-socket recovery and owned-socket cleanup regression.
- The agent control regression covers a long-path framed request/reply, stale
  socket recovery, live endpoint refusal and preservation of a foreign file.
  Shared helper tests reject symlinked and group/world-accessible directories.
- Test games in those Rust suites are inert owned fixtures, not Minecraft.
- Earlier Terra fixtures and failed-session evidence are retained unchanged.

Limits: Windows machine-boot registration is explicitly unsupported, not replaced
with sign-in startup. WSL restart testing does not establish physical-machine boot
behavior or Linux ARM64 coverage. Launcher onboarding after host setup remains
unfinished. No EULA was accepted and no real Minecraft process was started.

## Final result

Terra's [final black-box report](2026-10-27-clean-install-boot-passed-terra.md)
passes fresh installation into a deliberately long managed registry path, initial
agent readiness, automatic recovery after terminating/reopening only the disposable
WSL distro, and explicit scoped agent stop/start. Minecraft remained stopped and
the EULA remained unaccepted throughout. Boot policy stayed enabled after manual
stop/start. A check about 0.6 seconds into startup was too early; readiness succeeded
about three seconds after service start without manual intervention.

One idle sample recorded one agent at 18,044 KiB RSS and one supervisor at 3,944 KiB.
This is an observation, not a low-end resource benchmark, and excludes other
processes/shared-memory accounting. A physical Linux boot without user sign-in and
Windows boot registration remain unverified or unsupported as described above.
