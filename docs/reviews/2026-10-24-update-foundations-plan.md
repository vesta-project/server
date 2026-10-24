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
