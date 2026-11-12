# Signed setup bootstrap and release packaging

The setup bootstrap is a small source helper. It downloads the standalone `vesta-setup` executable only from an explicit HTTPS URL and checks its SHA-256 against a value supplied through a trusted channel before running it. That independently delivered checksum is what establishes the executable's trust. The signed release manifest is a second check performed by `vesta-setup`: it verifies the exact manifest bytes against the supplied manifest SHA-256 and validates `manifest.sig` with the Ed25519 release key pin.

Do not run an installer fetched from an untrusted mutable URL without independently obtained pins. HTTPS protects the connection but does not establish that a mutable server served an authorized executable. Deliver the setup URL, setup SHA-256, manifest SHA-256, and public key pin through a trusted release site or another authenticated channel. See [nightly release operations](nightly-releases.md) for the signed nightly publishing workflow.

## Build and package a release

Build the standalone binaries from the source tree for one supported target. Packaging requires Node.js only and does not build Rust code. New bundles require `vesta-setup`, `vesta-agent`, `vesta-supervisor`, and the short `vesta` CLI on Linux; the same names with `.exe` on Windows. Each artifact is covered by the signed manifest. The installer still accepts older signed three-component bundles. The packager accepts no arbitrary file paths and rejects symlinked inputs, incorrect executable formats or architectures, mismatched host operating systems, and an existing output directory. See [server-scoped management](scoped-cli.md).

Create an Ed25519 PKCS#8 PEM private key in a protected signing environment. Keep the private key out of the binary directory, output directory, source control, logs, and release host. For example, use an approved key-management process to supply the key file, then run:

```powershell
node scripts/bootstrap/package-release.mjs --binaries .\path\to\release-binaries --output .\out\vesta-1.2.3-linux-x86_64 --target linux-x86_64 --base-url https://github.com/vesta-project/server/releases/download/vesta-1.2.3 --asset-prefix linux-x86_64- --signing-key .\protected\release-ed25519.pk8.pem --version vesta-1.2.3
```

Use `windows-x86_64` or `linux-aarch64` for the other supported targets. Run packaging on Windows for the Windows target and Linux for Linux targets. The output must be a new directory outside the input binaries directory. The release directory contains the four executable files, `manifest.json`, detached base64 `manifest.sig`, base64 raw public key `manifest.pub`, generated one-paste commands in `commands.txt`, and public pin metadata in `release.json`.

The manifest has exactly this shape, with `files` ordered setup, agent, supervisor, CLI:

```json
{"schema":1,"version":"1.2.3","target":"linux-x86_64","files":[{"name":"vesta-setup","sha256":"<64 lowercase hex>","size":123},{"name":"vesta-agent","sha256":"<64 lowercase hex>","size":123},{"name":"vesta-supervisor","sha256":"<64 lowercase hex>","size":123},{"name":"vesta","sha256":"<64 lowercase hex>","size":123}]}
```

Publish each file as an immutable GitHub Release asset named `<target>-<logical filename>` (for example, `linux-x86_64-manifest.json`). Do not commit packages to a branch. The bundle URL passed to setup is the release download URL; setup retrieves prefixed assets while signed manifest logical filenames remain unchanged. Verify public pins through the trusted release process before distributing the commands. Do not publish or copy the private signing key.

## Bootstrap invocation

The PowerShell source helper is `scripts/bootstrap/install.ps1`; the Bash source helper is `scripts/bootstrap/install.sh`. Each requires explicit `SetupUrl`, `SetupSha256`, `BundleUrl`, `ManifestSha256`, and `ReleaseKey` inputs. `ReleaseKey` is 64 hex characters representing the 32-byte Ed25519 public key. The generated commands embed the setup checksum, manifest checksum, and key pin, and add `--asset-prefix` for flat release assets.

PowerShell example:

```powershell
& .\scripts\bootstrap\install.ps1 -SetupUrl 'https://github.com/vesta-project/server/releases/download/vesta-1.2.3/windows-x86_64-vesta-setup.exe' -SetupSha256 '<trusted setup sha256>' -BundleUrl 'https://github.com/vesta-project/server/releases/download/vesta-1.2.3' -ManifestSha256 '<trusted manifest sha256>' -ReleaseKey '<trusted 32-byte public key hex>' -AssetPrefix windows-x86_64-
```

Bash example:

```bash
bash scripts/bootstrap/install.sh --setup-url 'https://github.com/vesta-project/server/releases/download/vesta-1.2.3/linux-x86_64-vesta-setup' --setup-sha256 '<trusted setup sha256>' --bundle-url 'https://github.com/vesta-project/server/releases/download/vesta-1.2.3' --manifest-sha256 '<trusted manifest sha256>' --release-key '<trusted 32-byte public key hex>' --asset-prefix linux-x86_64-
```

For unattended deployment, add `--answers PATH --yes` (PowerShell: `-Answers PATH -Yes`). The installer must still apply its normal policy and license decisions from the provided answers and may reject missing or invalid consent; `--yes` does not grant consent. On Linux the helper connects installer stdin to `/dev/tty` when an interactive terminal is available, so it also works when the helper itself was piped to Bash. Both helpers use private temporary directories and remove only their own temporary download after execution. The downloaded setup binary is limited to 64 MiB and checked before execution.

The generated one-paste commands create a private temporary workspace, download only the pinned setup executable there, verify its checksum, then start it with the release metadata pins. They do not write into or require a writable current directory. No target package manager or Rust toolchain is required.

The same helpers support verified uninstall using `--uninstall` (PowerShell: `-Uninstall`) with the same setup and release pins. Full Vesta uninstall requires `--root HOST`. Removing one managed server uses `--server NAME --servers-root REGISTRY`; omit `--root` in that mode. Add `--plan` to preview. The helper forwards these options to signed native setup, which performs its confirmation and path validation. The helper refuses `--yes` for uninstall and does not remove files itself. See [uninstall behavior and recovery](uninstall.md).
