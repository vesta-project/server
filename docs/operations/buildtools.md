# CraftBukkit and Spigot source installations

The software identifiers are `bukkit` (CraftBukkit) and `spigot`. Both use one
BuildTools adapter and the existing software review, publication, activation and
verified rollback flow. The launcher sends version/build identifiers, never a JAR
URL. No unofficial compiled JAR mirror is used.

## Host requirements

- A JDK, including `javac`, selected through the normal Java configuration.
- Git on the agent's PATH. Windows also needs Git Bash from Git for Windows.
- A local supported installation volume with room for the source workspace,
  final JAR, and the existing free-space floor. Default reservation is 5 GiB plus
  the 512 MiB floor. This is scratch space, not a world backup.
- Minecraft stopped. Management holds its start gate throughout compilation,
  including the supervisor's stopped hold, rather than only during activation.

The agent checks the version metadata's inclusive Java classfile range, translated
to Java major versions. A JRE without its sibling compiler is insufficient.
Versions without an explicit Java range, target output hash or the reviewed
bootstrap layout are not offered as build choices. Versions requiring a newer
unaudited BuildTools release are also refused.
The current source-build adapter refuses configured transport proxies rather than
silently bypassing the operator's selected proxy. Other providers retain their
existing proxy support.

## Reviewed inputs and output

BuildTools 201 is pinned to the immutable official Jenkins asset:

`https://hub.spigotmc.org/jenkins/job/BuildTools/201/artifact/target/BuildTools.jar`

SHA-256: `81ed06f5ce5fb705b558bf9b3cc2c5c53cdd9742406c5338c452faa13a1e1c24`.
Its manifest identifies `git-BuildTools-5fc1010-201`. The reviewed upstream source
commit is `5fc101051bb4bc32a40c8fb7d585db5ed5e97884` in the official BuildTools Git
repository. Updating this pin requires reviewing the runner's behavior again.

A fresh official version document supplies the exact BuildData, Bukkit,
CraftBukkit and Spigot commit IDs, release build number, Java range and target
output hash. Each repository is fetched from its fixed official URL. Its local
`master` is pinned to that commit before BuildTools runs. The runner uses
`--dont-update`, without `--rev`, and binds `bt.name` to the reviewed build number.
This avoids a second mutable metadata lookup inside BuildTools. Local refs are
verified again after compilation. BuildData's Minecraft version must match the
reviewed selection.

BuildTools produces a local build. Vesta requires the generated output to match
the selected version document's target SHA-256. The verified build input and the
generated output have separate receipt fields. A mismatch never activates the
new software.

Cross-host byte consistency needs explicit newline handling. Repositories retain
LF checkout bytes and normalize generated source commits. The private JVM options
also set LF line separators for every compiler/Maven subprocess. Without that,
Windows Maven writes CRLF into embedded `pom.properties`, changing the final JAR
hash even when its class files match. No generated artifact is rewritten after
hashing to hide a mismatch.

Vesta validates the generated bootstrap identity, target/version JAR name, and
embedded runtime/library checksum lists. The known shaded `minecraft-server`
list entry is skipped exactly as the upstream bootstrap does; it is not a
separately embedded runtime file. The complete generated JAR is hashed locally.
A schema-4 receipt records the verified BuildTools input separately from this
local output, retaining the four source commits, source-document URL/hash and
selected Java major as well as the approved plan hash. Publication is receipt-last. Managed launch and rollback rehash the
retained local output. Schema-1/2/3 approval hashes remain unchanged.

The previous software and game directory remain intact until normal activation.
The installer neither starts Minecraft nor accepts its EULA.

## Resource and failure behavior

Source builds and Forge processor jobs share one CPU-heavy installation slot per
agent process. BuildTools receives a 512 MiB heap limit. Compiler/decompiler
subprocesses receive 512 MiB for older releases or 1 GiB for releases requiring
Java 25 or newer, with one active processor per JVM. These are per-JVM settings,
not a whole-host memory quota. A preflight check requires available memory for
the child heap, parent heap and another 256 MiB, respecting Linux cgroup memory
limits. Parent and compiler processes overlap. Building source takes considerably
more time and memory than downloading a Paper or Vanilla JAR; a small host may
manage Minecraft comfortably but lack the headroom to compile it.

The source job has an explicit one-hour deadline separate from the existing
ten-minute transfer deadline. Its tree is bounded to 4 GiB by periodic accounting,
200,000 entries and 64 directory levels. This is a monitored bound, not an OS disk
quota. Logs are limited to 8 MiB per stream per invocation. Ordinary Forge
processors retain their 1 MiB limit. Maven runs in batch mode without transfer
progress output. Java downloads have finite connect/read timeouts; Git stops a
transfer sustained below 1 KiB/s for 30 seconds. Decompiler out-of-memory and
incomplete-class diagnostics fail the build even if upstream exits successfully.

The process-family runner owns Git helpers, BuildTools and compiler descendants.
Cancellation, deadlines, excessive output, storage failures and nonzero exit stop
that owned family. The installer never looks up and kills arbitrary PIDs. Windows
installer jobs retain kill-on-owner-exit behavior.

Git and Maven use a private home/configuration for each build. Ambient Java
injection options, credential helpers, proxy environment, Git hooks and Maven
settings are not inherited. No operator Git/Maven configuration is changed.
BuildTools still executes upstream build logic and downloads dependencies. This
is lifecycle/resource management, not a security sandbox or a hermetic build.

Successful source workspaces are removed using a bounded inventory, refusing
external links and hard links. Spigot patch repositories are initialized before the reviewed script fetches from the local source repositories, avoiding its default hard-linked clone. The one known internal `decompile-latest` alias is
never followed. Failed workspaces remain under `.staging/buildtools_<id>` for
explicit inspection/recovery. No failed source job changes the active launch.

## Opt-in installation smoke test

Use a fresh existing directory outside all live game and agent roots:

```text
cargo run -p vesta-server-install --example buildtools_install -- --build bukkit 1.21.8 <absolute-java-path> <test-install-base>
cargo run -p vesta-server-install --example buildtools_install -- --build spigot 1.21.8 <absolute-java-path> <test-install-base>
```

The example builds, installs and re-verifies the selected software. It does not
launch the installed server or accept the EULA. Test logs and platform limitations
are recorded separately in the implementation review.

Upstream references: [BuildTools documentation](https://www.spigotmc.org/wiki/buildtools/),
[official version metadata](https://hub.spigotmc.org/versions/1.21.8.json),
[official source repository](https://hub.spigotmc.org/stash/projects/SPIGOT/repos/buildtools/browse).
