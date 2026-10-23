# Uninstall Vesta or one managed server

Uninstall is an OS-local operation run as the installation's user. It is available in the short `vesta` CLI, the scoped management terminal, native setup, and the verified bootstrap scripts. It does not expose a remote deletion API. Start with `--plan` to inspect the selected paths without stopping or removing anything.

## Remove one server

```text
vesta uninstall <server-name-or-id> --install-root <registry-directory> --plan
vesta uninstall <server-name-or-id> --install-root <registry-directory>
```

Inside `vesta manage <server-name-or-id>`, use `uninstall`; the management context supplies the registry and server. The direct equivalent is `vesta manage <server-name-or-id> uninstall` with the usual management registry options.

The command first asks whether to continue, then requires the exact resolved server name. It asks separately whether to permanently delete the server's managed files, worlds and backups. The default preserves them. A final review names the selected server and directory and requires `UNINSTALL <server-name>`.

Preserving removes the server from the registry and moves its managed directory into `recoverable/<recovery-id>/server` under that registry. The resulting location is printed. Permanent removal deletes that recovery copy after the server has stopped. Other servers and externally configured directories remain untouched. A changed name binding or configuration requires another review.

## Remove the Vesta installation

```text
vesta uninstall --root <installation-directory> --plan
vesta uninstall --root <installation-directory>
```

The default registry is `<installation-directory>/servers`. Use `--servers-root <registry-directory>` when the installation uses another registry. The selected registry must be a dedicated Vesta registry, not a shared directory of unrelated files. The review includes registered servers, unregistered managed instances, and recovery copies within that registry.

After the initial question, choose whether to permanently remove all managed servers and recovery copies in the selected registry. **Keeping server data is the default.** The final review lists the consequences and requires `UNINSTALL VESTA`.

- **Keep:** remove installed binaries and owned automatic startup registration, then move the remaining installation into the sibling directory `vesta-preserved-<installation-id>`. The review and result show the resulting registry path. A registry outside the installation stays in place. Preserved absolute configuration paths may need adjustment before reuse.
- **Remove:** remove owned automatic startup registration, the selected managed registry, and the installation directory. Recovery copies in that registry are included. External game, state or backup directories and Java installations are preserved.

The command verifies ownership before removing a current-user Windows Scheduled Task or Linux user systemd unit. It refuses mismatched registrations. It does not alter machine services, user linger settings, firewall rules, or other installations.

## Use setup or the bootstrap script

Native setup supports:

```text
vesta-setup uninstall --root <installation-directory> [--servers-root <registry-directory>] [--plan]
vesta-setup uninstall --server <server-name-or-id> --servers-root <registry-directory> [--plan]
```

Installed setup delegates to its companion CLI. With verified bundle arguments it instead stages signed tools outside the installation. Bootstrap scripts use this external staging path, so they can remove an installation whose installed tools need replacing.

Use the setup URL, setup SHA-256, manifest SHA-256 and public-key pin from the trusted release. Add these options to the source helper invocation in [bootstrap packaging](bootstrap-release.md):

```bash
# Whole installation; retain the normal setup/release verification arguments.
bash scripts/bootstrap/install.sh <verification-arguments> --uninstall --root /srv/vesta
# One server; omit --root.
bash scripts/bootstrap/install.sh <verification-arguments> --uninstall --server survival --servers-root /srv/vesta/servers
```

PowerShell uses `-Uninstall -Root 'C:\Vesta'`, or `-Uninstall -Server survival -ServersRoot 'C:\Vesta\servers'`, with the same release verification arguments. `--plan` / `-Plan` previews the selection. The shell scripts forward to native setup; they do not implement deletion themselves. The script source must also come from a trusted revision. See [nightly releases](nightly-releases.md) for the signed release asset layout.

## Completion, interruption and safety

Uninstall requires interactive input and output, including when the helper is piped to Bash over SSH. There is no `--yes`, `--force`, or answer-file bypass. Cancellation before final confirmation performs no removal.

Vesta requests cooperative local shutdown and proves the agent, supervisor and game have stopped before removing files. It does not force-kill Minecraft. An older agent without the local shutdown command must be exited manually first. A stale or unproven operator session is a deliberate refusal requiring the existing supervisor recovery process; uninstall does not discard the evidence.

Whole-installation removal records pending markers that block new setup, agent starts, registry mutations and offline edits. If it stops partway, inspect the reported error and remaining paths before rerunning and confirming the same scope. Do not delete pending markers to bypass an unresolved process or ownership problem. A success message is emitted only after the selected filesystem operation completes.

On Windows, a running executable cannot delete itself. When installed tools are being removed, the CLI can hand the confirmed operation to a private temporary helper. The caller exits **75**, meaning **handed off, not completed**. Keep the terminal open and inspect the printed `result.json` receipt: `complete: true` confirms completion; `complete: false` includes the failure code. The helper waits for both the CLI and an installed setup wrapper to exit before deletion. Private helper files remain in the OS temporary directory for diagnosis. External bootstrap staging normally completes synchronously and returns zero only after removal.

Tree removal inventories a bounded number of entries and path bytes, refuses special files and changed inventories, and unlinks links/junctions without traversing their targets. Stable coordination lock files remain in private OS temporary directories so concurrent uninstallers cannot obtain different lock identities. These checks protect normal same-user operation; they are not a security boundary against a hostile process running as that same OS user.

Windows fixture tests cover cancellation, name verification, keep/purge, nested registry preservation, junction isolation, maintenance gates, coordinator exclusion, and helper completion. Linux startup ownership has pure tests; real Linux SSH installation/uninstallation and hosted GitHub Actions execution still require platform validation.
