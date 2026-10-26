Clean-room Vesta usability review - 2026-10-27
Environment: disposable Ubuntu 24.04.5 WSL distro Vesta-Usability-20261027, user terra. Locally signed test releases, not public assets. No source, tests, implementation plan or existing reviews read. Setup SHA-256 matched TRUST.txt. No screenshots available: terminal observations are based on captured PTY text and escape sequences.

Findings, ordered by severity:

1. High - installation-to-management journey blocked by absent onboarding (documented product limitation).
Install verified release 1 interactively: new host, default location, custom port 18443, manual startup. Final screen says "Your host is ready", "Vesta is installed and the local connection is open", but gives no onboarding next step or caveat that management and Minecraft are unavailable. status reports phase ready and pending_host_ready true. activate reports "Vesta setup: setup_agent_configuration_invalid"; host-setup documentation explains app onboarding is future work. Expected: distinguish pending local readiness from management readiness and state current limitations and next steps. Real scoped management was blocked; no EULA accepted or game launched.

2. Medium - installed commands are not discoverable after setup.
Complete install at default path, run which vesta in fresh shell: exit 1, no output. Final screen shows installation directory, but no bin path, PATH instructions or runnable command. Documentation uses bare vesta and vesta-setup without explaining how installed binaries enter PATH. Absolute /home/terra/.local/share/Vesta/hosts/default/bin/vesta --help works. Expected: provide concrete invocation or PATH instructions. This was observed on fresh WSL; other platforms untested.

3. Medium - missing server produces opaque error.
Run installed vesta manage missing in clean registry: exit 1, "local_command_failed: local_file_unavailable". Expected: identify missing server or registry, mention selected registry and point to list or --install-root. Exact cause inferred from clean registry; message does not establish it.

4. Low - activation failure lacks recovery guidance.
Run installed vesta-setup activate --root <installed root> after successful setup: exit 1, "Vesta setup: setup_agent_configuration_invalid". Documentation says valid host.toml and onboarding are prerequisites, but command gives neither context nor remedy. Expected: explain missing or invalid configuration and onboarding prerequisite.

Positive observations and scope:
PTY wizard labelled six steps with navigation hints. Custom port abc showed "Enter a port between 1 and 65535"; correcting to 18443 passed. Review showed root, startup and endpoint. These are text observations, not screenshot-based visual assessment. Manual install succeeded and status remained ready. vesta list returned "No registered servers"; help and create/manage help returned syntax. No server created because creation requires Minecraft version and Java and test forbids real game and EULA. Signed update status trusted sequence 1, check and stage of release 2 succeeded with phase staged and sequence 2; activation documented unsupported. Uninstall --plan listed installation, registry and preservation destination. Interactive uninstall chose continue and default keep, inspected final review, then typed cancel; mismatch stopped uninstall and subsequent status remained ready. No removal performed.

Limitations: No screenshots, launcher onboarding, populated registry, scoped interactive session, real server lifecycle, game process, automatic startup, external probe, narrow terminal or SSH login test. WSL systemd/session and disposable local signing are test conditions, not evidence of public release defects. No code edits. Installed test host and staged update preserved at /home/terra/.local/share/Vesta/hosts/default.

