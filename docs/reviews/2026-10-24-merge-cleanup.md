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

## Merge validation results

Local validation used Rust 1.96.0 on Windows x86_64 and Ubuntu 24.04 x86_64
under WSL2. A repository toolchain file now selects that same compiler and its
formatting/lint components. Builds used isolated caches; no release was published.

- Formatting, locked workspace/all-target checking and strict workspace/all-target
  Clippy pass on both operating systems, with no lint suppression added.
- Windows Node checks: 191 pass, two Bash runtime checks skipped. Ubuntu Node
  checks: all 193 pass. The 370-entry requirement ledger and generated roadmap
  checks pass. Roadmap generation now normalizes embedded Markdown line endings.
- Windows serial workspace tests were completed in two stages: the full control
  suite (977 library tests plus its integration targets), followed by the workspace
  excluding that already-tested package. The latter completed all targets,
  including all 114 HTTPS tests, without test failures. Changed SFTP and sleep
  regressions were rerun against the updated source.
- Ubuntu serial workspace tests used `--no-fail-fast`: the control library's 963
  tests and remaining targets ran to completion. Three targets initially failed;
  their fixes and follow-up coverage are recorded below. The final game-runtime
  package tests and supervisor endpoint regression pass. Final transport unit
  tests (46) and all five download-related HTTPS regressions pass on both systems.

This evidence combines the workspace runs and focused follow-up runs after fixes;
it is not a claim that a single final workspace invocation was rerun from scratch.
Windows evidence is in `V:\tmp\merge-*.log`; Linux evidence is under
`~/.local/share/vesta-linux-test-20261024/logs/merge-linux-*.log`.

## Issues resolved during the merge gates

- Native launch parser tests still rejected the JVM arguments now supported by
  software switching. Negative fixtures now reject invalid types; positive
  coverage verifies JVM argument order and required artifact paths.
- The retained-client supervisor test used Windows file opening for a Linux Unix
  socket. It now exercises the proper platform endpoint on each system.
- SFTP advertised file type bits without owner access bits. OpenSSH on Linux
  consequently materialized downloads without read permission. Virtual attributes
  now advertise owner-only access, with real OpenSSH interoperability coverage.
- The sleep fixture observed native exit before the durable sleep operation had
  finished. It now waits for the actual operation and reconciliation boundaries.
- A steady, throttled Linux download was cut short by the per-window throughput
  check during socket backpressure. The existing floor now carries at most one
  64-KiB chunk of burst credit between windows. A deterministic test proves that
  even a large burst cannot earn unlimited idle time; real stalled-download tests
  verify disconnection and slot release. Buffer and concurrency caps are unchanged.
- Compiler and Linux-only warnings were corrected without weakening strict CI.

## Readiness and limits

`origin/main` at `9a6882b` is an ancestor of the branch, and `git merge-tree`
reports no conflicts. Cleanup changes and fixes are committed; no merge or push
was performed. Historical migration evidence and canonical protocol fixtures are
retained intentionally. The Java sources were already extracted before this task;
the removed local directories were ignored leftovers, not tracked runtime code.

These checks support merging the native server branch for continued development.
They do not certify a release: GitHub-hosted CI and Linux ARM64 were not exercised
here, Minecraft gameplay was not started, and the separate launcher branch was
not validated for merge. Signed update activation/health rollback and operating
system isolation remain explicit product limitations in the README.
