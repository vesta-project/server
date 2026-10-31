Canonical source: launcher crates/modpack-engine. Vendored for reproducible independent builds. Edit the launcher source then copy Cargo.toml and src here. Run checks/check-modpack-engine.ps1 -LauncherRoot <launcher checkout> to reject drift. No absolute build paths or third-party dependencies.

Incremental extraction only: classification, hash ownership, side filtering, override precedence. Full parser/manifest and config/world adapters still require migration; this does not establish full engine parity.
