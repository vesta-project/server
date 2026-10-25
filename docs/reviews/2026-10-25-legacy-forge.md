# Pre-1.17 Forge implementation checkpoint

2026-10-25. Extends the shared Forge installation pipeline; real installation,
launch tests and automated test execution remain deferred at the owner's request.

Implemented two legacy profile paths:

- Original `install`/`versionInfo` profiles, including the inspected Forge
  `1.7.10-10.13.4.1614-1.7.10` archive. Extract its universal JAR from the pinned
  installer, select server-required libraries, and pin downloads using profile
  checksums or official repository SHA-1 sidecars and sizes.
- Spec-1 profiles with no modern launch argument files, including inspected Forge
  `1.12.2-14.23.5.2864` (no processors) and `1.16.5-36.2.42` (processors). Empty
  download URLs refer to installer-embedded JARs and no longer become invented
  Maven download URLs. Embedded bytes are verified separately against their
  profile digest, or derived from the verified installer for original profiles.

Both adapters reuse the existing bounded transport, recipe/task validation,
staging, native process ownership, publication, receipts and rollback path.
Added the legacy SpecialSource processor and its fixed entry class/path flags.
Java-8 processor invocations use bounded literal argv, without `@argfile` or a
fallback to a different Java. Modern processor invocation remains unchanged.

The Forge catalogue now includes stable releases from the Minecraft 1.7 baseline.
Exact historical Maven coordinates, including trailing Minecraft-version suffixes,
are preserved. Earlier versions and unsupported schemas still fail closed.

## Launch layout

Legacy Forge requires Java 8 under the conservative matching-major policy. The
existing Java download/selection flow already supports that major. Preserve owner
JVM settings; incompatible custom JVM flags may still need owner adjustment.

Read the bounded main section of the verified universal JAR manifest. Accept only
the known server entry classes: `cpw.mods.fml.relauncher.ServerLaunchWrapper`,
`net.minecraftforge.fml.relauncher.ServerLaunchWrapper`, or
`net.minecraftforge.server.ServerMain`. Preserve its server classpath; the 1.16
entry class reads its own `ServerLaunchArgs` from the original Forge JAR.

Generate a deterministic manifest-only pathing JAR with absolute, URI-encoded,
properly folded classpath entries. This is not a new Java wrapper or compiled Java
component: the JVM invokes Forge's own server entry class. It avoids both Java 8's
lack of argument files and Windows command-line limits. Keep game cwd separate from
the immutable installation. Provide the expected `minecraft_server.<version>.jar`
alias as a verified copy, not an unchecked game-directory mutation or a hardlink.

Retain the generated launch JAR, embedded library evidence and vanilla alias in
normal file receipts. Rebuild/compare generated launch bytes during verification;
rehash embedded inputs and aliases against the approved installer/vanilla evidence.
The existing schema-3 modern plan serialization omits the absent legacy field.

## Reviewed sources and verification

- [Forge 1.7.10 server wrapper](https://github.com/MinecraftForge/FML/blob/1.7.10/src/main/java/cpw/mods/fml/relauncher/ServerLaunchWrapper.java).
- [Forge 1.12 server wrapper](https://github.com/MinecraftForge/MinecraftForge/blob/1.12.x/src/main/java/net/minecraftforge/fml/relauncher/ServerLaunchWrapper.java).
- [Forge 1.16 server main](https://github.com/MinecraftForge/MinecraftForge/blob/1.16.x/src/fmllauncher/java/net/minecraftforge/server/ServerMain.java)
  and [library lookup](https://github.com/MinecraftForge/MinecraftForge/blob/1.16.x/src/fmllauncher/java/net/minecraftforge/fml/loading/LibraryFinder.java).
- Official Maven installer ZIPs were inspected without executing Java. Research
  files live outside Git under `V:/tmp/forge-legacy-20261025`. Small official library
  checksum endpoints were read to confirm the older metadata mechanism.

Windows agent compilation and strict Clippy provide integration checks only.
The old unsupported-provider test was narrowed to versions before Minecraft 1.7;
it was not run. No real installer, processor, Minecraft server, EULA operation,
agent restart, live software switch or test suite was executed.

Required next validation: no-op and processor fixture coverage, exact embedded input
tampering, Java-8 command construction, manifest folding/Unicode/long paths,
Windows/Linux real installs for the inspected versions, and launch/update/rollback.
Existing limits on OS sandboxing, failed-stage retention, prereleases and untested
upstream profile variants still apply. This checkpoint does not certify every
Forge release or change the requirements' platform/version acceptance status.
