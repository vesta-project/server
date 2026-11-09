# Signed update foundations

The updater currently verifies trust/metadata and stages packages. It does **not**
replace running executables, restart agents/supervisors, migrate active state,
activate, roll back installed Vesta binaries or expose remote update API routes.
`vesta update` still refuses replacement and points to the staging commands.

## Keys and initial trust

Release signatures use Ed25519 through the existing ring verifier. The private
release key belongs to the release/signing environment, never a game host. Its
public pin is safe to distribute. This key is separate from host TLS, management
device keys, SFTP host/user keys, and future hosted-pack signatures.

Generate a production key only on the intended signing machine, into a new
directory **outside source control**, using an absolute path:

```text
node scripts/bootstrap/generate-release-key.mjs --output <new-private-directory>
```

The tool creates a fresh owner-only directory (Unix permissions or Windows ACL),
writes an Ed25519 PKCS#8 private PEM plus public PEM/hex, and prints only the public
pin/fingerprint. Existing paths are refused. Back up the private key securely;
losing it currently requires an explicit out-of-band trust migration. Never paste
it into chat, commit it, or ship it with a bundle. Configure GitHub's existing
`release-signing` environment with `VESTA_RELEASE_SIGNING_KEY`, and the matching
public `VESTA_RELEASE_PUBLIC_KEY` repository variable through trusted secret entry.
No production key or GitHub secret was created by this implementation.

New setup bundles containing update metadata automatically pin the independently
supplied release public key and the installed release's signed sequence. No key
downloaded as `manifest.pub` is adopted as authority. Older setup-only bundles
remain supported but do not automatically gain update trust.

For deliberate local initialization, the supplied bundle must contain signed
update metadata for the **exact installed manifest** in `private/setup.json`:

```text
vesta-setup update init --root <installation> --release-key <trusted-public-hex> --channel nightly --bundle-dir <installed-release-bundle>
```

The public pin must come from an independently trusted channel. Matching a public
manifest hash alone does not prove that a newly supplied key is authoritative.
Initialization never replaces an existing key/channel or resets its sequence floor.
Trust is stored under `private/update-state` with OS-restricted state storage.

## Metadata and packaging

Packaging supports `--channel nightly|stable --sequence <positive-integer>` in
addition to the existing release options. Both options are required together.
It emits `update.json` plus detached base64 `update.sig`. Metadata contains:

```json
{"schema":1,"purpose":"vesta-update-release-v1","channel":"nightly","sequence":42,"version":"nightly-example","target":"linux-x86_64","manifest_sha256":"<64 hex>","issued_at_ms":1800000000000,"expires_at_ms":1800604800000}
```

The signature covers the exact JSON bytes; the purpose is part of those bytes.
The manifest hash binds the artifact inventory and each binary's hash/size.
Packaging sets a seven-day expiry; the verifier caps validity at 30 days, permits
up to five minutes of issuance clock skew, and refuses expired metadata or a clock
earlier than its persisted accepted observation. A wrong host clock fails closed.
There is no external trusted-time service or freeze-proof offline installation.

Nightly packaging uses the workflow run number as sequence. Preserve its monotonic
ordering within the channel/target; do not reset the workflow counter or reuse a
sequence for different bytes. Any future stable release workflow needs its own
deliberate sequence allocation. All supported targets receive prefixed update
assets, which the isolated publisher verifies and includes in release archives.
Legacy packages without update assets remain installer-compatible.

Version labels are presentation, never ordering authority. Lower sequences and
different metadata at the same sequence are refused even when correctly signed.
Key rotation/revocation, multiple signing roles/thresholds and signed catalogue
discovery remain future work; this is **not a TUF implementation**. The design
uses the established [rollback/expiry threat model](https://theupdateframework.github.io/specification/v1.0.36/)
and standard [Ed25519 signatures](https://www.rfc-editor.org/rfc/rfc8032.html).

## Check and stage over SSH

```text
vesta-setup update status --root <installation>
vesta-setup update check --root <installation> --bundle-url <immutable-https-release-directory> --asset-prefix linux-x86_64-
vesta-setup update stage --root <installation> --bundle-url <immutable-https-release-directory> --asset-prefix linux-x86_64-
```

`--bundle-dir` is the local/offline alternative to `--bundle-url`. Commands return
JSON. Check does not fetch binaries, advance the accepted sequence, or start a
poller. Stage streams fixed-size chunks, validates size/hash, and records the
accepted sequence before writing candidate binaries. An OS lock serializes
staging across processes. Four retained candidate bundles and per-release size
bounds cap staging; free-space preflight preserves 64 MiB headroom. There is no
automatic candidate deletion yet; a full store refuses additional candidates.

Candidates live beside update state, never in the active installation's `bin`.
Interrupted stages remain `staging`; retrying the same signed release checks
completed files and resumes missing artifacts. Tampered existing files refuse
instead of being overwritten. Status is the last recorded outcome, not fresh
verification of all bytes. An eventual activator must reverify signatures, expiry,
all staged hashes and actor approval, then coordinate a durable process handoff
and health-check/rollback. It must never execute based solely on `phase=staged`.

These commands currently use OS-local authority. The check/stage operations do not
stop Minecraft or the agent and do not require a launcher connection. No live
Linux installation, remote activation or release publication is claimed yet.

## Fresh offline candidate verification

`vesta-setup update verify --root <installation> --manifest-sha256 <staged-manifest-hash>` re-verifies the saved signed update metadata, saved manifest signature/hash, current trust/expiry/sequence, and all four candidate binaries from protected single-link opened files. It uses no download source and does not execute, select, replace, or restart anything. The JSON observation includes version, sequence, target, timestamp, file/byte counts and `activation_supported: false`.

Staging now retains exact signed manifest bytes/signature in the protected update database. Older staged rows lacking that evidence refuse verification until explicitly re-staged from their valid signed source; the updater does not silently trust filenames or reconstruct unsigned evidence. A superseded candidate cannot bypass the accepted sequence floor. Same-size tampering, missing or linked files, modified proof and expired metadata refuse. Verify shares the installation/staging locks; active binaries remain untouched.

This observation is point-in-time diagnostics, not a portable activation authorisation. Future activation must revalidate authority, metadata and opened binaries at execution, preserve process ownership, and apply a coordinated health/recovery contract. This command does not implement self-update activation or automatic rollout.
