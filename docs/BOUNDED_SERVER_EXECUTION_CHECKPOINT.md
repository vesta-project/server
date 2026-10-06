# Bounded server execution foundation

Date: **2026-10-06**. Provider-plan step 4 groundwork, not an automatic loader installer or isolation/recovery certification. The narrow Windows fixture checkpoint is verified below; it is not hosting-product or release certification.

Companion launcher checkpoint: **`9c3116e6`**, an unsigned local commit; not pushed.

## Implementation boundary

The companion launcher's private `piston-lib::game::server_install::execution` module separates owned direct-child execution from Java version parsing. Production command construction remains the caller-selected Java executable with literal `-version`. General prepared-job invocation construction exists only under `cfg(test)` for inert owned fixtures. No public command decoder, metadata-selected main class/JVM flags, shell interpolation or Java fallback is introduced. The unsafe legacy client processor executor remains separate and client-only.

The primitive provides bounded concurrent stdout/stderr draining, null stdin, the five Java option-variable removals, cancellation/deadline admission and a process-local two-direct-child pool. Pause before spawn never keeps an acquired execution permit while waiting to resume; pause after spawn does not suspend the process or pipe draining. The selected-Java probe retains its ten-second ceiling and 16-KiB per-stream caps, further capped by the original planning deadline, including admission. Runtime overlap is refused before probing and selected binding is checked afterward. Internal job ceilings are ten minutes and 1 MiB per stream, with tighter fixture budgets and bounded literal argv. These are read/execution budgets, not CPU/memory or generated-write enforcement.

Exit disposition is private typed data: not started, confirmed direct-child exit, or unconfirmed termination. Success requires successful exit and completed bounded draining. Errors request kill of the owned direct child and allow at most two additional seconds for foreground reap. Dropped futures hand owned children/protection to a separately two-second-bounded reaper where possible; an unconfirmed exit, failed handoff, background timeout or dropped reaper conservatively quarantines admission/reservations. A kill request is not exit proof. No automatic retry/replay/reset is provided.

The private `execution_workspace` primitive reserves storage and an installation slot before creating an exclusive generated job under validated server-only roots, with fixed `inputs`, `work` and `outputs` subdirectories. Declarations reject escape/device/ADS/protected paths, duplicate/case-aliased targets and file/directory conflicts. They bound entries, exact file sizes and the combined declared footprint. Verification streams original expected digests independently of observed SHA-256, checks inputs before and after execution, scans both declared trees (including empty input roots), rejects undeclared entries and checks observed plain/reparse/hard-linked files. Final opened/reopened native file identity, link-count and metadata comparisons reject deterministic same-size/mtime path substitution. Checks remain best-effort, not race-resistant confinement.

Before execution, cleanup is restricted to exclusively created known paths and empty owned directories. Once execution starts, workspaces are retained even after confirmed direct-child exit: an unknown descendant or processor-created file must not be swept recursively. Successful output verification does not publish an installation, receipt or launch definition. The existing four-provider plan hash/receipt formats and publication path are not expanded.

## Check against the original requirements

The original `SERVER_SOFTWARE_REQUIREMENTS.md` remains the baseline, not this checkpoint's smaller plan. Exact ledger entries were checked rather than treating fixture success as phase completion:

| Original requirement | Narrow relationship to this checkpoint | Still not satisfied here |
| --- | --- | --- |
| `files-and-content-005` / `006` (lines 627–630) | Original expected digest checks; hashless declarations refused; observed SHA-256 never replaces upstream pins. | Authenticated arbitrary-content API, complete URL policy composition and signed authenticity. No fetching is added by this primitive. |
| `files-and-content-011` (643–645) | Tightenable declaration/read/time/admission limits, conservative volume admission, full declared footprint reservation plus 512-MiB safety floor. | Host-wide/interprocess accounting, generated-write/CPU/memory/network enforcement, crash-retained reservation reconstruction and complete owner configuration. |
| `resource-controls-and-diagnostics-002` (917–918) | Unknown/low capacity refuses before owned job expansion; unstarted cleanup releases known reservations. | Configurable agent-wide critical threshold, current owner recovery/cleanup authority and cross-operation policy integration. This requirement remains **not verified**. |
| `java-runtime-001` / `002` (887–891) | Selected-runtime binding and before/after evidence remain; bounded selected-Java probe is reused. | Managed signed Java, complete setup/update/launch integration and cryptographic executable identity. |
| `durable-operations-and-concurrency-001` / `006` / `008` (807–832) | In-process cancellation/unknown-exit ownership, retained incomplete workspace, no blind replay or automatic rollback. | This Rust primitive is not composed with canonical durable operations, journalled rollback, actor/audit/current-authority checks or restart reconciliation. It does not create the durable `RECOVERY_REQUIRED` state. |
| `scheduling-and-notifications-009` (780–782) | Fixed job cwd, literal parameter bounds, time/output controls and input pins inform future execution recipes. | No script-preset executor, complete environment allowlist, dedicated identity or schedule authority. Clearing five JVM variables is not an environment allowlist. |
| `isolation-003` (876–879) | **Not satisfied.** Workspace validation and owned direct-child handles are limited engineering controls. | Dedicated unprivileged/separate game-control identities, strict ACLs, process-tree ownership, resource limits and network policy remain mandatory baseline release gates. Optional stronger sandboxing does not make those baseline controls optional. |

This is not Minecraft's independent supervisor or process-generation control. Agent-crash continuity/protected IPC/readiness, Microsoft UUID proof/device mTLS/sessions, signed distribution and real platform/software certification remain open. Local review consent and implementation support confer no management authority.

## Verification and limitations

Fresh Windows evidence on **2026-10-06**, after exact mutation restoration:

| Gate | Actual result |
| --- | --- |
| `CARGO_INCREMENTAL=0 cargo test -p piston-lib --offline --locked` | **348 passed**: 283 unit + 65 integration; zero failures/ignored. |
| Focused `game::server_install` regressions | **140 passed**; zero failures/ignored. |
| `CARGO_INCREMENTAL=0 cargo check -p piston-lib --offline --locked` | Passed; production constructors remain sealed. Existing unrelated unused `tokio::task` warning remains. |
| Scoped eight-file `rustfmt --check --edition 2021 --config skip_children=true` | Passed. |
| Node protocol/ledger tests plus `requirements-ledger.mjs --check` | **146 passed**, zero failures/skips; ledger check passed. |
| Independent read-only review | Six concrete findings fixed; follow-up found no remaining concrete defect in the reviewed fixes. The additional deterministic reaper-shutdown hook/test was also inspected. No review execution evidence is claimed. |

Parent-authored regressions cover spawn-error cleanup/protection reuse, no-spawn refusals, running-child cancellation/deadline and future abort, native runtime-shutdown quarantine, pause while queued, planning's absolute probe deadline, pre-probe runtime/root refusal, original input/output pins and undeclared input/output scans (including empty roots), native hard links/junctions, same-size/mtime replacement and an inert complete job without publication. This adds **46 reported Rust unit cases**, including two guarded fixture entrypoints; raw harness totals do not mean 46 complete requirements are satisfied.

Four mutations were actually compiled and executed, each failing its named test rather than compilation:

| Weakened guard | Executed failing test |
| --- | --- |
| Stream cap removed | `execution::owned_stream_floods_are_independently_refused_and_children_reaped` |
| Unknown direct-child protection released | `execution_workspace::uncertain_job_retains_workspace_installation_slot_and_volume_reservation` |
| Quarantined job protection released | Same uncertain-job test; installation admission was incorrectly reusable. |
| Original expected output digest bypassed | `execution_workspace::output_digest_size_presence_and_undeclared_entries_are_verified` |

Exact source bytes were restored before authoritative verification. Restored execution/workspace SHA-256 values were `9dc7a40c1877058350ddd3b86052f8746d7184a0b66129bca32f9780e8277744` and `489cfe0330e330264222d0c0a6afbe8e2dbf70c0fb5469bfd48ff99cf7c2a137` respectively (raw worktree bytes, not a release signature). Logs: `V:/bounded-execution-final-{full,focused,node}.log`, `V:/bounded-execution-final-nonincremental-check.log`, and `V:/bounded-execution-mutation-{stream-cap,unknown-child-protection,unknown-job-protection,original-output-pin}.log`.

Failed intermediate evidence is retained honestly: initial compile integration mistakes, an async stack overflow corrected by bounded heap buffers, and a junction creation failure corrected with separate native path components. A later 33-pass/one-setup-failure run was not final passing evidence; fresh final runs execute the native guard. An earlier successful production check emitted an incremental-cache access-denied note; the final nonincremental check passed without changing permissions or deleting cache data.

The ledger remains **370 entries: 59 partially verified, 311 not verified, zero fully verified**. No entries or phases are promoted by this checkpoint. Fresh Java/frontend/Tauri/full-workspace/native Linux/macOS/real-loader verification was not performed.

Fixtures execute only the owned Rust test binary, not downloaded installers or Minecraft. They do not accept an EULA, alter a client profile or create a server bundle. Native Windows evidence is not Linux/macOS evidence. Filesystem checks retain hard-link/reparse/ancestor TOCTOU and trusted-root assumptions; deadlines do not certify interruption of blocking filesystem/syscall work. Child working directories do not restrict filesystem/network access, and direct-child reap does not prove descendants stopped. Quarantine is process-local and can require manual recovery; restart is not proof of child termination. There is no automatic sweep or durable recovery registry.

Fabric, Quilt, Forge and NeoForge remain explicitly unavailable. Their next checkpoint must supply reviewed compiled recipes, strict server-side substitutions/class policy, complete verified inputs/outputs and exact plan/consent/multi-file-receipt integration—not reuse the client runner or enable the descriptors early. Managed Java, modern hosting UI/headless/config adapters and authenticated remote installation remain separate open work.

The parent authored all new production code and tests. The protected user Tauri manifest, modern UI/manual imports and historical status JSON are preserved. No requirement status or phase is promoted; no real provider execution, EULA/service/network/signing/publishing effects are authorized by this evidence.
