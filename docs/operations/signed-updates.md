# Signed Vesta updates

Staging/checking remain independent from activation. OS-local activation is now implemented for the exact account-onboarded first-server consumer with clean stopped runtime/supervisor state and matching compiled schema/IPC. It never overwrites running binaries or grants a remote server Owner installation-wide update authority. Native Linux activation remains unverified; multi-server rolling activation and schema migration remain unsupported.

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
verification of all bytes. The native activator reverifies signatures, expiry, staged hashes and stopped ownership at execution. It never executes based solely on `phase=staged`.

These commands currently use OS-local authority. The check/stage operations do not
stop Minecraft or the agent and do not require a launcher connection. No live
Linux installation, remote activation or release publication is claimed yet.

## Fresh offline candidate verification

`vesta-setup update verify --root <installation> --manifest-sha256 <staged-manifest-hash>` re-verifies the saved signed update metadata, saved manifest signature/hash, current trust/expiry/sequence, and all four candidate binaries from protected single-link opened files. It uses no download source and does not execute, select, replace, or restart anything. The JSON observation includes version, sequence, target, timestamp, file/byte counts and `activation_supported: false`.

Staging now retains exact signed manifest bytes/signature in the protected update database. Older staged rows lacking that evidence refuse verification until explicitly re-staged from their valid signed source; the updater does not silently trust filenames or reconstruct unsigned evidence. A superseded candidate cannot bypass the accepted sequence floor. Same-size tampering, missing or linked files, modified proof and expired metadata refuse. Verify shares the installation/staging locks; active binaries remain untouched.

This observation is point-in-time diagnostics, not a portable activation authorisation. Future activation must revalidate authority, metadata and opened binaries at execution, preserve process ownership, and apply a coordinated health/recovery contract. Verification itself does not activate or restart; use the separate fenced activation command below.

## Opt-in startup discovery

The public `vesta update <action> ...` entry point now calls the exact same local
implementation as `vesta-setup update <action> ...`, including receipt validation,
installation lock and all trust checks. `vesta-agent` remains an executable alias
because existing startup records and signed artifact inventories still name it.
No shell or activation bypass is introduced. Stable installed CLI entrypoints dispatch only to the exact committed healthy payload. Bare `vesta update` prints the supported explicit actions.

After exact-installed-release trust bootstrap, an OS-local operator can configure
the installed setup host's next-start check:

```text
vesta-setup update configure --root <installation> --startup-check on --interval-seconds 21600 --bundle-url <operator-selected-https-release-directory> --asset-prefix linux-x86_64-
vesta-setup update status --root <installation>
vesta-setup update configure --root <installation> --startup-check off
```

The interval above is an example supplied by the operator, not a new default.
`--bundle-dir <absolute-directory>` supports offline discovery. No policy means
no automatic check. The existing pinned channel/key/target and accepted sequence
are unchanged; this does not implement channel switching or a signed catalog.
An immutable directory can only report its own signed release, so a future
channel catalog is still needed for unattended discovery of subsequent releases.

The installed host starts one background metadata-only attempt. It never waits
for network discovery before starting its readiness endpoint or existing agent.
The update lock serializes it with staging/configuration/verification; busy checks
are skipped. A durable pre-I/O admission spaces successful, failed and interrupted
attempts across process restarts. Local reconfiguration clears that observation,
allowing a deliberate immediate check without resetting trust. Clock rollback
does not bypass spacing. A lost directory or network/signature/expiry failure
records a failed observation, never a false current/succeeded release. Disabling
a saved policy remains possible when its local source is no longer available.

Status exposes `startup_policy` and `last_startup_check`: `incomplete`, `current`,
`available`, or `failed`, with a sanitized stable error classification. The
timestamp is admission time, not completion time. A failed persistence/lock/open
operation cannot produce a success observation. The service remains usable;
this slice does not add a notification bridge, daemon, periodic poller,
auto-stage/apply, restart, or remote machine-wide updater authority. HTTPS checks
use the existing operator-configured release transport and bounded metadata
reads/timeouts; public provider/artifact transport policy is unchanged.

The pure `vesta_update::rollout::preview_rollout` helper describes compatible
separate-supervisor handoff versus stopped maintenance and uncertainty/writes
that block or defer it. It consumes native observations, does not discover
consumers, and always reports `activation_supported: false`. Its output cannot
authorize execution. The installation registry, signed compatibility declaration,
stable dispatcher and mutation-fenced health trial remain activation prerequisites.

## Stopped activation and exact recovery

```text
vesta-setup update activate --root <installation> --manifest-sha256 <staged-hash> --idempotency-key <operator-intent> --restart on
vesta-setup update activate --root <installation> --manifest-sha256 <staged-hash> --idempotency-key <operator-intent> --restart off
vesta-setup update recover --root <installation> --activation-id <intent-id>
```

Stop the owned Vesta agent cleanly with its existing local shutdown control first. Unknown/live game generations, unclean session markers, unresolved writes/jobs/recovery, open database WAL and mismatched identity/configuration refuse activation; no process is killed to manufacture stopped proof. Legacy configurations without the canonical installation consumer do not gain implicit adoption/update authority. The exact original signed installed bundle proof must exist (or be explicitly supplied from the original pinned source) before fallback registration.

Packaging probes `update capabilities` on all four actual copied components using bounded local child commands. Only matching target/protocol/exact schema/supervisor IPC declarations become the signed optional `activation` metadata. All-legacy packages are staging-only; mixed/malformed declarations refuse packaging. The native trial verifies these declarations again and runs the held signed setup component?s restricted loopback HTTPS probe. It cannot open/migrate the game database, mutate management state or start Minecraft.

Activation retains verified executable handles and records trial -> restart_pending before normal execution. `--restart off` leaves that durable intent for the next `host --root`; `on` starts the verified bootstrap. Original installation binaries stay immutable; the selected setup component runs the selected agent/supervisor/CLI from its versioned payload. New normal agent readiness must bind the actual child PID, signed payload, server/TLS identity, schema/IPC and unchanged configuration/launch. A pinned real status request is also required. Only then does the transaction mark healthy and select the candidate.

A failed pre-readiness child may recover only after all actual stopped fences are reacquired. Recovery revalidates the exact prior committed signed build and health, never downloads an arbitrary older release, lowers the accepted sequence, restores a stale database or overwrites a world/config. Repeated same-intent submission/recovery is idempotent. No whole-pack rollback, accepted-job cancellation, generic process killing or post-health crash-loop rollback is promised. A live child with unconfirmed readiness blocks fallback; stop/recovery must establish ownership first.

The current compatibility boundary is intentionally exact compiled schema and supervisor IPC: a migration-capable future release requires a separately approved migration/recovery contract. Signed releases lacking activation declarations cannot be activated. Background startup discovery observes committed healthy selection, so the immutable original receipt does not falsely report an already-active release as available. Automatic channels/catalog discovery, periodic apply, key rotation and machine-wide multi-consumer updates remain separate work.

### Windows detached output capture

The fixed Windows bootstrap uses an explicit verified application path and CreateProcessW with handle inheritance disabled. It preserves the existing user, environment and job ownership; it does not overwrite executables, change tokens, break away from a job or launch a shell. Native UTF16 argument quoting preserves spaces, quotes, trailing backslashes and Unicode. Parent process/thread handles are closed without terminating the detached child.

Actual piped CLI capture is covered by the native signed restart-on/recovery acceptance test: output completes within its existing35s bound while the correct recovered service remains live and authenticates on its pinned TLS/server identity. The earlier file-output workaround is removed; failure cleanup gracefully stops only the owned test fixture. No runtime/game process killing or timeout weakening is used. Native Linux activation remains separately unverified.
