# Rust-native trusted launch configuration

Date: **2026-10-09**. Status: **implemented, scoped-reviewed and Windows-tested bounded adapter; migration and final whole-rebuild review incomplete**.

This continues the approved useful-current-Java migration, not a reconstruction of
`WrapperConfig`, a new application executable, or completion of future product requirements.
Java sources/tests/builds remain intact. The full configuration loader, protected configuration
file opening, authenticated transport, host startup and discovery composition remain unfinished.

## Application contract

`vesta_game_runtime::launch_config::LaunchConfiguration` has private validated fields, a
redacted `Debug`, no public deserializer/serializer and no authority. Its parser accepts an
explicit absolute host base plus at most **65,536 UTF-8 bytes** of launch-only TOML:

```toml
[minecraft]
working-directory = 'game'

[launch]
kind = 'java-jar'
jar = 'server.jar'
jvm-args = ['-Xms1G', '-Xmx3G']
args = ['nogui']
```

`parse_toml(bytes, base)` performs no file opening, executable discovery, spawn or automatic
start. `[minecraft]` accepts only `working-directory`; `[launch]` is a closed typed union:

| Kind | Required fields | Optional fields | Literal argv |
| --- | --- | --- | --- |
| `java-jar` | `jar` | `java-command`, `jvm-args`, `args`, `stop` | JVM args, `-jar`, resolved JAR, args |
| `java-argfile` | nonempty `argfiles` (maximum 16) | `java-command`, `jvm-args`, `args`, `stop` | JVM args, each ordered `@` resolved file, args |
| `native` | `executable`, `stop` | `args` | args only |

Java stop defaults explicitly to `console-stop`; `unsupported` may be selected. Native stop
must be stated. There are no invented `nogui` args, executable-search fallback, shell,
environment override, script registration, restart, readiness or loader execution semantics.
Duplicate fields, malformed TOML, wrong types, unknown/cross-kind keys, unsupported kinds,
legacy launch fields, `auto-start`, and unrelated security/server tables refuse with fixed
input-free errors. Missing/empty input never enables open security or a legacy runtime.
This is intentionally a **separate launch-only document**, not a drop-in full wrapper-config parser.

Paths are resolved lexically against the explicit base/working directory and normalized with
native Rust path components; parsing may precede artifact preparation. Base must be absolute.
Windows partial-root/drive-relative paths refuse rather than acquire an implicit drive/cwd.
Resolution is not native confinement; trusted paths can leave the working directory and lexical
normalization can differ from symlink traversal. The adapter does not open/protect configuration
files, parse JVM argfile contents, detect Java, verify executable/artifact hashes or isolate a
same-account game. Those responsibilities are not silently certified by regular-file preflight.

`into_launch_spec(selected_java)` consumes the plan into the existing process `LaunchSpec`.
An omitted Java executable requires the backend's explicit selected absolute Java path; there
is no PATH lookup. Configured executable paths resolve against cwd, not PATH. Compilation
preflights executable/cwd and all JAR/argfile references, and retains required files privately
in `LaunchSpec` so **actual spawn preflight rechecks them before the fresh effect guard**.
No stored executable/path/argument may be selected by lifecycle-operation input. `LaunchSpec`
continues to reject `.bat`/`.cmd` on all platforms and enforce aggregate UTF-8/conservative
Windows quoting budgets; stdin, generation ownership and guarded Stop/ForceKill are unchanged.

## Behavior dispositions

| Retained Java behavior/test | Rust equivalent or explicit disposition |
| --- | --- |
| `LaunchSpec` Java JAR / ordered argfile / direct native shapes | One typed launch parser produces existing literal process spec; native fixture records exact argv/cwd for each shape. |
| stop mode and unsupported graceful stop | Closed stop values; native requires a value; actual unsupported stop refuses without stdin effect and retains generation. |
| immutability, defensive copies, null setters, `withStopMode` copies | Private owned validated Rust data, consumed plan, typed required fields; no nullable mutable Java-style builder/copy API. |
| malformed text/path/list/kind and redacted rendering/errors | Strict typed TOML parser, UTF-8/document/path/assembled argv bounds, forbidden controls/separators, parser recursion guard and fixed errors. |
| literal shell metacharacters / Unicode | Actual inert child records unchanged args, including empty literal args and quoted/spaced/Unicode text; no shell side-effect file. |
| relative/absolute path normalization | Explicit absolute base, native path components, lexical normalization; no cwd/PATH inference. |
| `equals`/`hashCode`, Java whitespace / UTF-16 character-count quirks | Not reconstructed; process values stay private/non-serializable, with Rust UTF-8 and native command budgets. Empty literal args remain valid. |
| permissive `legacyJavaJar`, legacy launch defaults and automatic-start/shutdown paths | Not carried forward; invalid/legacy/automatic-start config refuses. Execution uses admitted, currently authorized closed operations. |
| `ConfigLoader` full TOML file opening/state/security/server/pack configuration | Still a gap. This adapter only covers explicit launch-table semantics and does not advance the full loader or `WrapperConfig` family. |
| protected config-file link/reparse/unreadable/missing handling | Still a gap at the future host/file-opening boundary; bytes parser is not evidence of safe native file opening. |
| JVM/loader/argfile interpretation or real Java compatibility | Not executed; inert executable witnesses argv only. Managed Java, installer/receipt launch provenance and real loader matrix remain separate gates. |

Applicable original scope includes `installation-components-and-process-continuity-010`
(ordinary literal argv/cwd and trusted code-loading operational capability). This is bounded
library/application evidence, **not whole-requirement promotion**. Advanced approved script
presets, onboarding/script conversion, independent supervisor/IPC, authentication, world/config
preservation during activation, platforms/MSRV and signed distribution are still unfinished.
Microsoft/Minecraft identity is the normal identity path; configuration possession never grants
management authority. Pack passwords, OP, connector and loopback remain non-administrative.

## Inventory accounting boundary

Only the two bounded `LaunchSpec.java` and `LaunchSpecTest.java` rows are mapped to this
Rust-native equivalent with the above explicit behavior dispositions. They are **migrated,
not verified**. The preserved 222-entry inventory becomes **26 migrated, 17 retained Java
plugin, 179 not_migrated, 0 verified**. All frozen Java byte hashes, 370 original requirement
IDs/text and original requirement statuses/history remain unchanged. The applicable ordinary
literal-launch requirement is linked for traceability, not promoted to complete.

`ConfigLoader`, `WrapperConfig`, their full test families, host startup and the other operation/
transport/file/pack families are not promoted by this subset. The mandatory final independent
whole-rebuild review remains `not_run`; scoped source review cannot satisfy that gate.

## Executed verification / failed-attempt history

Native Windows 11 / Rust 1.96.0 only. No real Java, Minecraft/provider/installer/Mojang execution,
EULA acceptance, world/demo deletion, service/firewall/router/signing changes or publication.

- Fresh pre-increment workspace baseline passed **635 tests**, zero failed/ignored; workspace
  format/all-target check/strict Clippy and Node **159** plus inventory/ledger checkers passed.
- Pinned TOML dependency resolution/initial all-target runtime compilation passed. That command
  preceded the adapter implementation and is dependency evidence, not adapter verification.
- First adapter command: formatter passed; test compilation failed because a new test used the
  wrong `start_guarded` signature (owned spec/zero-argument callback instead of borrowed
  spec/snapshot callback). Corrected the test to use the existing production signature; no
  production interface or guard was weakened. No tests or strict lint completed in that attempt.
- Second runtime attempt: **42 passed, 2 failed, 0 ignored** in the unit binary. Exact argv
  expectations used mixed `/` separators in Windows expected strings. Corrected the independent
  expectations by joining native path components; retained full exact argv assertions.
- Fresh complete `vesta-game-runtime` suite then passed **78 tests**, zero failed/ignored,
  including **13 new parser/shape tests and 6 actual inert-process witnesses**; strict all-target
  runtime Clippy passed. A new private canonical-admission/driver composition witness was then
  added separately and is not covered by that earlier 78-test result.
- The canonical configured-launch composition suite then passed **2 tests** and the integrated
  whole-workspace all-target check/strict Clippy passed. This preceded the latest driver fix.
- Independent Sol rereview of the previous operation checkpoint found a return-path defect:
  observation uncertainty raised inside dispatch could be retained internally but omitted from
  the final driver result after successful later polls. The parent merges sticky typed native
  failure into `first_failure` after dispatch, preserving any earlier failure. Two regressions
  cover late-dispatch injection after a real Start and earlier-error preservation. The injected
  private transition is **not** an actual native polling-syscall fault. The first regression
  compilation failed on two mistaken test-only `runner_lock` calls; corrected to the existing
  `acquire_runner_lock` API, then the focused driver suite passed **14 tests**, none failed/ignored.
- Independent Sol source rereview closed the propagation defect and found one test race:
  the inert fixture flushes console readiness before creating `commands.txt`. The new unsupported-
  stop witness could read too early. The parent added a bounded actual-file-creation wait before
  the exact refusal/generation/empty-bytes assertions, and Sol directly rereviewed that fix. No
  remaining concrete defect was found in this scoped source review. The reviewer ran no tests,
  did not audit dependency internals, and did not provide whole-migration/final-rebuild approval.
- A bounded Luna accounting pass covered only `LaunchSpec`/`LaunchSpecTest`. Its initial claim
  of a native-stop default was disproved by direct source inspection (the serde default attaches
  to `args`, not `stop`) and explicitly withdrawn. Parent accepts no unverified helper finding.
- The first settled-source verification stopped at formatter check (exit **1**): rustfmt
  required a multiline layout for the new wait assertion. No check/tests/lint ran in that
  command. Applied formatting without changing behavior, then started a fresh complete chain.
- The earlier whole-workspace chain completed successfully: **658 tests**, zero failed/ignored,
  **37 test-result summaries**, all-target check and strict Clippy passed. It began before the
  test-race fix and does not substitute for the separate settled-source rerun after formatting.
- Fresh Node contracts/accounting passed **159 tests**, zero failed/cancelled/skipped/todo.
  Both parity/requirement `--check` commands passed. A parent comparison against HEAD verifies
  only the two named launch rows changed; all baseline hashes, frozen requirement ID/text pairs,
  platform gates and final-independent-review gate remain unchanged.
- The separate fresh settled-source chain then passed (exit **0**): formatter check, whole-
  workspace all-target offline locked check, **658 Rust tests** with zero failed/ignored across
  **37 test-result summaries**, and strict whole-workspace all-target Clippy. This rerun includes
  the formatted, source-rereviewed command-log wait and both driver regressions. Its control unit
  binary passed **304 tests**. No earlier-source/component result substitutes for this run.

The final settled-source command chain uses the existing native Windows workspace:

```text
cargo fmt --manifest-path V:/server-connections-mvp/Cargo.toml --all -- --check
CARGO_INCREMENTAL=0 cargo check --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --all-targets --offline --locked
CARGO_INCREMENTAL=0 cargo test --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --offline --locked -- --test-threads=1
CARGO_INCREMENTAL=0 cargo clippy --manifest-path V:/server-connections-mvp/Cargo.toml --workspace --all-targets --offline --locked -- -D warnings
node --test V:/server-connections-mvp/scripts/*.test.mjs
node V:/server-connections-mvp/scripts/rust-wrapper-parity.mjs --check
node V:/server-connections-mvp/scripts/requirements-ledger.mjs --check
```

Fresh companion `cargo check --all-targets --offline --locked` and serial
`cargo test --offline --locked -- --test-threads=1`, both against
`V:/launcher-server-mvp/crates/piston-lib/Cargo.toml`, passed: **210 tests**, zero failed/ignored.
Only the pre-existing unused `tokio::task` warning remains; no companion strict-Clippy pass is
claimed. No launcher formatting or source edits were made for this increment. The companion
retains only its pre-existing protected manifest modification, with exact raw SHA-256
`4f0e1eef699c54f1c1ee09007a3eca7e932e7b04a5ed15f4db73ddbfe0d01251`.

Dependency intake is recorded in [DEPENDENCY_DECISIONS.md](DEPENDENCY_DECISIONS.md).
No Java inventory hash or frozen requirement text/status/history is replaced. Parity statuses
and mandatory final whole-rebuild review must not be inferred from the source or test count.
