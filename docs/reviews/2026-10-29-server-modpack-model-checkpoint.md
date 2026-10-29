# Server modpack model checkpoint

This is an incomplete implementation checkpoint, not acceptance of the complete
2026-10-29 corrected design. Following-pack delivery, provider update discovery,
and the final unified instance review/apply lifecycle still need implementation.

Implemented:
- Durable optional server link, independent client offering configuration, and
  pack-owned paths/hashes. Install/apply always links the server; the compatibility
  set_as_modpack flag no longer publishes a client workspace.
- Plans expose added/updated/unchanged files and removed owned path/hash pairs.
  Update/removal effects use the restart content journal and its file preconditions
  and trash. Local edits block removals; user additions stay separate.
- Unlink keep/remove-owned routes and client-mode route. Custom stays independent;
  following configuration becomes none on unlink. Pending link state only becomes
  installed when every associated content intent is applied.
- Content inventory exposes modpack_owned. Management keeps custom history; explicit
  none suppresses player serving. server-modpack serving deliberately refuses with
  pack_unavailable until an independent offering release stream is implemented.

Migration:
- New unpublished workspaces persist none as their default.
- Existing releases/provider pointers persist as custom client offerings. No server
  installation or ownership is inferred without a recorded installation receipt.
- Draft publication does not implicitly enable player offering; explicitly choose
  custom using POST /api/v3/manage/pack/client-mode.
- New tables are additive and initialized idempotently by PackWorkspace::open.
- Existing plan snapshots can conflict and require review again. The legacy client
  publication assignment step has been removed.

Routes:
- GET /api/v3/manage/pack/server-link
- POST /api/v3/manage/pack/install/plan and /apply (server link/update)
- POST /api/v3/manage/pack/server-link/unlink {remove_owned_files}
- POST /api/v3/manage/pack/client-mode {mode,require_update}
- GET /api/v3/manage/pack/draft additionally returns server_modpack and client_pack.

Validation:
- Focused real file-journal update/unlink and modified-file-removal tests pass.
- Independent mode and legacy provider migration regressions pass (four tests total).
- All-target clippy for control, transport, agent and vesta-dev passed with -D warnings.
- The original whole control/transport runs started before final regression additions;
  final tests use a separately named Rust test executable to avoid replacing an
  executable still running on Windows. The stale serial control test process was
  stopped after verifying its exact executable/arguments and superseded with the
  current complete suite at eight test threads. No runtime agent was stopped.
- Current complete control unit suite: 1,073 passed, one failed, two ignored.
  The failure is the same original-v8 host migration fixture reported before this
  rework, with state_migration_failed. No migration-fixture repair is claimed.
- All six control integration targets passed (28 tests).
- Transport: 47 unit tests passed; initial HTTPS run 121 passed, one failed, one
  manual ignored. Its sole failure expected publication to imply player offering;
  the updated test explicitly checks default-none and selects custom, then passes
  signed status, artifact download, policy and freshness assertions.
- Full vesta-agent suite: 77 passed; vesta-dev: eight passed, one manual ignored.
  The old check-config success fixture was corrected in dc55aff to supply an
  existing inert launch. The command still executes no game process.
- Launcher/client validation details are recorded in the linked handoff. Desktop
  typecheck and 22 focused pack checks passed. The expanded frontend run had one
  console HOT_SCOPE_TIME failure, which passed with its whole file in isolation.

Launcher implementation/handoff: feat/frontend-rewrite commit 3d3a3ab9, in
apps/desktop-solid/src/features/servers/MODPACK-REDESIGN.md. No pages edited and no
unrelated launcher changes staged. Server source metadata uses the existing
instance modpack fields; protected TLS/high-water receipts remain in the adapter.
The adapter is routed through common update checks, but its lifecycle is not yet
fully merged. Required updates continue blocking launch.
