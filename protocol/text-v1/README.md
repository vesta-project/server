# Historical Java text comparison fixture

The fixture and `scripts/JavaTextOracle.java` record the Java 21 behavior examined during
the initial migration draft. They are retained as comparison history, not as a Rust
production specification, active casing generator, or a requirement to reproduce JDK
whitespace, surrogate comparisons, case tables or timestamp formatting.

The owner corrected this approach on 2026-10-07: implement the Rust equivalent rather
than Java runtime emulation. The unexecuted Rust casing-generator scripts were removed
in the subsequent implementation increment. No generated Rust casing table was admitted.

See `docs/RUST_WRAPPER_MIGRATION_PLAN.md` and the current identity contract for the deliberate
Rust input policy and its unresolved Unicode file-scope gap. The independent capability /
built-in-role fixture under `protocol/identity-v1` and persisted audit-v1 byte contract
remain separate, justified contracts; this text fixture does not give authority or
establish scope matching, filesystem confinement, or platform support.
