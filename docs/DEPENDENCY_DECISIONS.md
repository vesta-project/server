# Dependency intake decisions

Date: **2026-10-04**. This records the foundation batch's new dependency; it is not a complete license, SBOM, vulnerability, or signed-distribution audit.

## SQLite JDBC — accepted for agent-only durable state

- Coordinate: `org.xerial:sqlite-jdbc:3.53.4.0`, pinned in `wrapper/build.gradle.kts`.
- Scope: Java 17 wrapper/agent only. Java 8 shared `core` and connector dependency graphs do not acquire this JDBC dependency.
- Reason: use real SQLite transactions/WAL, foreign keys, integrity checking, schema versioning, and the driver's supported online backup facility; a hand-written JSON store cannot meet the accepted crash-safety contract.
- Alternative not chosen: copy the live database/WAL directly before migration. A supported online backup is used and independently opened/checked instead.
- License evidence: the resolved Maven POM declares **Apache License 2.0**, linking `https://www.apache.org/licenses/LICENSE-2.0.txt`; upstream is `https://github.com/xerial/sqlite-jdbc`.
- The cached JAR SHA-256 observed in this environment was `bcb1f51e36f940867e83342f9efbf5968ac44a6bef4d397bb4af7b17b45cd2fb`. This records local intake only; it is **not** a trusted signature or Gradle dependency-verification lock.
- A case-insensitive inspection of this cached JAR found no `license` or `notice` filename. Do not infer that distribution notice obligations have been satisfied from the POM alone. Release packaging must obtain and preserve applicable driver/native licenses and notices and verify the packaged artifact against trusted release provenance.

### Operational implications

- The driver includes/loads native SQLite code. Owner-controlled native extraction/loading, supported CPU/OS targets, temp-directory permissions, service identities, executable policy, and runtime native-access behavior need clean Windows/Linux packaging tests. The database's owner-only ACL does not certify the native loader or isolate a game under the same OS account.
- State must remain on a supported local filesystem outside the live game/file-browser root. Linked database/WAL/SHM/journal/backups children reject, but Java checks retain TOCTOU limitations. Native/local-filesystem enforcement and platform support remain open release gates.
- SQLite schema support is currently version 1. Newer schemas reject; migration gets an online backup and integrity checks. Tests cover rollback/corruption/reopen/foreign keys, not arbitrary future migration compatibility or downgrade support.
- Retention, reserved audit capacity, disk-full behavior across all callers, long-running executor storage, and complete backup/export/recovery are not implemented by adding JDBC.

### Pending release intake

1. Generate the full runtime/native/transitive SBOM and dependency verification metadata.
2. Verify trusted artifact provenance and vulnerabilities at the approved release cutoff; establish an update policy without silently changing pins.
3. Collect applicable driver/native/transitive notices in distributable bundles and review project-license compatibility.
4. Prove native loading and WAL/ACL/filesystem behavior under clean supported Windows and Linux service accounts, including failed permissions, disk-full, and recovery drills.
5. Repeat dependency intake for planned restic, ACME/SFTP, installer/service integration, and signed update tooling before enabling those adapters.

The existing BouncyCastle dependency already supplies Argon2id/Ed25519 primitives. This checkpoint does not add a second crypto library or implement the vault/mTLS/signature contract; existing bcrypt-backed MVP authentication remains pending replacement under P2.
