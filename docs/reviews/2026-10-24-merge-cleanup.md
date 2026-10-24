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
