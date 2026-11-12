# Automatic JAR-provider backend checkpoint

Date: **2026-10-05**. Callable local installation backend; not a finished hosting wizard, management API, installer distribution or release certification.

## Implemented scope

`launcher-server-mvp:crates/piston-lib/src/game/server_install/` is a Tauri-free Rust module. The parent authored production code and every new test; agents provided read-only design/review. Both repositories were refreshed before implementation. The current-main launcher UI, manual Vesta import and the user's separate Tauri-manifest edit are preserved.

- Compiled descriptors for all eight initial targets. Vanilla, Paper, Folia and Purpur have automatic **backend** providers. Fabric, Quilt, Forge and NeoForge explicitly return `provider_not_implemented`; they remain the next provider goal, not dropped scope.
- `versions` performs bounded metadata discovery; `plan` resolves a specific version/build and probes caller-selected existing Java. Neither writes installation files. Planning intentionally uses network and executes that trusted local Java's literal `-version`, never a downloaded JAR.
- `Approval::review` binds the complete immutable snapshot hash, expiry, roots/exclusions, runtime evidence, limits, metadata revisions and immutable transport context. Legacy checksum and review warnings require acknowledgment. Approval is consumed by value; it is **local workflow consent, not identity proof or administrative authority**. Another review can issue another consent object, but a completed/colliding destination cannot be reused.
- `install` writes new server-only bundles; no adoption, update, client-profile mutation, EULA acceptance, game launch, service, firewall/router change or management listener. Dry-run validates an existing reviewed plan without network, probe or writes. Catalogue/planning APIs do not pretend their network requests are dry runs.
- `verify` strictly bounds/parses the receipt and rehashes actual installed bytes against both observed SHA-256 and the plan's published digest, including SHA-1/MD5. Self-consistent replacement of a mutable receipt and artifact is not sufficient. This is not a signed attestation.

## Sources and selection

Recipes, sources, licences/EULA review links and fixed launch intent are compiled in. Unknown upstream fields never supply executable policy. DTO collection visitors cap entries during decoding; malformed UTF-8, relevant duplicate fields/IDs, identity ambiguity and oversized metadata reject.

| Provider | Pin and Java evidence | Selection boundaries |
|---|---|---|
| Vanilla | Modrinth `/minecraft/v0/manifest.json`; catalogue **mirror `sha1`**, not `originalSha1`; exact detail `downloads.server`, SHA-1/size and `javaVersion.majorVersion` | Client JAR/mappings ignored. Legacy spaced catalogue labels are bounded/duplicate-checked but not selectable and cannot poison modern releases. Missing runtime evidence blocks. Snapshots require opt-in. |
| Paper/Folia | Fill v3 grouped version strings, version Java minimum, build `downloads['server:default']`, `checksums.sha256`, size and exact object path | Numeric version/build selection; stable preferred even with experimental opt-in. Exact experimental builds require opt-in; upstream support/channel warnings are retained. Recommended JVM flags ignored. Folia carries region-threading warning. |
| Purpur | Official v2 catalogue/version/exact successful-build metadata; compiled `/version/build/download`, **MD5 only**, no fabricated published size | Default searches at most five descending candidates; an exact failed build never silently substitutes. Java guide policy only: 1.20–1.21.11 → 21; 26.1–26.3 → 25. Older/unknown/future ranges block. |

Purpur's [installation guide](https://purpurmc.org/docs/purpur/installation/) explicitly delegates to [Paper's getting-started guide](https://docs.papermc.io/paper/getting-started/). The compiled matrix is reviewed guidance at this cutoff, not an exact historical bytecode minimum or certification. SHA-1/MD5 are legacy consistency evidence, not authenticity; computed SHA-256 is separately observed evidence, never relabelled an upstream checksum. First-run bundler/patcher downloads, extraction, plugin compatibility and real game behavior are not verified here.

## Transport and budgets

A dedicated reqwest client bypasses the permissive client downloader/cache shortcuts. HTTPS, exact compiled origins/path families, no userinfo/query/fragment/arbitrary port/IP host/encoded path; manual redirect validation **before** requests. Identifying/contact User-Agent, identity encoding, no ambient cookies/auth, no automatic proxy inheritance. Explicit trusted configured proxy is retained and warned about; invalid configuration does not silently fall back.

Direct DNS resolution is bounded and all returned addresses must pass the special/private-address policy; validated addresses are pinned into the per-hop client. Proxy mode delegates DNS and remains a disclosed trust boundary. No DNS-rebinding/platform certification claim is made from fixture tests.

Defaults: 16 MiB metadata; 1 GiB artifact; 4 GiB cumulative consumed bytes including failed attempts; two process-local install slots; three attempts; three redirects; 15-second connect; 30-second idle; ten-minute total, including retries/pauses. Limits may tighten, not exceed ceilings or lower the 512-MiB reserve. Hashless artifacts and artifact-to-memory calls reject. Published size/digest and real stream caps are independently checked; encoding/range responses reject. Errors discard raw inputs, response bodies and underlying secret-bearing causes. Cancellation is polled during stalled response waits, streaming, retry waits, pauses and publish boundaries.

Capacity resolution uses the most-specific unique sysinfo mount and a compiled local-filesystem allowlist (NTFS on Windows; reviewed hardlink-capable local families on Unix). The 2026-10-06 review follow-up adds a bounded unfiltered Linux `/proc/self/mountinfo` check (1 MiB, 10,000 entries) before sysinfo capacity discovery: the actual most-specific mount must be known local and match the capacity record. Missing/invalid/ambiguous mount information or a filtered-out mount refuses rather than borrowing capacity from a visible parent. These are compiled and fixture-tested helpers on Windows, not a claim of native Linux certification. Unknown/network/FAT/exFAT filesystems fail closed. Windows additionally requires read-only native fixed/removable drive classification. Process-local per-volume reservations cover the staged artifact plus the reserve, with recheck before publish. Interprocess quotas, mount aliases/hidden bind/subst arrangements, native confinement and platform certification remain open.

## Runtime, publication and receipts

Java probing owns a child with null stdin, concurrent capped stdout/stderr (16 KiB each), ten-second deadline, cancellation, kill-on-drop and bounded reap. The probe removes `JAVA_TOOL_OPTIONS`, `_JAVA_OPTIONS`, `JDK_JAVA_OPTIONS`, and (since the 2026-10-06 follow-up) OpenJ9's `OPENJ9_JAVA_OPTIONS` and `IBM_JAVA_OPTIONS`; an isolated child test sets all five and proves removal without mutating the main test runner's environment. Unknown/conflicting/failed/incompatible output refuses. Canonical plain-path, length/mtime evidence is rechecked before effects and commit; this is **best-effort**, not cryptographic executable identity or sandboxing of a caller-selected executable.

Layout:

```text
<server-only-base>/.staging/job_<uuid>/{artifact.part,receipt.part}
<server-only-base>/instances/srv_<uuid>/
  software/<compiled-provider-name>.jar
  game/                              # launch working directory, initially empty
  control/receipt.json               # completion marker, outside game subtree
```

Roots exclude actual caller-supplied client/profile/state roots in both overlap directions. Existing linked/reparse ancestors and non-plain leaves refuse; unpredictable staging and exclusive files/directories avoid replacing existing data. These std-path guards are not race-resistant protection against a hostile same-account process.

Owned files are flushed/synced. Exact destination is reserved by `create_dir`, including refusal of an empty collision. Artifact and then bounded synced receipt publish with same-volume no-clobber hardlinks; unsupported hardlinks fail closed, with **no replacing rename/copy fallback**. Receipt publication is the completion marker; cancellation afterward cannot erase it. Prior failures may leave an incomplete bundle, which is never adopted, automatically retried or started. Cleanup touches only this job's two known staging files and empty job directory, never unrelated staging or final user data. Crash leftovers require explicit reconciliation; no automatic sweep or multi-file/power-loss guarantee.

The launch DTO maps later to `LaunchSpec.javaJar(java, emptyJvmArgs, artifact, ["nogui"])`, `WrapperConfig.Builder.minecraftLaunch(...)` and `.minecraftWorkingDirectory(game)`, with `console-stop`. That mapping is **documented intent only**: no Java-agent configuration persistence/adapter was implemented.

Receipt verification currently requires the original in-memory `InstallPlan`. There is deliberately no permissive persisted-plan decoder. Restart/recovery integration must provide a strictly validated plan/state representation and canonical authority; receipt existence does not do so. Verification has a total timeout but no user cancellation API, and filesystem syscalls retain platform blocking limitations. Directory-fsync/power-loss certification, signature/provenance and distribution notice review remain release gates.

## Verification and remaining integration

Fresh Windows results, 2026-10-05: `CARGO_INCREMENTAL=0 cargo test -p piston-lib --offline --locked` passed **286 tests** (221 unit, including 89 new `server_install` cases of which eight are runtime child-fixture entrypoints, plus 65 integration), zero failures/ignored; scoped `rustfmt --check` on the module passed. Four deliberately disabled guards (approval hash, published-digest rehash, stable-build preference, raw percent refusal) were each caught by the named test, and source bytes were restored and SHA-256 checked afterward. Server `:core:test :wrapper:test --rerun-tasks` passed **891 tests / 64 suites** with one existing Unix-only skip; `requirements-ledger.mjs --check` passed and the protocol plus ledger Node suites passed **146/146**. Frontend, Tauri, whole launcher workspace, Linux/macOS and real provider/Minecraft runs were not performed. Test bodies are inert local bytes; owned Java-probe children re-enter the Rust test executable, not scripts or game JARs. Windows junction and local-volume tests execute; Unix link cases are compile-gated, not claimed tested on Windows.

### Review follow-up — 2026-10-06

The parent fixed the omitted-Linux-mount capacity fallback and removed the two OpenJ9 option-injection variables from selected Java probes. Five new Windows-executed tests exercise bounded Linux mount-table parsing/reader consumption, malformed and ambiguous entries, one-pass path escapes, unknown/network mount refusal, and exact capacity-mount matching. The isolated probe child checks all five known Java option variables while preserving its parent's environment. Both deliberately disabled follow-up guards were caught by executed tests; source bytes were restored before the full run.

Fresh Rust verification: **291 passed** (226 unit, including 94 `server_install` cases, plus 65 integration), zero failures/ignored, with scoped rustfmt checks passing. Node protocol/ledger verification: **146/146 passed** and ledger check passed; no requirement status promotion. The previous Java suite was not rerun in this Rust-only follow-up. Native Linux glue/platform behavior was not built/run, and frontend/Tauri, real provider/game execution and release certification remain unverified here.

No entire requirement or phase is fully verified. Microsoft/Minecraft UUID proof remains the intended common online-mode security flow; pack passwords remain fallback pack access, not administrative authority. No new actor/grant/capability action is introduced by provider support or approval.

Next: loader recipes/`LoaderProcessorContext`, then local launcher/headless adapters and hosting wizard over this backend. Managed signed Java, authenticated remote `content.install`, OS-local bootstrap, device mTLS/sessions, independent supervisor/protected IPC, separate OS identities/native confinement and signed/platform-certified distribution remain open. The existing Java wrapper still owns the game child and cannot provide agent-restart continuity. Do not expose/deploy the MVP as a secure completed server product.
