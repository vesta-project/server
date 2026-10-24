# Nightly signed releases

Nightlies now include signed sequence/expiry update descriptors. See
[signed update foundations](signed-updates.md) for key generation, trust bootstrap,
check/stage commands and the remaining activation boundary.

`.github/workflows/ci.yml` runs checks for pushes, pull requests, and manual validation. `.github/workflows/nightly.yml` runs a native build and signed publish when supported source changes are pushed to `main` or `feat/launcher-compat`, on a daily UTC schedule, or by manual dispatch from either approved branch. GitHub only starts scheduled workflows from the repository's default branch. The scheduled run begins after this workflow reaches that branch; pushes to the current integration branch can publish nightlies before then.

The nightly pipeline runs Node checks and the Rust formatting, check, test, and Clippy suite first. It then builds all four executables on native Windows x86_64, Linux x86_64, and Linux ARM64 runners. The build jobs verify the PE/ELF file format and target architecture before uploading unsigned binaries. Separate native packaging jobs download those products, require the configured production signing key, and sign each release manifest. A final publisher rechecks every executable hash and Ed25519 signature against the pinned public key before publishing.

## Required GitHub configuration

Create an Actions environment named `release-signing` and configure:

- Environment secret `VESTA_RELEASE_SIGNING_KEY`: the Ed25519 PKCS#8 PEM private key used for manifests. Store the PEM text as a secret. The workflow fails when it is absent; it has no generated-key or unsigned fallback.
- Repository variable `VESTA_RELEASE_PUBLIC_KEY`: the corresponding raw 32-byte Ed25519 public key encoded as 64 hex characters. This is a public pin. Every native package and the publisher compare the generated key to it before publication.

Restrict the environment to `main` and `feat/launcher-compat`, require reviewers if that matches the repository's release policy, and protect both source branches. The publishing jobs do not run for pull requests or forks. Build and test jobs never receive the private key. The signing jobs run only after the checks and native builds have completed and do not run the Rust build commands.

The final job needs `contents: write` to create a GitHub Release. The repository or organization Actions policy must allow this job permission. All published files are GitHub Release assets; the workflow does not write package binaries to a Git branch.

## Release layout and identity

Each run uses `nightly-YYYYMMDD-<12 character commit SHA>` as both the manifest version and GitHub Release tag. The tag is unique per UTC date and source commit. A preflight skips rebuilding a source commit already represented in the most recent 100 published releases. A single workflow-wide concurrency lock serializes publication across approved branches; an existing Git tag or GitHub Release causes the publisher to stop without replacing assets. Every target must finish signing and verification before the publisher can run. Jobs have explicit timeouts; build artifacts have short retention periods.

The signed assets use flat target-prefixed names at the GitHub Release URL:

```text
https://github.com/vesta-project/server/releases/download/<tag>/<target>-<logical-filename>
```

For example, `linux-x86_64-manifest.json` and `linux-x86_64-vesta-setup` use the unchanged logical manifest names `manifest.json` and `vesta-setup`. The release has each target's setup binary, agent, supervisor, CLI, `manifest.json`, detached `manifest.sig`, public `manifest.pub`, `release.json`, and generated one-paste commands, plus a convenience `.tar.gz` per target. Bootstrap passes the exact supported target prefix so native setup can fetch prefixed assets while checking the original signed logical filenames.

The `commands.txt` entries pin the setup executable SHA-256, exact manifest SHA-256, and release public key. Review the GitHub Release and its source commit before distributing any command. Consumers should receive the command or all pins through an authenticated channel. The nightly label identifies cadence; the signed manifest and independently delivered pins establish package integrity.

## Operations and recovery

Use **Actions → Nightly signed releases → Run workflow** on `main` or `feat/launcher-compat` for an intentional publishing run. The run still executes full validation and all three target builds before signing. Use the CI workflow's manual dispatch for validation without publishing.

The publisher copies only the verified staging files and rebuilds the convenience archives from those files. It compares bootstrap commands against the canonical generator for the verified release pins and restores Linux executable permissions lost during artifact transport. It uploads all targets into a draft prerelease, then publishes it after successful upload. It does not mark a nightly as the latest stable release. Enable GitHub's immutable-release setting where available to add repository-enforced protection; refusal to overwrite in the workflow does not restrict an administrator's separate actions.

The publishing workflow never overwrites an existing tag or release. If a run stops during asset upload, inspect the Actions log, Git tag, and draft GitHub Release before retrying. Do not delete an existing publication to make a retry pass; investigate and reconcile the run with the same commit and pins under the repository's release process. Re-running a completed run skips an already published source commit, or stops at the existing-publication guard. A later run with a new source commit receives a new identity.

The schedule is present in this branch, but GitHub will not execute scheduled runs until the workflow exists on the repository's default branch. See [bootstrap packaging and trust](bootstrap-release.md) for local packaging and source helper details.
