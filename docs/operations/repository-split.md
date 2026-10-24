# Rust and Java repository boundary

The Rust agent and product specification remain in `vesta-project/server`.
`vesta-project/server-java` owns `core/`, `wrapper/`, Minecraft connector sources,
Gradle/Stonecutter builds, the loader version matrix, and Java deployment scripts.
The extraction preserves path-filtered Java history from source commit
`4fbc904a104d081a5e97dcbcf611f2f86873ecd7`. Original commit IDs remain available in
the source repository; filtered Java commit IDs differ.

## Independent builds and historical migration checks

Rust compilation and tests use checked-in protocol fixtures, not Java source
directories or a sibling checkout. Java builds also need no Rust checkout.

The migration inventory remains in `docs/RUST_WRAPPER_PARITY.json`. Its
`javaSource` field pins an exact Java repository commit. Run the checker against
that commit in a sibling `server-java` checkout, or set `VESTA_JAVA_REPO` to its
path. The checker requires the pinned HEAD and validates Java source byte hashes;
missing Java evidence fails rather than silently skipping the comparison. Java
test references in the requirement ledger
use the prefix `server-java:` and retain their existing partial status.

The old manual cross-repository CI workflow was retired during merge cleanup.
An explicit historical comparison can still be run with
`node scripts/rust-wrapper-parity.mjs` against the pinned external checkout.
Ordinary Rust CI needs no Java checkout or private-repository credential and does
not certify Java evidence.
Java sources use LF checkout bytes on both platforms. Where this changes the
old mixed-ending byte checksum, the ledger retains `preExtractionJavaSha256`;
only line endings changed, and no migration status was promoted.

`protocol/` remains canonical here. Java contains a snapshot with source commit
and file hashes in `protocol/SOURCE.json`. For a contract change, update and test
Rust first, explicitly copy the affected contracts into Java, refresh the snapshot
manifest, test Java, then update the Rust migration pin if appropriate. Do not
fetch mutable Java HEAD during ordinary Rust tests.

## Legacy wrapper and local files

The Java wrapper remains in the independent Java repository as historical
reference. It is not built, shipped or launched by the native server repository.
Optional Java connector work remains independent. Removing old local outputs
does not certify all migration requirements, live Minecraft compatibility or
deployment isolation.

Only tracked Java files were extracted initially. The 2026-10-24 cleanup preserved
leftover Java build/cache directories, demo worlds and review logs outside the
checkout; details are in [the cleanup record](../reviews/2026-10-24-merge-cleanup.md).
Active development installations and Rust build outputs were left intact. Historical product
audits remain here; Java source links now point to the Java repository. The legacy
Java installer documentation is owned by the Java repository and is not a Rust
agent deployment instruction.
