# CraftBukkit and Spigot installation validation

**Policy follow-up:** the earlier refusal results below are historical. Following
upstream research and the user's decision, a reference difference for a locally
compiled output now succeeds with a durable warning. Downloaded inputs retain
strict checksum verification. See [warning-result validation](2026-10-27-source-build-warnings.md).

## Implemented path

Both providers share the official BuildTools runner, source discovery, tool/JDK
binding, process ownership, resource limits, output validation and receipt
publication. Forge reuses the extracted process-family runner and heavy-job
permit. The launcher passes fixed software/version/build identifiers; the agent
resolves fresh official authority before approving an installation.

Source builds hold the existing stopped gate through compilation and activation.
They do not change worlds, settings, mods, plugins or EULA acceptance. Generated
server bytes are separate from the pinned BuildTools input in schema-4 receipts.
Launch and rollback rehash the retained output.

See [operator requirements and limits](../operations/buildtools.md).

## Real installation matrix

Tests used fresh isolated installation roots, never live agent/game directories.
Each successful test compiled, published and re-verified the installed receipt
and JAR. Minecraft was not started.

| Host | Target | Minecraft | JDK | Result |
| --- | --- | --- | --- | --- |
| Windows | CraftBukkit | 1.21.8 | Temurin 21.0.8+9 | Matches official SHA-256 |
| Windows | Spigot | 1.21.8 | Temurin 21.0.8+9 | Matches official SHA-256 |
| Ubuntu 24.04 / WSL | CraftBukkit | 1.21.8 | Temurin 21.0.13+9 | Matches official SHA-256 |
| Ubuntu 24.04 / WSL | Spigot | 1.21.8 | Temurin 21.0.13+9 | Matches official SHA-256 |
| Windows and Ubuntu | Spigot | 26.3 | Temurin 25.0.3+9 | Refused: output differs from official SHA-256 |
| Ubuntu 24.04 / WSL | Spigot | 26.3 | Temurin 26.0.2+10 | Same output hash as Java 25; refused |
| Ubuntu 24.04 / WSL | CraftBukkit | 26.3 | Temurin 25.0.3+9 | Refused: output differs from official SHA-256 |

The 1.21.8 source build is 4534. CraftBukkit output is 84,207,987 bytes,
SHA-256 `482e909b28925d34ce28e6cf0e8aea2301985e59906f31e3fd266cc736bbdf60`.
Spigot output is 84,523,129 bytes,
SHA-256 `6ab69fac1efc2e30beef4803fb7b14bb749a73322a7ff7d0f4e34de0ce4964e2`.

Windows roots and JDK paths included spaces. Ubuntu tools were isolated under
the test user's cache; no system Java/Rust installation or shell profile changed.

Logs: `V:/tmp/buildtools canonical-20261027/{bukkit,spigot}.log` and
`V:/tmp/buildtools-linux-installations-20261027.log`.

### 26.3 upstream reproduction

The stock, unmodified pinned BuildTools 201 was also run on Ubuntu with
`java -Xmx512m -jar BuildTools.jar --nogui --rev 26.3 --compile spigot`, a private
Git/Maven home, a 1 GiB child heap, two visible processors and a 20-minute owned
timeout. It fetched the official version document and repos itself, without
Vesta's `--dont-update` or pre-created repositories. It exited successfully and
produced exactly Vesta's Spigot hash:

`c5a3ad776e222219b7a0aad44173a591fb5757b01b57f4f0aa2354ff08c2df56`.

The official version document still advertises
`d64fb48abc0a02c09273487248b641005b363c5070860d97bc13a35c8808a4e4`.
CraftBukkit's isolated output was
`fe16d328e258f80765f5b204fab42cf7ccbeef09166ad62d7e66cd880b3dca62`, against
the advertised `5a0d737a972defb8a0c0c263b727eebe799ee28e52dff9f47c398d50d597fb05`.

This reproduces the discrepancy outside Vesta under the tested conditions. It
does not establish which upstream build input differed at publication time.
Do not bypass the check or claim verified 26.3 support from successful compiler
exit alone. The original failed stages and stock build remain available for
inspection. Stock result: `V:/tmp/buildtools-linux-stock26-run2-20261027.log`;
detailed logs reside under the isolated WSL directory
`/home/eatham/.cache/vesta-buildtools-runs-20261027/stock-spigot-26.3-run2`.

## Problems found by real builds

- Nested debug-mode installer futures overflowed the Windows stack. Boxing the
  execution boundary fixes this without increasing every worker's stack.
- The original generic processor log cap was too small for normal source builds.
  BuildTools now has its own bounded 8 MiB per-stream allowance; Forge keeps its
  existing limit.
- BuildTools' known internal decompiler alias and transient file renames needed
  explicit handling in the monitored inventory. External links remain refused.
- Maven child processes needed explicit private HOME/TMP JVM properties. Parent
  properties alone did not isolate the operator's Maven cache.
- Spigot's local patch fetches need file protocol access. Empty owned patch repos
  preserve those fetches while avoiding upstream local-clone hard links.
- Windows generated source commits need Git input normalization. Maven also needs
  canonical LF line separators: two embedded pom.properties files otherwise
  changed the final hash despite identical classes. Both host outputs now match
  the official 1.21.8 hashes without modifying completed JARs.
- A 26.3 decompiler ran out of memory but upstream still reported success. Source
  builds now reject incomplete-decompiler diagnostics, use a larger reviewed
  child budget for Java 25+ releases and check available memory before starting.
  A subsequent complete Java 25 build still differed from the reference, so it
  remains refused rather than weakening the output check.
- Java 26 produced exactly the same Spigot 26.3 bytes as Java 25. Exact source
  refs, full Git history, release name, manifest versions and commit-derived
  timestamps were checked. The compiler-major hypothesis did not explain the
  difference. A dedicated `build_output_mismatch` reason now distinguishes this
  case from malformed downloads and confirms the current software is unchanged.
- The workspace regression run exposed Windows supervisor owner-crash behavior
  killing Minecraft. This was fixed and checkpointed separately in `23d7116`;
  explicit game stop/kill and installer process ownership remain enforced.

## Automated validation

- Windows installer library: 205 passed, zero failures, two opt-in diagnostics
  ignored. Ubuntu installer library: 204 passed, zero failures, one opt-in
  diagnostic ignored. The Windows-only volume test and a completed real bundle
  diagnostic were also run explicitly and passed earlier.
- Current full workspace/all-target compilation and strict Clippy for installer,
  control, game runtime and supervisor pass.
- Launcher typechecking and 33 hosting, reset-UI and localization checks pass.
- Client adverse-link/control scheduling regressions: 13 passed, zero failures.
  These cover delayed/throttled/stalled reads, coalescing, reserved control
  capacity, remote pacing and uncertain mutation replies without replay. This
  is application-level TLS response shaping, not packet-level 3G emulation.
- Before the last source provenance/headroom additions, the full server workspace
  all-target test run passed 2,016 tests, with zero failures and four opt-in tests
  ignored. Those additions received the scoped installer suites above.

Latest logs use `V:/tmp/buildtools-resume-*-20261027.log`. The earlier full run is
`V:/tmp/buildtools-workspace-tests-final-20261026.log`. After adding the dedicated
checksum-failure reason, Windows installer tests, strict installer/control
Clippy, full workspace compilation, launcher typechecking and all 33 launcher
checks were repeated successfully (`V:/tmp/buildtools-error-*-20261027.log`).

## Measurement limits

Per-JVM heap/CPU budgets, available-memory refusal, disk/entry bounds, deadlines,
pipe limits, cancellation and process ownership have tests. These are not a
whole-host CPU/memory sandbox. No representative full agent + Minecraft run on a
physical 1-core/2-GiB machine has been certified. Source compilation needs more
headroom than ordinary downloaded-server installation or background management.
Failed diagnostic workspaces remain available for explicit recovery; no broad
cache or source-directory deletion was performed.
