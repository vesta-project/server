# Signed update foundations

Starting checkpoint: server `3d1a9ae`, launcher `45624dde`. Scope is release trust,
metadata verification and download staging, not replacing running executables.

Reuse the setup release verifier in a shared `vesta-update` crate. Keep setup's
existing API compatible. Pin the release public key from the independently
verified installation; never adopt a downloaded public key. Release signing keys
are unrelated to host TLS, management devices, SFTP keys or hosted-pack signing.

Signed update metadata binds purpose, target, channel, sequence, version, manifest
hash, issuance and expiry. Persist monotonic sequence/hash and observed time before
staging; reject downgrade, same-sequence substitution, expiry and clock rollback.
Metadata cannot grant a new trusted key. Initial trust is local/explicit; key
rotation and revocation need a separate reviewed protocol.

Staging is serialized, storage bounded, streamed and side by side. Partial staging
never activates and can resume only for the same verified release. Record progress
durably, retain exact signed metadata, and reverify existing staged files. No
periodic polling, automatic restart, activation helper or remote management route
is claimed. Local setup commands make status/check/stage testable over SSH.

Add private-key generation tooling that writes an owner-only key without printing
it, emits a public pin, and refuses overwrite. Sign update descriptors during
packaging when an explicit release sequence/channel is supplied. Test tampering,
wrong key/channel/target, expiry, rollback, concurrent staging, interrupted staging
and preservation of active binaries. Production keys are not created or configured
in GitHub automatically.

## Implemented and verified

The shared verifier now lives in `crates/vesta-update`; setup reexports the old
release API. New metadata-bearing installations automatically persist the verified
release pin, channel and initial signed sequence. Explicit setup update commands
support local trust bootstrap, status, metadata checks and side-by-side staging.
CLI staging shares the installation/uninstall fence as well as a cross-process
update lock. The root lock was added during the second review to close a possible
uninstall/staging race. An extended Windows path-prefix validation failure found
by real staging tests was also fixed in the shared release reader.

Release packaging optionally emits signed update descriptors; nightly CI requests
them and the publisher verifies/retains them. Production key generation is an
explicit release-side command with fresh owner-only output, public-pin export,
and overwrite refusal. Production keys/secrets were not generated/configured.

Validation on Windows: setup + update crate tests: 26 passed; focused agent CLI:
1 passed; bootstrap Node tests: 19 passed, 2 Bash-only tests skipped. Scoped
all-target Clippy passes with the existing collapsible-if style exception.
Formatting and whitespace checks pass. Tests include signed metadata tampering,
context/expiry/clock/sequence failures, initial release binding, unchanged active
binaries, interrupted/resumed/reverified staging, lock contention, automatic setup
trust provisioning, CLI operation and private-key file/output handling.

Activation, coordinated handoff, health-check rollback, signed catalogue discovery,
key rotation/revocation, multi-role/threshold signing and remote management API
admission remain deliberately outside this foundation. No VM, production release,
running server, existing installation or GitHub settings were changed.
Operator guide: `docs/operations/signed-updates.md`.
