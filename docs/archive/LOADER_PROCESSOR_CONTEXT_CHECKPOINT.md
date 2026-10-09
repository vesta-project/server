# Loader processor context: sided metadata foundation

Date: **2026-10-06**. This is a small part of provider-plan step 4, not an automatic mod-loader installer or an execution-safety certification.

## Delivered

The companion launcher's `crates/piston-lib/src/game/installer/modloaders/processor_context.rs` defines a closed `ProcessorSide::{Client,Server}` and an immutable `LoaderProcessorContext`. It provides only:

- The exact compiled `client` or `server` label; there is no implicit/default or metadata-selected context.
- Processor-side membership: absent `sides` means both, empty means neither, and explicit labels match exactly. Unknown/case-variant labels do not match; the old client behavior of accepting a list containing `client` alongside unknown labels remains unchanged. This filter is **not** strict recipe validation.
- Selection of `ModrinthSidedDataEntry.client` or `.server`, without substituting the other side when the selected value is empty.

The existing `execute_loader_processors` signature and callers are unchanged. It explicitly constructs **Client** context, retains its existing standard-data injection and routes filtering/argument-data selection through that context. The private argument resolver can be exercised with either context in tests, but no new public server processor executor is added.

The parent authored all new code and tests; a read-only research agent located the existing seam. No agent wrote production code or tests.

## Boundaries

A context selects data; it does not prove ownership, reserve paths, validate an installation plan, approve a processor class or execute anything. It does not grant `content.install`, administrative authority or a protocol feature.

**Do not reuse the legacy client runner as a server installer.** It still inherits its client pipeline's permissive argument resolution, malformed-Maven fallback and unknown-token behavior, metadata/JAR-manifest main-class selection, Java fallback, and unbounded processor lifetime/output. This context extraction does not fix or certify those behaviors. Future server execution needs compiled reviewed recipes, verified complete inputs, bounded deterministic substitutions with unresolved-token refusal, trusted server-specific root/library/game-artifact bindings, owned bounded children, cancellation/reaping, validated outputs, exact consent, staging/receipt integration and recovery rules. None of that is implied by selecting Server.

Fabric/Quilt/Forge/NeoForge remain explicitly unavailable in `server_install`; all four are still initial-delivery goals. Official Fabric/Quilt server mechanisms and Forge/NeoForge sided processor policies must be resolved separately, rather than relabelling client profiles as server installs. No descriptor is promoted to an automatic backend here.

## Verification

Fresh Windows verification after both context mutations were caught and source bytes restored: `CARGO_INCREMENTAL=0 cargo test -p piston-lib --offline --locked` passed **302 tests** (237 unit, including **11 new loader-context/runner cases**, plus 65 integration), zero failures/ignored. Scoped rustfmt checks passed. Protocol/ledger Node regressions passed **146/146**, and `requirements-ledger.mjs --check` passed. Requirement statuses remain 59 partial / 311 not verified / zero fully verified. The Java suite was not rerun in this Rust-only checkpoint; its prior results remain dated evidence, not a new run.

The new tests cover both filter sides, absent/empty/unknown/case-variant labels, exact sided values/no fallback, whole/inline placeholders, Maven values/direct references, and unchanged client literal/unknown-token/fallback behavior. An actual non-dry legacy-runner call proves server-only/empty/unknown filters skip, while client/both/absent filters reach the expected missing fixture-JAR refusal. The selected Java path is deliberately nonexistent; no processor or game is executed, no download occurs, and the temporary fixture root stays empty.

This is not real Forge/NeoForge/Fabric/Quilt compatibility evidence. No native Linux/macOS, frontend/Tauri, real artifact installer, Minecraft/EULA, networking/service, signing or publishing work is performed. The separate user Tauri manifest remains excluded. No requirement status or entire phase is promoted.

## Next

Implement a separate bounded server processor/installer execution foundation with trusted per-job path bindings and output verification, then integrate provider-specific server recipes into the exact reviewed plan/new-bundle receipt path. Managed Java, hosting UI/headless/config persistence, authenticated remote installation, identity/mTLS, independent supervisor, native confinement and signed/platform-certified distribution remain open.
