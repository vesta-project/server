# Signed setup bootstrap and release packaging

The setup bootstrap is a small source helper. On Linux it discovers an installer and authenticates its bootstrap descriptor with the release public key embedded in the trusted source script, then verifies the standalone `vesta-setup` executable's signed size and SHA-256 before running it. Independent installer descriptors declare supported server manifest schema; legacy release-matched descriptors bind one package manifest hash. Explicit URL/checksum/key inputs remain available. Native `vesta-setup` parses and independently checks the chosen package's full manifest, signature, platform and version before installation.

The source script is the bootstrap trust anchor: obtain it from the official repository. It never trusts a release key or executable checksum supplied by GitHub release metadata alone. To pin the bootstrap itself, use a reviewed immutable source commit instead of `main`, or download and inspect it before execution. Explicit pins must also come from a trusted channel. See [nightly release operations](nightly-releases.md) for the signed nightly publishing workflow.

## Install on Linux

Start the latest independent installer (x86_64 or ARM64 is detected automatically), then choose the server channel/version:

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

Select an exact release instead:

```bash
(
  set -e
  umask 077
  installer=$(mktemp)
  trap 'rm -f -- "$installer"' EXIT
  curl --disable -fsSL --proto '=https' --proto-redir '=https' --connect-timeout 15 --max-time 60 -o "$installer" https://raw.githubusercontent.com/vesta-project/server/main/scripts/bootstrap/install.sh
  bash "$installer" --tag nightly-20261112-a248a66a6ad3
)
```

Discovery requires Bash 4+, curl, standard Linux file/hash tools (`sha256sum`, `head`, `tr`, `wc`, `awk`), and OpenSSL supporting Ed25519 `pkeyutl -rawin` verification (OpenSSL 3 is recommended). Python, jq, Node, Rust and Java are not installer prerequisites; the script installs no packages. The public feed selects the newest `installer-*` build independently of the desired server package. Its signed descriptor authenticates the exact installer identity before execution. `--latest-nightly`, `--channel stable|nightly` or `--tag RELEASE` preselect the package in that installer; without them its native UI offers channel and version selection. Native setup uses bounded official catalog requests and the shared signed manifest verifier, never catalog-supplied keys/hashes/asset URLs. Exact tags require an authentic platform-matching server manifest. No GitHub credentials are needed or forwarded.

The script bounds the release feed to 1 MiB, the bootstrap descriptor to 64 KiB, its signature to 256 bytes, setup to 64 MiB, and HTTPS redirects to five per download. It does not fetch release-list asset inventories or parse JSON in shell. Discovery and explicit pins cannot be mixed. The documented command downloads the complete source into a private temporary file before execution; a failed or partial fetch never runs the received script. Existing answers and uninstall options work with either selector. This invokes normal setup; it does not adopt or modify an existing AMP server automatically.

The first published nightly, `nightly-20261112-a248a66a6ad3`, predates bootstrap descriptors. Its reviewed Linux setup size/hash and manifest hash are pinned directly in the trusted script, preserving its installation path. Other descriptor-less releases require independently trusted explicit pins; there is no unsigned-metadata fallback. Original release assets are not modified.

Until the first independent installer is published, the script retains the verified release-matched nightly/exact-tag path. Stable preselection requires the independent installer. Uninstall continues using verified release-matched tools. Explicit local bundles and pins keep their existing CLI behavior. These compatibility paths never execute unverified setup.

## Independent lifecycle

`installer-YYYYMMDD-COMMIT` releases contain only setup plus signed installer metadata. Their descriptor purpose is `vesta-installer-bootstrap-v1`, with server manifest schema `1` in the final field instead of a package manifest hash. Their separate `installer.json` purpose is `vesta-installer-release-v1`; it cannot be mistaken for a server manifest. Packaging, header checks, staging, key verification and signing share the existing implementation via `--product installer` rather than separate download/install engines. The native UI resolves the chosen package, checks its signature, exact tag, target and supported schema, and uses the existing preparation engine. It installs that package's matched host tools; the downloaded independent UI never replaces them with unrelated binaries.

An interactive standalone binary can run with no arguments or `vesta-setup install`. Use `install --channel stable|nightly` or `install --tag RELEASE` for preselection. Headless standalone use requires explicit channel/tag plus `--answers FILE --yes`. Existing pinned `--bundle-dir`/`--bundle-url` commands are retained. Installer code still needs an installer binary release; changing the shell script alone needs no binary release.

The main-push release orchestrator classifies changed files: shell-only and documentation edits publish no binaries, installer-view edits publish only an installer, and shared host/runtime/build-contract changes publish affected server and installer products. All three platforms must verify before publication. Releases remain immutable, signed, canonical-main-only; no scheduled/manual/fork publication or key fallback is introduced.

`bootstrap.txt` is a strict seven-line ASCII protocol, with a final newline: purpose `vesta-setup-bootstrap-v1`, version/tag, target, setup filename, setup SHA-256, decimal setup byte size, and exact manifest SHA-256. `bootstrap.sig` signs these exact bytes with the same Ed25519 key as the manifest. The shell checks the signature before parsing, then exact field count, identity, canonical encoding, size and hash. Packaging and publication share the descriptor generator and require exact agreement with the verified manifest. The publisher requires both files for new releases. A missing or bad descriptor fails closed.

## Build and package a release

Build the standalone binaries from the source tree for one supported target. Packaging requires Node.js only and does not build Rust code. New bundles require `vesta-setup`, `vesta-agent`, `vesta-supervisor`, and the short `vesta` CLI on Linux; the same names with `.exe` on Windows. Each artifact is covered by the signed manifest. The installer still accepts older signed three-component bundles. The packager accepts no arbitrary file paths and rejects symlinked inputs, incorrect executable formats or architectures, mismatched host operating systems, and an existing output directory. See [server-scoped management](scoped-cli.md).

Create an Ed25519 PKCS#8 PEM private key in a protected signing environment. Keep the private key out of the binary directory, output directory, source control, logs, and release host. For example, use an approved key-management process to supply the key file, then run:

```powershell
node scripts/bootstrap/package-release.mjs --binaries .\path\to\release-binaries --output .\out\vesta-1.2.3-linux-x86_64 --target linux-x86_64 --base-url https://github.com/vesta-project/server/releases/download/vesta-1.2.3 --asset-prefix linux-x86_64- --signing-key .\protected\release-ed25519.pk8.pem --version vesta-1.2.3
```

Use `windows-x86_64` or `linux-aarch64` for the other supported targets. Run packaging on Windows for the Windows target and Linux for Linux targets. The output must be a new directory outside the input binaries directory. The release directory contains the four executable files, `manifest.json`, detached base64 `manifest.sig`, base64 raw public key `manifest.pub`, signed `bootstrap.txt`/`bootstrap.sig`, generated one-paste commands in `commands.txt`, and public pin metadata in `release.json`.

The manifest has exactly this shape, with `files` ordered setup, agent, supervisor, CLI:

```json
{"schema":1,"version":"1.2.3","target":"linux-x86_64","files":[{"name":"vesta-setup","sha256":"<64 lowercase hex>","size":123},{"name":"vesta-agent","sha256":"<64 lowercase hex>","size":123},{"name":"vesta-supervisor","sha256":"<64 lowercase hex>","size":123},{"name":"vesta","sha256":"<64 lowercase hex>","size":123}]}
```

Publish each file as an immutable GitHub Release asset named `<target>-<logical filename>` (for example, `linux-x86_64-manifest.json`). Do not commit packages to a branch. The bundle URL passed to setup is the release download URL; setup retrieves prefixed assets while signed manifest logical filenames remain unchanged. Verify public pins through the trusted release process before distributing the commands. Do not publish or copy the private signing key.

## Bootstrap invocation

The PowerShell source helper is `scripts/bootstrap/install.ps1`; the Bash source helper is `scripts/bootstrap/install.sh`. PowerShell requires explicit `SetupUrl`, `SetupSha256`, `BundleUrl`, `ManifestSha256`, and `ReleaseKey` inputs; Bash accepts either the Linux release selectors above or those explicit pins. `ReleaseKey` is 64 hex characters representing the 32-byte Ed25519 public key. Generated `commands.txt` files remain an optional auditable set of release-specific pins, rather than the normal Linux installation entry point. They add `--asset-prefix` for flat release assets.

PowerShell example:

```powershell
& .\scripts\bootstrap\install.ps1 -SetupUrl 'https://github.com/vesta-project/server/releases/download/vesta-1.2.3/windows-x86_64-vesta-setup.exe' -SetupSha256 '<trusted setup sha256>' -BundleUrl 'https://github.com/vesta-project/server/releases/download/vesta-1.2.3' -ManifestSha256 '<trusted manifest sha256>' -ReleaseKey '<trusted 32-byte public key hex>' -AssetPrefix windows-x86_64-
```

Bash example:

```bash
bash scripts/bootstrap/install.sh --setup-url 'https://github.com/vesta-project/server/releases/download/vesta-1.2.3/linux-x86_64-vesta-setup' --setup-sha256 '<trusted setup sha256>' --bundle-url 'https://github.com/vesta-project/server/releases/download/vesta-1.2.3' --manifest-sha256 '<trusted manifest sha256>' --release-key '<trusted 32-byte public key hex>' --asset-prefix linux-x86_64-
```

For unattended deployment, add `--answers PATH --yes` (PowerShell: `-Answers PATH -Yes`). The installer must still apply its normal policy and license decisions from the provided answers and may reject missing or invalid consent; `--yes` does not grant consent. On Linux the helper connects installer stdin to `/dev/tty` when an interactive terminal is available, so it also works when the helper itself was piped to Bash. Both helpers use private temporary directories and remove only their own temporary download after execution. The downloaded setup binary is limited to 64 MiB and checked before execution.

The source helpers and generated one-paste commands create a private temporary workspace, download the pinned setup executable there, verify its checksum, then start it with the release metadata pins. They do not write into or require a writable current directory. No target package manager or Rust toolchain is required.

The same helpers support verified uninstall using `--uninstall` (PowerShell: `-Uninstall`) with the same setup and release pins. Full Vesta uninstall requires `--root HOST`. Removing one managed server uses `--server NAME --servers-root REGISTRY`; omit `--root` in that mode. Add `--plan` to preview. The helper forwards these options to signed native setup, which performs its confirmation and path validation. The helper refuses `--yes` for uninstall and does not remove files itself. See [uninstall behavior and recovery](uninstall.md).
