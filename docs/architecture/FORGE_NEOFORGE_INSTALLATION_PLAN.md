# Forge and NeoForge server installation

Research and proposed implementation, 2026-10-25. No installer or Minecraft was
executed. No production support flag changes are included. Luna scanned Vesta's
existing execution, workspace, transport and receipt foundations; root inspected
upstream sources and four downloaded official installer archives.

## Decision

Implement a server-only, versioned processor pipeline in `vesta-server-install`.
Use upstream installer profiles and artifacts as inputs, with Vesta-owned download,
execution, output verification and publication. Do not call the launcher's client
installer, invoke shell scripts, or run a whole online installer against the game
directory. Keep the existing software selector, review/apply jobs, stopped-state
activation and rollback flow.

The official headless baseline is `java -jar <installer.jar> --installServer`.
It downloads dependencies and runs processors; modern distributions then launch
through platform-specific argument files. This command is useful as a compatibility
oracle in disposable integration tests, rather than Vesta's production transport.
Running it online would bypass Vesta's network policy, download accounting and
artifact receipts. An offline whole-installer mode is an alternative only if the
entire selected installer version's inputs, effects and offline behavior are proven;
it is not a generic shortcut or automatic fallback.

## Evidence and compatibility families

Sources checked:

- [NeoForge server setup](https://docs.neoforged.net/user/docs/server/): headless
  install, generated scripts, JVM settings and update procedure.
- [Forge installer CLI](https://github.com/MinecraftForge/Installer/blob/2.0/src/main/java/net/minecraftforge/installer/SimpleInstaller.java)
  and [server action](https://github.com/MinecraftForge/Installer/blob/2.0/src/main/java/net/minecraftforge/installer/actions/ServerInstall.java).
- [Forge processor engine](https://github.com/MinecraftForge/Installer/blob/2.0/src/main/java/net/minecraftforge/installer/actions/PostProcessors.java)
  and [profile schema](https://github.com/MinecraftForge/Installer/blob/2.0/src/main/java/net/minecraftforge/installer/json/Install.java).
- [NeoForge server action](https://github.com/neoforged/LegacyInstaller/blob/main/src/main/java/net/minecraftforge/installer/actions/ServerInstall.java)
  and [processor engine](https://github.com/neoforged/LegacyInstaller/blob/main/src/main/java/net/minecraftforge/installer/actions/PostProcessors.java).
- [NeoForge InstallerTools](https://github.com/neoforged/InstallerTools): tasks are
  implementation details without a stable external API guarantee.
- [Forge promotions](https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json)
  and [NeoForge Maven catalogue](https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml).
- [Forge 26.1 launch script](https://github.com/MinecraftForge/MinecraftForge/blob/26.1/server_files/run.sh)
  adds a shim Java check before the argfile launch.
- [NeoForge 26.1 migration guidance](https://github.com/neoforged/.github/blob/main/primers/26.1/index.md)
  establishes the Java 25/deobfuscation change; [26.3 properties](https://github.com/neoforged/NeoForge/blob/26.3.x/gradle.properties)
  also select Java 25. The older user guide's open-ended Java 21 range is insufficient
  for current releases.

Archive inspection (all `install_profile.json` files report spec 1):

| Official artifact version | Minecraft | Server-applicable processors | Important behavior |
| --- | --- | ---: | --- |
| Forge `1.20.1-47.4.28` | 1.20.1 | 9 | Bundler extraction, mappings, split/remap and binary patching |
| NeoForge `21.4.111-beta` | 1.21.4 | 9 | Similar pipeline with NeoForged tools; beta example is research, not a default recommendation |
| Forge `26.3-66.1.0` | 26.3 | 4 | Extraction plus a newer binarypatcher flow; shim Java precheck |
| NeoForge `26.3.0.99` | 26.3 | 2 | Script extraction and `PROCESS_MINECRAFT_JAR` |

These are inspected shapes, not claims that every release in a family works.
Some older Forge steps declare output hashes; inspected NeoForge processor lists
have no `outputs` declarations. All inspected launch argument files contain relative
library paths. Both installer profiles and Minecraft's bundled server metadata
contribute dependencies; `version.json` is not a complete server recipe.

Research copies are outside Git at `V:/tmp/forge-research-20261025`. Observed SHA-256:

| Artifact | Observed SHA-256 |
| --- | --- |
| Forge 47.4.28 installer | `d615397a6b2f1e99d4b454e2db2b4ca2e94be91d5e0243c8c7e023d965db39e7` |
| NeoForge 21.4.111-beta installer | `e7f84a51b56b6b5c24be058ae59b4443db6b65c44a5543de915b4cacfc611680` |
| Forge 66.1.0 installer | `31b5d7272b97771b586f3bcad3b0ceab40b03117ef437f23ccc8afe11c462b4e` |
| NeoForge 26.3.0.99 installer | `7eeb74544422b757aa1611f6487a74bf9f4a8c7e04b2023c2080a6e637dfd218` |

These locally observed hashes identify the inspected bytes; they are not upstream
signatures or a replacement for published digest verification.

## Existing Vesta foundations and gaps

- `providers.rs`, `providers/releases.rs`: existing version/release and immutable
  plan seam. Add Forge providers behind the same API.
- `transfer.rs`, `model::SourceFamily`: bounded HTTPS transfers, approved hosts,
  digests, cancellation, retries and total budgets. Add explicit Forge/NeoForged,
  Minecraft metadata/mappings and library source families as needed. No arbitrary
  Maven repositories from profile URLs, mirrors or client requests.
- `execution.rs`: literal argv, selected Java rechecks, bounded streams, deadlines
  and direct-child ownership. Production currently only constructs Java probes.
- `execution_workspace.rs`: private declared inputs/outputs and path validation.
  It is not an OS sandbox, and currently expects published digests for declared
  files. Generated outputs without published hashes need a separate contract.
- `bundle.rs`, `managed.rs`: verified multi-file publication and separate game
  working directory. Current Fabric/Quilt launch construction must not be reused
  unchanged for Forge modular launch arguments.
- `vesta-control::software`: owner authorization, revision, approval, durable jobs,
  worker, stopped guard, activation and rollback already exist.
- Launcher client processor context is useful behavioral reference for server
  sidedness only. Archived checkpoints explicitly reject its permissive client
  runner as the production server executor.

## Implementation sequence

### 1. Catalogue and exact identities

Add private `providers/forge.rs` and `providers/forge/catalogue.rs`. Resolve exact
Maven coordinates from upstream catalogues; preserve the full Forge coordinate
instead of reconstructing historical suffixes. Forge promotions annotate latest
and recommended choices, not the complete version list. NeoForge Minecraft mapping
must be verified against profile metadata; do not infer it with a universal version
string formula (26.x changed the numbering scheme).

Use official Maven/profile metadata as the primary path. Earlier archived plans
suggested Modrinth sided metadata; that remains an optional, separately validated
adapter, not an assumed working endpoint or a reason to execute client recipes.
The current client-constructed NeoForge v0 manifest URL returned 404 during this
research. A mirror outage must not silently change a reviewed artifact identity.

Expose releases with `loader_version`, null numeric `build`, and exact IDs through
the existing `/software/versions`, `/software/releases` and plan APIs. Sort using
version components and prerelease semantics, never lexical order or Maven document
order. Recommended/latest labels need to distinguish prereleases; existing
`experimental:false` must remain enforced. Add explicit opt-in before beta-only
releases can be installed; do not label a beta as stable to bypass the contract.

### 2. Server-only plan and input acquisition

Boundedly inspect the installer ZIP. Validate archive entries, duplicate names,
compression expansion, JSON size/counts, profile spec, exact Minecraft/loader
identity and full Maven grammar including classifiers/extensions.

Normalize to immutable `ForgeInstallPlan` containing recipe version, input pins,
server processor steps, declared temporary/final outputs, launch specification,
Java compatibility, resource budgets and warnings. Hash all of this into the
existing approval. Planning can fetch/cache bounded installer metadata but executes
no installer/processors and changes no game files.

Choose server data explicitly; absent processor sides means both, empty means
neither. Reject malformed/unknown sides in new server recipe validation. Resolve
only known tokens, including ROOT, INSTALLER, LIBRARY_DIR, MINECRAFT_JAR and sided
data. Reject cycles, unresolved tokens, escaping paths and arbitrary flags/classes.
Recipe adapters approve tool coordinates, entry classes, tasks and argument shapes;
upstream JSON alone cannot authorize a new executable policy.

Prefetch all dependencies through Vesta. Read bundled server libraries from the
verified Minecraft server JAR. Replace mapping-download tasks with a Vesta download
of the exact server mapping pin followed by its expected output binding. Preserve
published SHA-1 evidence where required, while storing observed SHA-256 separately.
Never permit TLS bypass or treat an observed hash as publisher authentication.

### 3. Bounded processor execution

Add a production invocation constructor accessible only to validated Forge plans.
Run processors serially in an owned staging workspace, with no game/world/config
tree writes. Prefer native Rust archive extraction for well-defined extraction
steps; keep patching/remapping in pinned upstream Java tools.

Use the selected Java, headless operation, private temp/home, sanitized environment,
null stdin, bounded logs and one absolute job deadline. Run one installation at a
time per server and conservatively cap host processor concurrency. Protect urgent
agent control capacity; do not place processor work on management request workers.

Before release, close the direct-child versus process-family cleanup gap for
installer execution on Windows/Linux. Reuse or extract the existing tested native
process ownership primitives instead of inventing PID-name cleanup. Staging and
post-validation do not provide filesystem/network isolation; use an OS-enforced
restricted execution policy for processors before claiming those guarantees.
An unsupported isolation profile must be reported honestly, not silently weakened.
This does not require a Docker/container UI or a redesign of server hosting.

Extend output contracts to bounded expected paths/types/counts, with optional
published digest. Validate all available expected hashes; validate JAR/ZIP structure
and required entries for generated outputs lacking one, then record observed hashes
and exact input/recipe provenance. Never fabricate an expected digest to satisfy
the current workspace type. Inventory the full workspace before publication.

Cancellation/timeout requires proven process-family termination before cleanup or
retry. Unknown outcomes retain owned staging and produce recovery-required status;
restart must not blindly replay a processor against partially generated files.

### 4. Publication, launch and rollback

Add a schema-versioned processed-install receipt containing input evidence,
generated output evidence, platform/architecture, recipe version, Java identity and
canonical launch files. Keep existing receipt formats readable. Stage, verify and
atomically publish a new immutable installation before acquiring the brief stopped
activation guard. Downloads/processors do not hold the game start lock for minutes.
Recheck revision, stopped state and all pins immediately before activation.

Generate Vesta-owned Java argv/argfiles from validated launch data. No shell or batch
execution. Preserve module-path, classpath, opens/exports and loader properties;
rewrite every path-bearing property/argument to the immutable installation root,
not just `-cp`. Keep game cwd at the actual server game directory. Preserve the
selected heap/JVM settings without executing installer-provided user flags.

For newer Forge, reproduce the verified shim Java compatibility check or implement
its exact equivalent before launch; do not simply omit it. Record all launch inputs
in revalidation, including argfiles. Test spaces, Unicode, Windows separators and
Java argfile quoting on the actual Java versions involved.

Installer Java and game Java may have different compatibility requirements.
Read Minecraft's Java metadata plus versioned loader constraints; do not assume
newest Java works with every historical loader. Test supported majors explicitly.

Updates build another installation, then switch the launch pointer. Preserve worlds,
mods, config, EULA and previous software. Never auto-start Minecraft or auto-accept
the EULA. Rollback changes software only; retain the existing world downgrade warning.

### 5. UI and support promotion

Keep the current software editor and running read-only view. Enable Forge/NeoForge
choices only for certified provider/platform/Java combinations; show the exact loader
version in review. Expose bounded progress steps (download, prepare, patch, verify,
activate) through existing jobs. Add clear fixed errors for unsupported profile,
Java incompatibility, insufficient storage, processor timeout and corrupt output.
Avoid translating all of these into a generic invalid-input message.

No launcher-local installation, new hosting dashboard or modpack implementation is
needed for this scope. Future modpacks can request these exact loader identities.

## Delivery and verification gates

1. Capture small, source-attributed profile/argfile fixtures; catalogue/parser tests
   and resource accounting first. Do not commit upstream executable JARs.
2. Certify representative modern Forge 1.20.1/NeoForge 1.21.1 or 1.21.4 pipelines
   with disposable official-installer differential tests.
3. Certify the separate 26.x pipelines with Java 25 before advertising current
   Minecraft support. Do not infer coverage from older profiles.
4. Add legacy Forge 1.12.2 and 1.16.5 adapters, then investigate 1.7.10 separately.
   This is staged coverage, not a change to the requirements' Minecraft 1.7+ target.
   Older server-JAR launch layouts must not be forced through modern argfile logic.
5. Windows x64 and Ubuntu 24.04 x64 end-to-end install/launch/stop/update/rollback;
   Linux ARM64 validation before advertising that combination. WSL is useful for
   Linux filesystem/process behavior, but not proof for systemd/resource isolation
   or all ARM64/native-library combinations.

Tests must cover ZIP traversal/bombs/duplicates, malicious Maven/token/URL inputs,
wrong-side processors, unknown tools, partial/hash-mismatched downloads, unsupported
profiles, missing generated files, absent upstream output hashes, failed patching,
disk exhaustion, cancellation/log flooding, descendants and interrupted recovery.
Add real Java argfile/module launch tests with separate install/game roots and
low-memory/one-core measurements. Proposed starting processor heap ceiling is
512 MiB, one active job on a 1-2 core/2-4 GiB host; benchmark before committing it as
a product default. Some versions may need more and must fail clearly without
destabilizing agent control. Keep storage/cache bounded; no background installer JVM.

Differential tests should compare required files, launch behavior and upstream
expected hashes, not demand byte-identical ZIPs when timestamps differ. A real
Minecraft boot/EULA step belongs in an explicitly opted-in disposable test harness.

## Second-pass review

The plan was rechecked against the inspected archives and existing code boundaries.
Specific traps addressed: 26.x recipe changes, missing NeoForge output hashes,
client/server metadata separation, relative launch paths, beta policy, Forge shim
checks, legacy launch layouts, process-family ownership and direct input-versus-
observed-output provenance. The first implementation milestone is parser/catalogue
and fixture evidence; production execution and UI support flags stay gated until
the execution/publication tests pass. No support percentage or roadmap completion
status is raised by this research document.
