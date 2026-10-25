# Catalogue and Windows development restart follow-up

The failed independent-agent launch occurs during preflight, before the old
agent is stopped. Cargo's Windows process job can forbid breakaway; detaching a
console alone does not escape that lifetime. Do not bypass the safety guard or
force-kill an agent. Start `scripts/dev-shell.ps1` in a standalone PowerShell
terminal instead of `cargo dev`: Cargo builds the helper and exits before the
helper opens the development shell. Existing shells must be exited first.
Other enclosing jobs can still prevent breakaway; this is intentionally refused.

Read-only live catalogue diagnostics independently reproduced three defects:

- Fabric historical labels containing spaces poisoned otherwise valid lists.
  They are now excluded from selectable identifiers after bounded raw validation.
- Quilt v0 returned HTTP 404. Quilt now uses v1 version groups and selects the
  group belonging to the requested Minecraft version. URL policy stays scoped
  to the exact trusted host and known version-group/loader-version path.
- NeoForge and Quilt mirrors mark numeric loader releases `stable:false`.
  Numeric release IDs are accepted; beta/RC suffixes remain excluded. NeoForge
  release/plan selection still intersects the official Maven catalogue, and
  Quilt planning still verifies the official server profile and artifact pins.

Validation: 28 loader fixture tests and the NeoForge stability regression pass;
the Windows wrapper builds the helper and forwards `--help` successfully.
Live metadata-only checks return Minecraft version counts: Vanilla 908, Paper
68, Folia 13, Purpur 41, Fabric 520, Quilt 441, Forge 57, NeoForge 15. Latest
listed Quilt/Forge/NeoForge choices returned 53/13/21 loader releases.
These observations are time-dependent; `cargo run --locked -p
vesta-server-install --example catalogues` reproduces the diagnostic without
installing or executing server software. It creates only an owned temporary
empty root. No live agent or Minecraft process was restarted by this work.

References:
- Cargo process job rationale: https://doc.rust-lang.org/stable/nightly-rustc/cargo/util/job/index.html
- Windows job lifetime/breakaway: https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects
- Current Quilt mirror: https://launcher-meta.modrinth.com/quilt/v1/manifest.json
- Current NeoForge mirror: https://launcher-meta.modrinth.com/neo/v0/manifest.json
