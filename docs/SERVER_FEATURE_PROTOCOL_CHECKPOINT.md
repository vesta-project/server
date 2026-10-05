# Server-feature discovery protocol checkpoint

Assessment date: **2026-10-05**. This adds the discovery/negotiation portion of provider-plan step 1, after the completed [launch-definition checkpoint](LAUNCH_DEFINITION_CHECKPOINT.md). It is a **draft, transport-neutral, no-listener contract**, not a complete management API or release. Schema stays **4**. No launcher source changed, no compatibility migration was added, and no legacy route was expanded.

## What exists

- Normative [draft-v1 specification](../protocol/README.md), JSON Schema 2020-12 request/manifest/error schemas, canonical golden bytes, and one shared **129-case** wire corpus.
- Immutable Java `ServerFeatureProtocol` models and `ServerFeatureCodec`: highest-common-version negotiation, response-context consistency checks, exact known feature queries, strict shape/type/duplicate/numeric-lexeme/UTF-8/BOM/trailing-content checks, copied/sorted collections, hard **16,384-byte complete input/output cap**, and fixed input-free errors without causes. Encoding has bounded ASCII fields/collections as well as the byte check; it neither truncates nor filters unsupported entries.
- Separately implemented dependency-free **Node peer**, usable without Java. Its own parser retains duplicate-key and integer-lexeme evidence; caller-created values are validated before bounded encoding. It consumes the same physical fixtures and produces identical canonical bytes. The CLI accepts one bounded stdin message, has no listener, and prints only a fixed error envelope on refusal.
- Trusted `AgentRuntime.featureDiscovery()` composition. The manifest uses the persisted canonical server UUID, not the legacy advertised ID. Start/kill require a configured launch definition; graceful-stop support follows its explicit stop mode. Task replay describes the existing sanitized **in-process** service, not a wire replay API. Actual readiness and independent-supervisor continuity are explicitly unsupported. Missing artifacts are not preflighted or claimed executable/ready. Reporting creates no game, identity, grant, operation, confirmation or event.
- Known features are separate from canonical capabilities. Unknown valid IDs/reasons are inert descriptions, never dispatch, permissions, identity or executable policy. A client response check matches offered versions/expected UUID only; it cannot prove identity or defeat a transport downgrade attack.
- Node 22 conformance/ledger checks are added to CI alongside the existing Java suites. The CI workflow has been edited, **not run remotely**.

## Parent-authored verification and review

All production code, Node peer and new tests were authored by the parent. Subagents performed read-only semantic, codec and composition reviews; they wrote no code or tests.

The shared corpus exercises valid canonical/reordered/escaped/unknown/boundary inputs and invalid duplicate members/IDs/versions, unknown fields, null/wrong types, forbidden permission/command metadata, version lexical forms, missing fields, status/reason mismatches, invalid IDs/UUIDs, Unicode/lone-surrogate text, malformed UTF-8/BOM, nesting/trailing content, 64-feature/list bounds, and exact complete-byte limits. Both peers independently compare actual encoding to expected canonical bytes; a self-only round trip is not the sole interoperability assertion.

The runtime suite has **15 cases** across no launch, Java JAR/argfiles, native, and both stop modes. Counts are measured **after** opening the runtime so discovery is not confused with existing startup state/runner work. Tests prove canonical ID persistence on reopen, no owner/process creation, no path/argument leakage, unknown features cannot register actions, a forged supported report with the correct UUID cannot authenticate, current exact deny/revocation still prevents submission/replay, and an owned fake-game child cannot be killed by a support declaration: force kill stays awaiting exact confirmation until canonically confirmed.

### Review defects fixed before completion

A read-only codec reviewer found two reproducible Node object-API bugs: sparse arrays escaped `.map()` validation, producing wire nulls / accepting an invalid version offer; and the support helper accepted conflicting duplicate feature declarations without validation. The parent reproduced both and added **four failing regression tests before the fix**. Array validation now uses `Array.from` to visit holes, and the support helper validates the manifest first. All **141 Node conformance tests** passed afterward. The reviewer confirmed both fixes. These defects concerned descriptive peer APIs, not a permission bypass.

The parent also aligned Java programmatic selected-version range checks with wire/Node checks: outside 1–65,535 is malformed; in-range unimplemented versions are unsupported.

## Final verification

Fresh local verification on **Windows, 2026-10-05**, after all source/review fixes and after restoring every mutation:

| Gate | Result |
|---|---|
| `:core:test :wrapper:test --rerun-tasks --no-build-cache --offline --console=plain` | **891 tests / 64 suites**: core 78 / 20, wrapper 813 / 44; zero failures/errors; **1 skipped** (existing Unix-only execute-permission case on Windows). Build succeeded in 1m57s. |
| New Java cases | **204**: `ServerFeatureProtocolTest` 54, `ServerFeatureCodecTest` 6, `ServerFeatureConformanceTest` 129, `AgentRuntimeFeatureDiscoveryTest` 15. All executed with no failures/skips. |
| Node conformance and ledger | **146 passed**, no failures/skips: 141 protocol checks (including the 129 shared wire cases), plus all 5 ledger tests. Java/Node wire cases intentionally overlap; these are not 345 unique scenarios. |
| Mutation spot checks | **8 caught by actual test assertions**, not compile failures: Java byte cap (3 failed cases), duplicate decoded keys (7), integer spelling (5), no silent version fallback (3), exact feature matching (1), runtime unsupported stop (2), canonical identity (6); Node duplicate decoded keys (7). Sources were restored byte-identically before the authoritative full run. |
| `node scripts/requirements-ledger.mjs --check` | Passed, **370** entries: **53 partially verified, 317 not verified, zero fully verified**. Only narrow evidence was appended to the already-partial compound protocol entry; no status or phase promoted. |
| `git diff --cached --check` | Passed for the complete scoped source/spec/fixture/test/documentation changes. |

The only pre-fix red test run in this slice was the intentional four-case Node review-regression reproduction; it passed after the corrections. Guard mutation failures were intentional and superseded by the restored-source full run. There was no final product-test failure. An initial ad hoc XML-total extraction script failed because of regex escaping; a corrected attribute parser successfully read the fresh XML totals above. Existing Java 8 source/target and deprecation warnings remain. No Gradle cache replay is counted as verification.

## Remaining gates and limits

- No HTTPS/OpenAPI discovery/action route, authentication proof, listener, production wire event feed, published deprecation window or complete API negotiation. This is narrow schema/codec/local-discovery evidence, not completion of P1/P8 or the compound protocol requirements.
- A supported mechanism is **not** currently runnable, ready, remotely available, authorized, or certified. A report is self-reported, not a software/runtime identity proof. No new permissions or grant path exists.
- Schema-structure/selected-assertion checks are not a general-purpose third-party JSON Schema validation engine; lexical, duplicate-key, total-byte, duplicate-ID and response-context rules additionally live in the normative contract/codecs.
- No software descriptors/registry, verified downloader, automatic Vanilla/Paper/Folia/Purpur/Fabric/Quilt/Forge/NeoForge provider, loader processor or setup wizard. No real server/software compatibility is certified.
- Independent supervisor/protected IPC, OS-authorized bootstrap, Microsoft UUID proof, device enrollment/mTLS/sessions, separate game identity/native confinement, removal of legacy trust routes and signed distribution remain release gates. Do not expose the MVP as a completed secure product.
- Local verification is Windows-only; CI/Linux has not been run here. The real process test uses the owned fixture JVM, not Minecraft, FerrumC or an installer. No real downloads, EULA acceptance, demo erasure, service/firewall/router/signing-config change, push or public deployment.
- Existing GPL-3.0 remains; no separate license or public repository is created. The launcher user's `vesta-launcher/src-tauri/Cargo.toml` change remains untouched. Progress commits are unsigned under prior authorization, not pushed.
