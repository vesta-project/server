# Shared CraftBukkit and Spigot discovery checkpoint

Both targets now use `providers/buildtools.rs`, rather than separate provider
implementations. The backend identifiers are `bukkit` and `spigot`.

Implemented:

- Fixed first-party version index and concrete version metadata endpoints.
- Numeric version ordering; moving aliases and unsafe paths cannot be selected.
- Strict source commit-ID, build-ID, tool-version, Java-range and output-digest
  parsing. The same parser serves both targets; output hashes remain separate.
- Shared validated source documents through the agent catalogue cache.
- No build choices when the selected target lacks its published output digest.
- Explicit `buildtools_execution_pending` capability gate. Neither target is
  advertised as automatically installable. Plans refuse before any build effect.
- Metadata source policy rejects JAR downloads and arbitrary endpoints. No
  unofficial binary mirror is used.

The current implementation does not compile, install or activate either target.
This is the discovery/provenance foundation for the shared executor, not completed
BuildTools support. Claude's UI remains unchanged.

## Verification

- Full installer suite: 192 passed, 0 failed, 1 existing opt-in volume test ignored.
- Five focused BuildTools/descriptor tests passed, covering both targets, separate
  output hashes, missing digests, shared requests, malformed refs, source policy
  and refusing execution while gated.
- Software-service/cache regressions: 14 passed.
- Strict all-target installer/control Clippy passed.
- Entire server workspace, all targets: `cargo check --locked --workspace
  --all-targets` passed, including agent, setup and transport consumers.

Read-only live discovery succeeded for both targets. The first CraftBukkit query
returned 68 Minecraft revisions in 46 ms; its repeated query took 0.16 ms.
Spigot reused the index and took 0.71 ms. The concrete 26.3 metadata was shared
between the two release queries. These are observations on a healthy direct
connection, not an installation or low-end certification.

Logs: `V:/tmp/buildtools-full-installer-tests.log`,
`V:/tmp/buildtools-foundation-tests.log`, `V:/tmp/buildtools-foundation-clippy.log`,
`V:/tmp/buildtools-workspace-check.log`, `V:/tmp/buildtools-live-catalogues.log`.

## Executor work still required

Use existing process-tree ownership, bounded output, Java identity, disk
reservation and stopped activation. Add only the source-build-specific workspace
and Git preparation rules that those foundations do not currently provide.

Source inspection of official BuildTools commit
`5fc101051bb4bc32a40c8fb7d585db5ed5e97884` identified a candidate input-binding
route: prepare BuildData, Bukkit, CraftBukkit and Spigot repositories with local
`master` refs at the exact reviewed commits, then use `--dont-update` without
`--rev`, binding `bt.name`. Prove this invariant with a pinned tool binary and
isolated tests; do not infer it from the CLI documentation alone. BuildTools
internally checks out CraftBukkit's configured ref during patch processing, so
all relevant local refs must remain pinned, not merely the initial HEAD.

Still required: trusted tool/bootstrap resolution, private Git/Maven configuration,
bounded source checkout/build workspace, owned child cancellation, output and
runtime-layout validation, receipt-last publication, rollback and real Windows/
Ubuntu builds. A source-build process is not a sandbox. Do not silently expand
the existing ten-minute subprocess limit or advertise untested host support.
