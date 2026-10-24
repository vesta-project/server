# Server repository cleanup and merge preparation

Starting checkpoint: `c688de1`; recovery branch:
`checkpoint/pre-cleanup-20261024`. Scope is the server repository on
`feat/launcher-compat`. The launcher repository is separate; its active bundled
Java exit helper is still required and is not part of this removal.

The tracked server tree already contains no Java sources, Gradle builds or JARs.
Ignored Java build/cache directories and old local demo/test outputs remain from
before extraction. Preserve those outside the checkout using validated,
same-volume directory moves; retain demo worlds and review logs rather than
destroying them. Do not clean Rust target directories or active dev-server data.

Refresh Rust entry-point documentation, remove obsolete Java-only release filters
and the manual private-repository migration workflow, and clearly label retained
migration evidence as historical. Keep canonical protocol contracts, requirement
traceability and optional Java connector references. Java remains the runtime for
Minecraft; this cleanup does not remove server software installation support.

Fetch main for a current merge baseline, then run the actual CI checks on the
cleaned tree: formatting, all-target check, serial workspace tests, strict Clippy,
Node checks, requirements ledger and generated roadmap. Use the existing isolated
Ubuntu WSL tool/cache with the CI-pinned compiler for Linux evidence. Record any
remaining limitations without promoting requirement completion. Commit cleanup;
do not merge, push, rewrite main, publish or modify the other main worktree.

## Local preservation

The old `.gradle`, `build`, `buildSrc`, `core`, `wrapper`, `versions`, `gradle`
and `demo-server` directories were moved to
`V:\repository-cleanup-archive\server-20261024`. No tracked file was inside those
directories. Before/after file counts and total lengths matched; `manifest.json`
records the source/destination inventory and recovery commit. The archive parent
is restricted to the current Windows account. Demo worlds and all root-level
review logs were preserved. This is a same-volume move, not a disk-space recovery
claim. Rust `target`, editor settings and active development instances were not
removed. No running Java process referenced the checkout when the move began.

The fetched server `origin/main` is an ancestor of this branch, so the merge has
no divergent main commits to reconcile. This does not itself establish passing
CI; complete gate results follow below. The separate launcher rewrite was not
prepared for merge by this task.
