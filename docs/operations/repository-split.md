# Rust and Java repository boundary

The Rust agent and product specification remain in `vesta-project/server`.
`vesta-project/server-java` owns `core/`, `wrapper/`, Minecraft connector sources,
Gradle/Stonecutter builds, the loader version matrix, and Java deployment scripts.
The extraction preserves path-filtered Java history from source commit
`4fbc904a104d081a5e97dcbcf611f2f86873ecd7`. Original commit IDs remain available in
the source repository; filtered Java commit IDs differ.

## Independent builds and migration checks

Rust compilation and tests use checked-in protocol fixtures, not Java source
directories or a sibling checkout. Java builds also need no Rust checkout.

The migration inventory remains in `docs/RUST_WRAPPER_PARITY.json`. Its
`javaSource` field pins an exact Java repository commit. Run the checker against
that commit in a sibling `server-java` checkout, or set `VESTA_JAVA_REPO` to its
path. The checker requires the pinned HEAD and validates Java source byte hashes;
missing Java evidence fails rather than silently skipping the comparison. CI
checks out that exact revision. Java test references in the requirement ledger
use the prefix `server-java:` and retain their existing partial status.

The cross-repository comparison is a manual CI workflow and requires the
`SERVER_JAVA_READ_TOKEN` read credential because both repositories are private.
Ordinary Rust CI runs without that checkout and does not certify Java evidence.
Java sources use LF checkout bytes on both platforms. Where this changes the
old mixed-ending byte checksum, the ledger retains `preExtractionJavaSha256`;
only line endings changed, and no migration status was promoted.

`protocol/` remains canonical here. Java contains a snapshot with source commit
and file hashes in `protocol/SOURCE.json`. For a contract change, update and test
Rust first, explicitly copy the affected contracts into Java, refresh the snapshot
manifest, test Java, then update the Rust migration pin if appropriate. Do not
fetch mutable Java HEAD during ordinary Rust tests.

## Retained wrapper and local files

The Java wrapper remains available as migration reference. Retirement requires
resolved Rust host migration gaps, Windows/Linux lifecycle and recovery evidence,
live Minecraft compatibility, and connector builds independent of wrapper code.
This split does not claim those gates have passed.

Only tracked Java files were extracted. Existing local build caches, demo runtime
logs, server worlds, and installed servers were left in place. Historical product
audits remain here; Java source links now point to the Java repository. The legacy
Java installer documentation is owned by the Java repository and is not a Rust
agent deployment instruction.
