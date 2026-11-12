# Nightly signed releases

Nightlies now include signed sequence/expiry update descriptors. See
[signed update foundations](signed-updates.md) for key generation, trust bootstrap,
check/stage commands and the remaining activation boundary.

`.github/workflows/ci.yml` checks main pushes, pull requests, and manual validation. Feature branches run through their PR rather than duplicate push and PR runs. `.github/workflows/nightly.yml` builds and publishes only after a new commit is pushed to `main` in the canonical repository. It has no scheduled, pull-request or manual publishing trigger. Re-running an already published source commit skips that product.

The shared change classifier selects checks and publications. Shell-script-only and documentation changes run Node contracts without building or publishing binaries. Installer-view changes test setup and publish an independent installer; shared host/runtime and unknown changes retain full Rust checks and publish both products. Packaging changes exercise setup and update verification. CI-configuration changes retain the full check matrix without themselves requiring a binary release. The required Rust status names remain present even when no Rust checks apply.

For affected products the release pipeline runs Node contracts and Rust formatting, check, test, and Clippy first. Native Windows x86_64, Linux x86_64, and Linux ARM64 runners build either setup alone or all four server executables. Build jobs verify PE/ELF format and target architecture before upload. Separate packaging jobs require the production signing key. The final publisher rechecks every executable hash and Ed25519 signature against the pinned public key, for every requested product and platform, before publishing anything. It reuses the same signing/staging implementation for installer and server products.

CI restores Rust caches opportunistically, but only successful main push jobs may save them. PRs cannot write trusted caches; release builds do not consume these caches. Cache failures do not suppress mandatory checks. Full workspace validation still runs for shared/configuration changes, unknown paths and manual checks. This reduces redundant work, not the assertions or security boundaries of the selected suites. See [testing](../../TESTING.md).

## Required GitHub configuration

Create an Actions environment named `release-signing` and configure:

- Environment secret `VESTA_RELEASE_SIGNING_KEY`: the Ed25519 PKCS#8 PEM private key used for manifests. Store the PEM text as a secret. The workflow fails when it is absent; it has no generated-key or unsigned fallback.
- Repository variable `VESTA_RELEASE_PUBLIC_KEY`: the corresponding raw 32-byte Ed25519 public key encoded as 64 hex characters. This is a public pin. Every native package and the publisher compare the generated key to it before publication.

The signing preflight and publisher also require this variable to match the public key embedded in the trusted `scripts/bootstrap/install.sh` and the native installer's `scripts/bootstrap/release-public-key.txt`. Changing a signing key requires a coordinated source and trusted-installer publication; changing GitHub configuration alone intentionally blocks the release. Downloaded release metadata cannot add trusted keys. An older key's releases need the corresponding reviewed bootstrap revision or independently trusted explicit pins; this check does not introduce automatic key rotation.

Restrict the environment to `main`, require reviewers if that matches the repository's release policy, and protect the source branch. The publishing jobs do not run for pull requests or forks. Build and test jobs never receive the private key. The signing jobs run only after the checks and native builds have completed and do not run the Rust build commands.

The final job needs `contents: write` to create a GitHub Release. The repository or organization Actions policy must allow this job permission. All published files are GitHub Release assets; the workflow does not write package binaries to a Git branch.

## Release layout and identity

Server packages use `nightly-YYYYMMDD-<12 character commit SHA>` as manifest version and GitHub Release tag. Independent installers use `installer-YYYYMMDD-<12 character commit SHA>`. Preflight fetches all release-history pages and compares main against each product's latest published tag commit independently. Skipped/failed intermediate pushes are therefore included even when the current push only changes docs. Missing or non-ancestor baselines conservatively require full validation/publication of the affected product; a failed diff stops planning. Deduplication checks all published tags, including older dates, so completed server publication does not hide a missing installer publication or allow a duplicate installer for the same source. A workflow-wide lock serializes publication; existing tags or releases are never replaced. Every requested target/product must finish signing and verification first. Jobs have explicit timeouts and short build-artifact retention.

The signed assets use flat target-prefixed names at the GitHub Release URL:

```text
https://github.com/vesta-project/server/releases/download/<tag>/<target>-<logical-filename>
```

For example, `linux-x86_64-manifest.json` and `linux-x86_64-vesta-setup` use the unchanged logical manifest names `manifest.json` and `vesta-setup`. The release has each target's setup binary, agent, supervisor, CLI, `manifest.json`, detached `manifest.sig`, public `manifest.pub`, `release.json`, and generated one-paste commands, plus a convenience `.tar.gz` per target. Bootstrap passes the exact supported target prefix so native setup can fetch prefixed assets while checking the original signed logical filenames.

Independent installer releases instead contain six files per target: setup, `installer.json`, `installer.sig`, `installer.pub`, `bootstrap.txt`, and `bootstrap.sig`. Their signed purpose/schema differ from server packages. The source script fetches an installer independently; that native UI selects Stable, Nightly, or an exact server tag and authenticates its server manifest using the shared verifier. The chosen package supplies its own matched host/runtime binaries. An installer-only release never updates installed servers. Stable selection requires a published stable package; this workflow does not create stable releases. Expired signed update metadata still fails the existing install policy, including for old exact tags.

The `commands.txt` entries pin the setup executable SHA-256, exact manifest SHA-256, and release public key. Linux source-script installation instead uses signed `bootstrap.txt`/`bootstrap.sig` assets, containing the exact setup and manifest identities, so it requires no JSON runtime. Packaging and the publisher verify these against the same manifest; new releases require both descriptor files. The source script embeds the trusted key and checks the signature before executing setup. Review the GitHub Release and its source commit before distributing any command. Consumers should receive the script or all pins through an authenticated channel. The nightly label identifies cadence; signatures and independently delivered trust pins establish package integrity.

## Operations and recovery

Configure the repository secret `CURSEFORGE_API_KEY` for the native build job.
Release builds refuse a missing key using `VESTA_REQUIRE_PROVIDER_KEYS=1`.
The application key is embedded in agent binaries, as in the launcher, and can
be recovered from them. Raw env files are not part of the release assets.
Pull-request validation receives no provider secret and works without a key.

Publish through a reviewed new commit on `main`; there is no manual nightly publishing action. Use the CI workflow's manual dispatch for validation without publishing.

The publisher copies only the verified staging files and rebuilds the convenience archives from those files. It compares bootstrap commands against the canonical generator for the verified release pins and restores Linux executable permissions lost during artifact transport. It uploads all targets into a draft prerelease, then publishes it after successful upload. It does not mark a nightly as the latest stable release. Enable GitHub's immutable-release setting where available to add repository-enforced protection; refusal to overwrite in the workflow does not restrict an administrator's separate actions.

The publishing workflow never overwrites an existing tag or release. If a run stops during asset upload, inspect the Actions log, Git tag, and draft GitHub Release before retrying. Do not delete an existing publication to make a retry pass; investigate and reconcile the run with the same commit and pins under the repository's release process. Re-running a completed run skips an already published source commit, or stops at the existing-publication guard. A later run with a new source commit receives a new identity.

There is no scheduled publishing trigger. See [bootstrap packaging and trust](bootstrap-release.md) for local packaging and source helper details, including bounded public discovery and the explicit-pin Windows PowerShell helper.
