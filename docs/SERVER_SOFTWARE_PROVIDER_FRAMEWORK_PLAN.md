# Server software providers, automatic installation and open protocol — plan

Status (2026-10-06): **partially implemented foundation** — step 2 (typed launch definition, [checkpoint](LAUNCH_DEFINITION_CHECKPOINT.md)), the discovery/negotiation portion of step 1 ([protocol checkpoint](SERVER_FEATURE_PROTOCOL_CHECKPOINT.md)), and step 3's callable local Rust JAR-provider backend ([automatic-provider checkpoint](AUTOMATIC_JAR_PROVIDER_CHECKPOINT.md)). All eight initial IDs have compiled descriptors; Vanilla/Paper/Folia/Purpur resolve exact artifacts and install new local bundles through a bounded verified transport, reviewed plans and receipts. Step 4 now has explicit Client/Server processor data selection ([context checkpoint](LOADER_PROCESSOR_CONTEXT_CHECKPOINT.md)) and private bounded direct-child/declared-file workspace groundwork ([execution checkpoint](BOUNDED_SERVER_EXECUTION_CHECKPOINT.md)). Production execution remains the selected-Java version probe; prepared-job execution is fixture-only, without plan/receipt promotion or native confinement certification. The existing client executor remains client-only and is not a safe server installer. Loader installers, managed Java, launcher/headless wiring and setup wizard remain unimplemented. Draft-v1 discovery is not a complete management wire API; software descriptors never grant authority or certify compatibility. This planning document is not release evidence; the requirement ledger changes only through dated checkpoints.

## Goal

Users pick software and a version, review requirements/EULA, and Vesta installs it. Finding an executable is not the normal path; custom executables and launch settings stay available as an advanced option. Separately, the server-capability protocol is a language-neutral contract that other software can implement without Vesta's Java wrapper.

Two boundaries, kept apart:

1. **Install providers** — turn a software/version choice into verified files plus a typed launch definition.
2. **Open capability protocol** — describes what a server implementation supports. A supported feature never grants a user permission; every action needs implementation support **and** current canonical authorization. Microsoft/Minecraft identity remains the expected common online-mode flow; pack passwords, OP, plugins and connector possession never imply administration.

## Initial automatic-install scope

| Software | Source / shape |
|---|---|
| Vanilla | Modrinth launcher metadata `downloads.server` |
| Paper, Folia | PaperMC Fill v3 builds; identifying User-Agent; published size/checksum |
| Purpur | Official Purpur v2 catalogue/build API |
| Fabric, Quilt | Official server installation mechanism (the sampled Modrinth profiles are client launch profiles) |
| Forge, NeoForge | Modrinth sided data/processors via a server `LoaderProcessorContext` |

Later: FerrumC and other native servers (experimental, individually certified), proxies, Spigot/CraftBukkit through an explicit BuildTools workflow. Deprecated software only on demand. Planned entries are shown as unavailable with a reason, never as supported.

## Design rules

- Typed descriptor and install plan; recipes are compiled in. Downloaded metadata never supplies executable policy, scripts or classes.
- Launch contract is literal executable + argv + working directory, with JAR, classpath/argfile and native variants. No shell interpolation, no universal `server.jar`.
- Reuse the existing generation-bound lifecycle: one `game.generation` lock, the three existing operation types, opaque generation IDs. No per-software operations or locks.
- Unsupported graceful stop is rejected before any effect. No automatic force-kill. A live process or open port is not "ready".
- The current launch implementation accepts legacy `minecraft.launch-jar` through an explicit adapter and rejects conflicting definitions. Legacy code may be replaced directly during development; the user does not require compatibility migrations.
- Server installs use their own root and receipts. They never touch client profiles or overwrite an existing server.
- Approval binds to the exact plan snapshot, rechecked before commit. EULA acceptance is a separate affirmative choice.
- The existing launcher downloader is not safe enough as-is (unbounded streaming, locked files accepted as valid). Server installs need a verified path with streamed-byte caps, deadlines, redirect limits, source allowlists and exact digests.
- Installers and processors are code execution: reviewed consent, owned child processes, bounded output and time, no blind replay after an unknown outcome.

## Where this fits among other work

**Can proceed now (independent of the gates below):** protocol schemas, descriptors and launch contract; JAR providers; loader providers; the setup wizard for local installation.

**Must not be implied by this work:** authenticated start/stop/remote management, public hosting, or "secure" anything. Remote install/update later needs current exact `content.install` through the canonical operation path.

**Still-open gates (unchanged by this plan):** independent supervisor and protected IPC, OS-local owner proof/bootstrap, Microsoft UUID proof adapter, device enrollment/mTLS/sessions, separate game identity and filesystem confinement, removal of legacy trust routes, signed distribution, real platform certification.

## Suggested order

1. Protocol contract and descriptors: schemas, golden fixtures, one independently written peer, feature-versus-permission tests.
2. Generic launch contract on `MinecraftServerProcess`, tested with real owned fake-game processes in each command shape.
3. Verified transfer path plus Vanilla/Paper/Folia/Purpur providers, tested against local fixture servers.
4. Loader providers: `LoaderProcessorContext`, Fabric/Quilt recipes, fixture-driven installer processes; client loader regression tests rerun.
5. Setup wizard (UI only) over real typed commands and task events; labels distinguish installed, alive, ready, authorized and management unavailable.
6. Fresh full verification, narrow dated ledger evidence, unsigned commits, no push.

Backend and all new tests are authored by the parent session; Opus is used for UI only.

## Development-time safety

Tests use fixtures only. No real provider artifact download, build or installer run, no Minecraft or FerrumC launch, no EULA acceptance, no demo erasure, no service, firewall or router change, no public exposure, no signing-config change, no push. The reset/launch-only scripts remain a separate deferred task.

## Open decisions for the user

- Spec/SDK licensing for the open protocol (both repositories are GPL-3.0 today); publishing as a separate repository.
- Whether to ship the JAR providers first as an intermediate release before loader providers are done.
