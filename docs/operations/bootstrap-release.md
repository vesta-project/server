# Signed setup bootstrap and release packaging

The setup bootstrap is a small source helper. It downloads the standalone `vesta-setup` executable only from an explicit HTTPS URL and checks its SHA-256 against a value supplied through a trusted channel before running it. That independently delivered checksum is what establishes the executable's trust. The signed release manifest is a second check performed by `vesta-setup`: it verifies the exact manifest bytes against the supplied manifest SHA-256 and validates `manifest.sig` with the Ed25519 release key pin.

Do not run an installer fetched from an untrusted mutable URL without independently obtained pins. HTTPS protects the connection but does not establish that a mutable server served an authorized executable. Deliver the setup URL, setup SHA-256, manifest SHA-256, and public key pin through a trusted release site or another authenticated channel. No production release is published by this repository.

## Build and package a release

Build the three standalone binaries from the source tree for one supported target. Packaging requires Node.js only and does not build Rust code. The binaries directory must contain exactly the named artifacts needed for the selected target: `vesta-setup`, `vesta-agent`, and `vesta-supervisor` on Linux; the same names with `.exe` on Windows. The packager accepts no arbitrary file paths and rejects symlinked binary inputs, mismatched host operating systems, and an existing output directory.

Create an Ed25519 PKCS#8 PEM private key in a protected signing environment. Keep the private key out of the binary directory, output directory, source control, logs, and release host. For example, use an approved key-management process to supply the key file, then run:

```powershell
node scripts/bootstrap/package-release.mjs --binaries .\path\to\release-binaries --output .\out\vesta-1.2.3-linux-x86_64 --target linux-x86_64 --base-url https://downloads.example.invalid/vesta/1.2.3/linux-x86_64/ --signing-key .\protected\release-ed25519.pk8.pem --version 1.2.3
```

Use `windows-x86_64` or `linux-aarch64` for the other supported targets. Run packaging on Windows for the Windows target and Linux for Linux targets. The output must be a new directory outside the input binaries directory. The release directory contains the three executable files, `manifest.json`, detached base64 `manifest.sig`, base64 raw public key `manifest.pub`, generated one-paste commands in `commands.txt`, and public pin metadata in `release.json`.

The manifest has exactly this shape, with `files` ordered setup, agent, supervisor:

```json
{"schema":1,"version":"1.2.3","target":"linux-x86_64","files":[{"name":"vesta-setup","sha256":"<64 lowercase hex>","size":123},{"name":"vesta-agent","sha256":"<64 lowercase hex>","size":123},{"name":"vesta-supervisor","sha256":"<64 lowercase hex>","size":123}]}
```

Publish the output directory as an immutable versioned HTTPS directory. The bundle URL passed to setup is the directory URL; setup retrieves `manifest.json`, `manifest.sig`, and the binaries from it. Verify the public pins through the trusted release process before distributing the commands. Do not publish or copy the private signing key.

## Bootstrap invocation

The PowerShell source helper is `scripts/bootstrap/install.ps1`; the Bash source helper is `scripts/bootstrap/install.sh`. Each requires explicit `SetupUrl`, `SetupSha256`, `BundleUrl`, `ManifestSha256`, and `ReleaseKey` inputs. `ReleaseKey` is 64 hex characters representing the 32-byte Ed25519 public key. The generated commands embed the setup checksum, manifest checksum, and key pin.

PowerShell example:

```powershell
& .\scripts\bootstrap\install.ps1 -SetupUrl 'https://downloads.example.invalid/vesta/1.2.3/windows-x86_64/vesta-setup.exe' -SetupSha256 '<trusted setup sha256>' -BundleUrl 'https://downloads.example.invalid/vesta/1.2.3/windows-x86_64' -ManifestSha256 '<trusted manifest sha256>' -ReleaseKey '<trusted 32-byte public key hex>'
```

Bash example:

```bash
bash scripts/bootstrap/install.sh --setup-url 'https://downloads.example.invalid/vesta/1.2.3/linux-x86_64/vesta-setup' --setup-sha256 '<trusted setup sha256>' --bundle-url 'https://downloads.example.invalid/vesta/1.2.3/linux-x86_64' --manifest-sha256 '<trusted manifest sha256>' --release-key '<trusted 32-byte public key hex>'
```

For unattended deployment, add `--answers PATH --yes` (PowerShell: `-Answers PATH -Yes`). The installer must still apply its normal policy and license decisions from the provided answers and may reject missing or invalid consent; `--yes` does not grant consent. On Linux the helper connects installer stdin to `/dev/tty` when an interactive terminal is available, so it also works when the helper itself was piped to Bash. Both helpers use private temporary directories and remove only their own temporary download after execution. The downloaded setup binary is limited to 64 MiB and checked before execution.

The generated one-paste commands create a private temporary workspace, download only the pinned setup executable there, verify its checksum, then start it with the release metadata pins. They do not write into or require a writable current directory. No target package manager or Rust toolchain is required.
