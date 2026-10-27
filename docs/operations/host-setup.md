# Vesta host setup

`vesta-setup` installs the signed Vesta host bundle and starts a small, local readiness service. It prepares a host for later app onboarding; it does not set up Minecraft, choose Java, pair an owner device, or enable management APIs. The app onboarding and the activation step described below are still future integration work.

The setup CLI targets Windows x86-64, Linux x86-64, and Linux ARM64. Automatic setup-host startup is available on Windows when PowerShell and Task Scheduler are available, and on Linux when the current user's systemd session is available. This setup-host option is distinct from each managed server agent's boot policy. macOS and other targets are unsupported. Windows and isolated Ubuntu 24.04 installation tests are recorded; Linux ARM64 remains untested. There is no live Minecraft launch in this setup path.

Focused verification covers a real pending TLS host, resume/recovery, manual stop/restart, signature and artifact rejection, strict headless input, Windows junction rejection, port rechecks, platform path handling, external-result validation, terminal frame bounds, and 40 captured production-renderer states. Windows interactive uninstall paths have also been exercised through a PTY. Clean Ubuntu terminal testing found an invalid quoted systemd WorkingDirectory, now covered by an explicit systemd parser regression test. Automatic registrations are tested only in the disposable Ubuntu environment, not on the development host. See the dated clean-install reports for actual outcomes and remaining coverage.

## Install interactively

Use exactly one verified release source: an absolute local release directory or an HTTPS release URL. Both require the release manifest SHA-256 pin and the Ed25519 public release key supplied by the distributor.

```text
vesta-setup --bundle-dir <release-directory> \
  --manifest-sha256 <64-hex-character-pin> \
  --release-key <64-hex-character-public-key>
```

For an HTTPS release, replace `--bundle-dir <release-directory>` with `--bundle-url <https-base-url>`. A local release directory contains `manifest.json`, `manifest.sig`, and the listed artifacts. Setup checks the pinned manifest, signature, target, and artifact hashes before installing. Do not copy verification values from an untrusted source.

The interactive wizard chooses a dedicated Vesta host folder, optionally records the location of an existing Minecraft directory, offers the default HTTPS port 8443 or a custom port, checks availability, and asks whether the host should start at user sign-in or manually. A busy default port can be rechecked. The TLS listener is verified after startup. An existing game directory remains in place; Vesta installs to a separate host root. No game files are launched or changed by this setup stage.

`--no-color` disables terminal colors and `--ascii` uses ASCII framing. Setup checks terminal capabilities and may require an interactive terminal for the wizard.

## Headless install and answer-file schema

Headless mode uses the same answer validation as the wizard. It requires both `--answers` and `--yes`:

```text
vesta-setup --bundle-dir <release-directory> \
  --manifest-sha256 <64-hex-character-pin> \
  --release-key <64-hex-character-public-key> \
  --answers <answers.json> --yes
```

Schema version 1 accepts exactly these fields. Paths must be absolute; the host root must be a dedicated directory that does not overlap the optional existing Minecraft directory.

```json
{
  "schema": 1,
  "root": "C:\\Vesta\\hosts\\default",
  "existing_server": null,
  "startup": "manual",
  "port": 8443
}
```

`existing_server` is either `null` or an absolute path to a detected existing Minecraft server directory. `startup` is `manual` or `automatic`. `port` defaults to 8443 when omitted; a custom port must be nonzero and available on IPv4 loopback. The optional `external_probe` field is described below. Unknown fields, relative paths, overlapping paths, link/reparse-point paths, and non-empty unrelated destination folders are rejected. Windows accepts either slash spelling and displays native backslashes; Unix preserves case and literal backslashes in filenames. If setup already began in the destination, rerun with the same answer values and the same release manifest to resume; changing either requires a new host root.

## External reachability checks

An external check is separate from the required local TLS readiness check. Configure an operator-hosted HTTPS probe using `VESTA_SETUP_PROBE_URL=https://probe.example.org/v1/check` before starting interactive setup. The wizard asks before sending the public source address, selected port, and TLS fingerprint to that service. Without a configured probe, setup reports external access as not checked. No third-party checker is silently selected.

Headless answers can include `"external_probe": {"url": "https://probe.example.org/v1/check", "consent": true}`. The URL must be clean HTTPS without credentials, query parameters, or fragments. Explicit consent is required. Probe checks are bounded to eight seconds and reject redirects, oversized or malformed replies, and mismatched request nonces, installation IDs, ports, or TLS fingerprints.

External results are `reachable`, `unreachable`, `unavailable`, or `not_checked`; none prevents a locally ready installation from completing. Local readiness proves that the TLS listener is running on this computer, not that a firewall or router permits internet access. The pending host remains loopback-only, so direct public access is expected to be unreachable unless an explicitly configured forwarding path reaches it. Setup does not silently change firewall rules, NAT mappings, or listener exposure.

The stateless service and its deployment contract are documented in [scripts/reachability-probe](../../scripts/reachability-probe/README.md). Deploy it on a separate public network; there is no published Vesta probe URL in this repository. It probes only its request's TCP source address, verifies the pinned pending-host identity, and accepts no arbitrary target address. A reverse proxy that replaces the source with a private address is unsupported.

## Host lifecycle commands

All lifecycle commands take the installed host root. Quote paths that contain spaces.

```text
vesta-setup status --root <host-root>
vesta-setup host --root <host-root>
vesta-setup stop --root <host-root>
vesta-setup activate --root <host-root>
```

`status` prints JSON with `phase`, `installation_id`, `pending_host_ready`, and the last `external_access` result. The readiness value checks the pending local setup endpoint; it is not a Minecraft or management readiness signal. The external result is a setup-time snapshot, not continuous monitoring.

`host` runs the host process in the foreground. Before activation it serves only the pending local readiness endpoint. After activation it starts the installed `vesta-agent` with the host configuration. The normal installer launches this process in the background so setup can verify the endpoint before it exits.

`stop` writes a local stop request. A running pending host notices it shortly and exits cleanly. This does not uninstall Vesta or remove the automatic startup registration. `stop` is not the stop command for an activated agent; use the agent's lifecycle controls once onboarding has activated it. To run the pending host again, launch `vesta-setup host --root <host-root>` or rerun setup with its original options.

`activate` is the local handoff boundary for app onboarding. It requires a ready pending host, a valid `host.toml` in the host root, and a working `bin/vesta-agent`. It asks that agent to validate the configuration with `check-config`, then changes the receipt phase from `ready` to `activated`. The running host notices the phase change and hands off to the agent. The CLI does not create the configuration, authenticate an owner, or provision Minecraft. There is not yet an app flow that performs these steps; invoking activation before a future onboarding flow has written a valid configuration will fail.

## Pending local TLS service and app handoff

Setup creates a private TLS key and certificate and records the certificate's SHA-256 fingerprint in `private/handoff.json`. That file has schema `1`, an `installation_id`, an `endpoint`, `tls_sha256`, status `awaiting_app_onboarding`, and a `vesta://setup` app URI. The endpoint is `https://127.0.0.1:<port>`; the service binds IPv4 loopback only.

Before activation, the only successful request is:

```http
GET /api/v3/setup HTTP/1.1
Host: localhost
```

Its JSON response contains `api_version: "3"`, the matching `installation_id`, `status: "awaiting_app_onboarding"`, the release `version`, and `management_available: false`. Other paths return 404. This endpoint is a bounded readiness/handoff probe, not a management API: it has no pairing flow, owner credential, mutation route, game controls, or management authorization. The TLS certificate is self-signed, so a future app client must pin the fingerprint from the local handoff and verify the installation ID before proceeding. The fingerprint and installation ID identify the local installation; neither is a pairing credential.

The intended local activation contract is: the app reads the handoff descriptor, pins the local TLS identity, confirms the installation ID against the pending endpoint, completes its local onboarding work, writes a valid `host.toml`, and invokes `vesta-setup activate --root <host-root>`. Owner pairing, device credentials, Minecraft selection, Java setup, and management API availability belong to that later app flow and are not implied by a successful setup receipt.

## Startup behavior

Both startup choices start the pending host once as setup finishes so the installer can verify it. `manual` means setup does not register automatic startup; it does not mean the host remains stopped after installation. The manually started host lasts for its current process/session and can be stopped with the command above.

`automatic` registers a current-user logon start: a limited, interactive Windows Scheduled Task or a current-user systemd service. It starts at user sign-in/session startup, not at machine boot, and does not create a privileged machine service. This behavior depends on the user's platform session facilities being available.

## Uninstall

Use `vesta uninstall --root <host-root>` or `vesta-setup uninstall --root <host-root>`. Both require interactive confirmation and offer to preserve managed server data by default. Verified Bash and PowerShell bootstrap helpers can also perform uninstall. See [uninstall commands and safety](uninstall.md) for individual-server removal, path scope, Windows completion receipts and recovery.

## Reproduce terminal preview screenshots

The preview export uses the production Rust view constructors and renderer. It creates a new export directory containing `states.json` plus one `.ansi` file per state. The browser viewer renders the exported ANSI frames with xterm; it does not synthesize onboarding content.

From the repository root, export frames into a directory that does not already exist:

```powershell
cargo run -p vesta-setup -- preview --export 'V:\work\setup-frames'
```

Install the pinned preview tools, then start the local-only viewer with an absolute export path:

```powershell
cd scripts/setup-preview
npm ci
node server.mjs --states 'V:\work\setup-frames' --port 4318
```

Open `http://127.0.0.1:4318/`. Use the state selector or left/right arrow keys to move through frames. A direct capture URL is `http://127.0.0.1:4318/?state=<state-id>&capture=1`; capture mode hides the selector and leaves the small preview caption above the terminal.

To reproduce the screenshot set and gallery, leave the viewer running and use an installed Chromium-compatible browser executable. The output directory must be new:

```powershell
node scripts/setup-preview/capture.mjs `
  --browser 'C:\Program Files\Google\Chrome\Application\chrome.exe' `
  --url 'http://127.0.0.1:4318/' `
  --output 'V:\work\setup-captures'
```

The capture script visits each exported state, waits for xterm's write and browser layout to settle, and writes PNG files, a hash manifest, and `gallery.html`. Screenshots should be generated from the compiled Rust export and this viewer so review evidence stays tied to production renderer output.
