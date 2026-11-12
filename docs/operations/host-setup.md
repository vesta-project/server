# Vesta host setup

`vesta-setup` installs the signed Vesta host bundle and starts a loopback TLS pending host. Backend onboarding authenticates the current Minecraft account, claims only this installation?s first server, and prepares either a new server installation or a copy of an OS-selected exported server. Launcher onboarding UI remains future work. Java/source/entrypoint selection and installation updates stay OS-local; first-server Owner access does not grant machine administration. No onboarding action starts Minecraft or accepts its EULA.

The setup CLI targets Windows x86-64, Linux x86-64, and Linux ARM64. Automatic setup-host startup is available on Windows when PowerShell and Task Scheduler are available, and on Linux when the current user's systemd session is available. This setup-host option is distinct from each managed server agent's boot policy. macOS and other targets are unsupported. Windows and isolated Ubuntu 24.04 installation tests are recorded; Linux ARM64 remains untested. There is no live Minecraft launch in this setup path.

Focused verification covers a real pending TLS host, resume/recovery, manual stop/restart, signature and artifact rejection, strict headless input, Windows junction rejection, port rechecks, platform path handling, external-result validation, terminal frame bounds, and 40 captured production-renderer states. Windows interactive uninstall paths have also been exercised through a PTY. Clean Ubuntu terminal testing found an invalid quoted systemd WorkingDirectory, now covered by an explicit systemd parser regression test. Automatic registrations are tested only in the disposable Ubuntu environment, not on the development host. See the dated clean-install reports for actual outcomes and remaining coverage.

## Install interactively

On Linux, use the source bootstrap to select and verify a signed release:

```bash
(
  set -e
  umask 077
  installer=$(mktemp)
  trap 'rm -f -- "$installer"' EXIT
  curl --disable -fsSL --proto '=https' --proto-redir '=https' --connect-timeout 15 --max-time 60 -o "$installer" https://raw.githubusercontent.com/vesta-project/server/main/scripts/bootstrap/install.sh
  bash "$installer"
)
```

Choose Stable, Nightly or an exact tag inside the installer. To preselect it, append `--channel stable`, `--latest-nightly` or `--tag RELEASE`. Installer releases are independent of the selected server package. See [bootstrap prerequisites and verification](bootstrap-release.md#install-on-linux). The bootstrap verifies the installer before execution; copying `commands.txt` is optional. The selected server package still supplies its version-matched host/runtime tools.

For direct native invocation:

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

`host` runs the host process in the foreground. Before activation it serves the pending account authentication and first-server endpoints below. After activation it starts the installed `vesta-agent` with the host configuration. The normal installer launches this process in the background so setup can verify the endpoint before it exits.

`stop` writes a local stop request. A running pending host notices it shortly and exits cleanly. This does not uninstall Vesta or remove the automatic startup registration. `stop` is not the stop command for an activated agent; use the agent's lifecycle controls once onboarding has activated it. To run the pending host again, launch `vesta-setup host --root <host-root>` or rerun setup with its original options.

The normal first-server backend flow publishes protected launch/configuration/consumer documents and the completed job before changing the receipt to activated. The pending host drains, closes its state and hands off to the actual managed agent. The older OS-local `activate` command validates an already-provided configuration; it does not replace this account-bound provisioning flow.

## Pending account and first-server flow

The TLS leaf fingerprint and installation ID in `private/handoff.json` must match the actual endpoint before authentication. The descriptor is public identity, never an invitation or session credential. Pending listeners remain IPv4 loopback-only; no firewall/NAT change or public listener is implied.

The local administrator explicitly selects Java (and, for adoption, one entrypoint):

```text
vesta-setup configure-server --root <host-root> --java <absolute-java>
vesta-setup configure-server --root <host-root> --java <absolute-java> --adoption-jar server.jar --copied-export
vesta-setup configure-server --root <host-root> --java <absolute-java> --adoption-argfile libraries/forge/args.txt --copied-export
vesta-setup setup-code --root <host-root>
```

For adoption, `existing_server` must point to a separately created export COPY, and `--copied-export` attests that provenance. Exactly one relative JAR or Java argfile may be selected locally; arbitrary scripts and remote executable/path selection are unsupported. The selected source is opened read-only, snapshotted and hashed, rechecked at copy/publication, and never moved, modified or deleted. The target is the stable `servers/first/game` root; interrupted identical effects may be reconciled, foreign effects refuse. Source version/entrypoint choices are operator responsibility.

Restart the pending host after local configuration. `setup-code` explicitly outputs a short-lived invitation only to the invoking local terminal. Do not include it in logs/reports. It can authorize only the first-server claim by a verified account, not a machine-wide anonymous Owner. Normal account verification and signed device challenge/session contracts are shared with the managed service.

| Method/route | Contract |
| --- | --- |
| GET /api/v3/setup | Public readiness: API/installation/server IDs, awaiting onboarding, management unavailable, whether provisioning is configured. No credential. |
| POST /api/v3/manage/minecraft/challenge and /complete | Existing Minecraft account verification/enrollment, with invitation where required. The corresponding /api/v3/auth aliases share the handler. |
| POST /api/v3/manage/session/challenge and /session | Existing signed device session; a session must remain linked to the current account. |
| POST /api/v3/setup/claim | Current account-bound Owner; EMPTY body. Idempotent same first owner, rejects other accounts and lesser roles. |
| GET /api/v3/setup/status | Authenticated first-server claim status. |
| POST /api/v3/setup/plan | Authenticated claimed Owner; accepts software/version selection or adopt_copy, never host paths. Returns202 job. |
| POST /api/v3/setup/apply | Reviewed job/confirmation, idempotency_key, explicit warning/legacy-checksum acknowledgements. Returns202 same job for identical retry. |
| GET /api/v3/setup/jobs/<uuid> | Current claimed Owner; bounded persisted job state, phase, safe error and review. |

Install selection example:

```json
{"kind":"install","name":"First server","software":"vanilla","version":"1.21.1","build":null,"loader_version":null,"experimental":false}
```

Copy selection example:

```json
{"kind":"adopt_copy","name":"Copied server","minecraft_version":"1.21.1"}
```

Jobs progress through planning -> review -> applying -> ready -> complete, or failed/interrupted. Prepare after a service restart requires a new review; no serialized installer plan is accepted as execution authority. Current account/rank/device/claim binding is rechecked at execution boundaries. Revocation refuses subsequent effects. Unknown routes, bodies on GET/claim and unknown request fields refuse. There are no pending game, update or isolation management routes.

At the managed-agent handoff the pending routes retire, connections can close and in-memory sessions require a fresh signed challenge/session. Clients must verify the normal GET /api/v3/status on the SAME pinned TLS/server identity and reauthenticate; a 404 or disconnect alone never proves completion. The protected completed provisioning record remains in the canonical state database. Launcher UI/adapters must implement this handoff explicitly.

The installer?s real download/verification/runtime selection and managed launch implementation is reused. Initial new servers get `eula=false`; copied EULA/config/world/disabled files keep their exact bytes. Activation here starts Vesta?s agent only, not Minecraft. Publication is absent-only or identical so a foreign host configuration is never overwritten.

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
