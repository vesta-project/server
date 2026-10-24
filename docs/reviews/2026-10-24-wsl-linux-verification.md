# Ubuntu WSL Linux installation/update verification

Environment: Ubuntu 24.04.1 LTS, WSL 2, x86_64, kernel 5.15.167.4,
systemd PID 1 and a running systemd user session. Tested source is `65060fc`
plus the Unix fixes committed in `61272ed`.

An isolated workspace was created at
`~/.local/share/vesta-linux-test-20261024`, on the Linux filesystem rather than
DrvFs. It contains its own Rust toolchain (stable 1.99.1), Cargo cache, Node
22.14.0, source snapshot, build outputs, test-only signing key, release bundles,
installation and logs. Downloads were checked against their official SHA-256
files. Shell profiles, default toolchains, existing services and the Windows
development server were not changed. Build concurrency was two jobs and debug
symbols/incremental compilation were disabled to bound resource use.

## Results

- Bootstrap Node suite: **21 passed, zero skipped**, including both Bash runtime
  tests previously skipped on Windows and Linux private-key permissions.
- Rust setup/update suites: **27 passed** (2 setup library, 20 setup integration,
  5 update library); no failures or ignored tests.
- All four Linux executables built successfully: setup, agent, supervisor and CLI.
- Two local Linux release bundles were signed using a **test-only** generated key
  with sequences 1 and 2. These were not published or configured as production trust.
- Actual headless setup installed the first bundle into the isolated workspace,
  started its onboarding host and verified the local TLS endpoint. No Java or
  Minecraft server was installed or started.
- Automatic update trust provisioning recorded sequence 1 and its public pin.
  Explicit check/stage accepted sequence 2. SHA-256 checks confirmed all four
  active installed executables were unchanged after staging.
- Trying to stage sequence 1 afterward returned `update_rollback_refused`.
- The temporary onboarding host was stopped. A subsequent status query confirmed
  `pending_host_ready=false`. Files and logs remain for the next test session.

## Issues found and fixed

Unix-only error conversion closures in release permission/sync and startup-unit
creation needed explicit String conversion. Linux startup ownership test helpers
also needed visibility to the parent test module. Unnecessary mutable file
bindings were removed. These issues had been hidden by Windows conditional
compilation and are fixed in `61272ed`.

The first source archive made by Windows Git contained checkout-converted CRLF
files, causing a workflow-regex test failure. Re-exporting the committed tree
with `git -c core.autocrlf=false archive` corrected the test input; no product
test was weakened. Future snapshots should preserve committed LF source bytes.

The newer Linux Rust compiler reports deprecation warnings for existing
`AtomicUsize::fetch_update` calls. They were not replaced with a newer-only API
because the repository still supports Rust 1.89. These warnings did not fail
compilation or tests.

## Evidence and remaining scope

Logs live in the workspace's `logs/` directory: `bootstrap-tests.log`,
`update-setup-tests.log`, `linux-build.log`, `linux-install.log`,
`active-before.sha256`, `update-before.json`, `update-check.json`,
`update-stage.json`, `update-after.json`, `rollback-refusal.log` and
`test-host-stop.log`.

This verifies local WSL/Linux filesystem, executable and installer behaviour.
It is not an SSH transport test, published HTTPS release download, automatic
startup/service deployment, independent Linux boot, constrained-host performance
benchmark, security isolation test or real Minecraft/launcher pairing test.
Vesta binary activation/health-check rollback remains unimplemented.
