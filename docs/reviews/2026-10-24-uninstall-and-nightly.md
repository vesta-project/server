# Confirmed uninstall and signed nightly delivery

Scope: `server-connections-mvp`, branch `feat/launcher-compat`. Initial implementation checkpoint: `6cec785`. The launcher checkout and live development agent were not changed or restarted for this work. No actual user server was uninstalled, no remote release was published, and no signing secrets were configured.

## Delivered behavior

- OS-local single-server uninstall, including the scoped management terminal. Initial confirmation, exact server name, independent keep/purge choice, and final typed consequences confirmation are required.
- Whole-Vesta uninstall with an explicit registry selection, keep-data default, final `UNINSTALL VESTA` confirmation, owned startup deregistration, and graceful shutdown checks.
- Private external helper and completion receipt for Windows installed-tool removal. Exit 75 represents handoff; success is recorded only after completion. An installed setup wrapper is also awaited before deleting its executable.
- Native setup and Bash/PowerShell bootstrap forwarding, including verified signed tools staged outside the installation. Uninstall rejects unattended confirmation bypasses.
- Push/PR/manual CI and signed nightly release workflows for native Windows x86_64, Linux x86_64 and Linux ARM64. Checks precede signing; the publisher independently verifies signatures, executable hashes, target metadata and canonical bootstrap commands.
- Flat target-prefixed GitHub Release assets, date/commit identities, serialized publication, unchanged-source skip, draft upload before publication, and no overwrite of an existing tag/release.

Commands, defaults, deletion boundaries and interruption behavior are in [uninstall.md](../operations/uninstall.md). Signing configuration and activation requirements are in [nightly-releases.md](../operations/nightly-releases.md).

## Review findings resolved

The first independent review found a registry mutation race after releasing the registry lock, misleading preserved registry paths, and premature Windows success reporting. Maintenance admission checks now reject pending removal, while stable private coordination locks outside the removal trees exclude another coordinator through final unlink/rename. Preserved paths are displayed before confirmation. Windows handoff has its own non-success status and a completion receipt.

The second independent review found the Windows working-directory lock case for individual-server removal. The command now leaves all selected removal trees first, including external registries. Follow-up checks also covered the installed setup executable remaining locked and Windows case/verbatim spellings in relocated registry paths.

The publisher rejects injected bootstrap commands even if all genuine pins remain in the text. It compares against the same canonical generator used by packaging. Linux executable permissions are restored after artifact transport; convenience archives are rebuilt from verified staging. A truncated PE header is rejected without an out-of-bounds read.

## Validation recorded on Windows

- `cargo test -p vesta-agent -p vesta-setup --locked -- --test-threads=1`: 92 tests passed. Subsequent focused tests passed for the added Windows registry spelling regression and native setup refusal cases. Existing behavioral tests remain; obsolete help-text expectations were updated. Focused uninstall tests: 12 passed; native setup refusal: 1 passed.
- `cargo clippy -p vesta-agent -p vesta-setup --all-targets --locked -- -D warnings`: passed.
- Node conformance/bootstrap/probe suite: 189 tests, 187 passed, 2 Bash runtime tests skipped on Windows, no failures.
- Bash syntax, PowerShell AST parsing (including generated commands), Rust formatting, Git whitespace checks, requirement ledger (370 baseline bullets), and generated roadmap freshness: passed.
- Actual PTY: incorrect server name refused without removal; individual keep retained a recovery world; individual purge removed only the selected instance; whole keep retained state/worlds at the printed sibling location; whole Windows self-removal wrote `complete: true` after exit 75.
- Additional actual PTY: purge from inside the server's world directory succeeded; installed `vesta-setup` handed off and completed after both parent executables exited; external setup loaded an ephemeral signed local test bundle, staged the tools, and completed removal synchronously with exit zero.

Every destructive terminal test used newly generated inert fixtures under OS temporary directories, with no Java process, active service or real world. The local signed test bundle used an ephemeral in-memory test key. Actual fixture outcomes were checked on disk, including registry records, preserved world files, removed roots and helper receipts.

Local raw logs are in `V:\tmp\uninstall-tools-complete.log`, `uninstall-final-safety-tests.log`, `uninstall-setup-refusal.log`, `uninstall-clippy-complete.log`, and `uninstall-node-complete.log`. They are disposable evidence; this report preserves the results outside Rust `target`.

## Remaining deployment checks

Real Linux SSH setup/uninstall, user-systemd behavior, production startup registration removal and hosted Actions execution have not been exercised here. The workflows need the documented `release-signing` environment, private signing secret, public-key repository variable and branch policy before publication can succeed. Scheduled execution requires the workflow on the default branch. No push or remote configuration was performed.

External game/state/backup directories and Java are intentionally outside recursive deletion. Unknown or unproven running sessions fail safely. Tree inventories are bounded and no-follow; same-user hostile filesystem races remain outside the isolation guarantee. Retained data with absolute paths may require reconfiguration before reuse.
