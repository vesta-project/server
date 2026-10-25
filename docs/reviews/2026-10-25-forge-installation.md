# Shared Forge/NeoForge installation checkpoint

Implementation, not installation certification. The owner explicitly deferred test
runs and real installation runs. No test suite, upstream installer, processor,
Minecraft instance or EULA acceptance was executed for this change.

## Implemented

- One `providers/forge` pipeline, shared by both providers: catalogue, profile parsing,
  recipe policy, bounded downloads, staged execution, publication and revalidation.
- Exact official Maven coordinates and installer SHA-1 pins. NeoForge uses Modrinth's
  **neo** metadata for Minecraft/loader mapping and intersects it with official
  Maven versions; profile Minecraft identity is checked again. Numeric stable loader
  releases only; prereleases are not silently substituted. Forge legacy layouts
  before Minecraft 1.17 remain unavailable.
- Server-side data/processors with bounded token substitution, validated Maven
  paths, approved tool classes/tasks/flags and scoped file arguments. Native archive
  extraction replaces installer script extraction execution. Server mappings are
  downloaded by Vesta instead of allowing a processor to fetch them.
- Owned Java subprocesses with selected Java checks, literal Java argument files,
  private temp/home, null stdin, hidden Windows console, bounded output and one
  deadline. Reuses `vesta-game-runtime::process::tree` for suspended Windows job
  attachment and Unix process-group ownership; no duplicated native job code.
- Per-agent-process processor admission of one job, a 512 MiB Java heap ceiling,
  staged filesystem budget checks, and space reservations for staging plus published
  copies. Working storage cap derives from input sizes (3x plus 128 MiB, capped by
  existing configured limits). This is not a global semaphore across independent agents.
- Schema-3 processed receipts distinguish upstream-pinned inputs from locally
  generated file hashes. Reuses bundle rehash/identity checks, original-pin validation,
  hardlink restrictions, coordinator/reservations and existing software jobs.
- Full layout/inventory validation, patched-JAR structure checks, original input
  revalidation after processing, receipt-last publication and managed artifact hashes
  for rollback. Existing schema-1/2 receipt and plan serialization stay intact.
- Platform-specific launch templates become absolute managed Java arguments while
  keeping game cwd separate. Current Forge shim metadata supplies its bootstrap
  classpath; the shim's Java check is retained as an installation step. This avoids
  its hardcoded cwd-relative library lookups without adding a Java wrapper.
- Loader version is retained in installed software status. Forge-family Java
  compatibility uses the matching Minecraft Java major instead of assuming every
  newer Java is compatible. The existing review, approval, stopped activation guard
  and previous-install rollback remain the management boundary.

Reviewed archive and tool manifests without executing Java: Forge 47.4.28/66.1.0,
NeoForge 21.4.111-beta/26.3.0.99. The beta archive informed parser research only;
production catalogue selection excludes it. Processor entry classes were checked
against official tool JAR manifests. This is structural evidence, not a passing
real installation test.

## Code review corrections

Review fixed bounded substitution cycles; original-pin rehashing rather than trusting
receipt-only observed hashes; classpath quoting/Windows command limits; current Forge
shim cwd assumptions; missing installation-parent creation; prerelease filtering;
mapping sizes in aggregate accounting; duplicate catalogue fetches; preserving loader
identity in durable software state; and retained installer-metadata memory limits
(32 MiB across pending service plans). Embedded archive contents contribute bounded
size/hash evidence to plan hashes rather than being serialized as JSON byte arrays.
Existing tests asserting that Forge is wholly unimplemented were updated for the new
provider contract/legacy refusal; they were not run.

## Verification and limits

Windows agent compile and strict Clippy were used for integration checking. Tests,
Linux checks, real installation/launch, low-end benchmarks, update/rollback fault
injection and cancellation/descendant exercises are deferred to the next test pass.

Native process-family ownership is **not** a hostile-code sandbox. Unix descendants
can deliberately leave their group, and processor Java still runs as the agent's OS
identity. Private staging, scoped arguments and post-validation do not enforce OS
filesystem/network isolation. Do not describe this as certified confinement or as
safe execution of arbitrary uploaded installers. Only the compiled upstream provider
recipes are accepted. Unknown tools/flags/profile shapes fail closed.

Failed or interrupted staging is retained for investigation; it is not blindly
replayed or recursively deleted. Successful publication removes only inventoried
staging files; empty directories/private temporary remnants can remain. Disk checks
bound a job, not total accumulated failed stages. A deliberate ownership-aware
retention/cleanup policy remains follow-up work.

The modern adapters still need fixture and real-install coverage across actual
loader versions. In particular, default heap/deadline/storage headroom, generated
output differences, Java argfile edge cases, and the Forge shim replacement need
the planned disposable Windows/Linux comparison runs. Pre-1.17/legacy server-JAR
adapters, beta opt-in and OS confinement are not implemented in this checkpoint.
No roadmap compatibility percentage or release certification is promoted.
