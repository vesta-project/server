# Typed launch definition checkpoint

Assessment date: **2026-10-05**. This implements step 2 of the [server software provider plan](SERVER_SOFTWARE_PROVIDER_FRAMEWORK_PLAN.md): a **shell-free launch definition that is not assumed to be `java -jar server.jar`**, wired into the existing generation-bound lifecycle. It prepares the process layer for software providers and native servers. It does **not** download or install any software, certify any server implementation, add a stop/readiness protocol, supervise independently of the agent, or change authority. Schema stays **4**; no launcher source changed.

## What exists

`com.vesta.wrapper.launch.LaunchSpec` is an immutable, filesystem-free value with three shapes:

| Kind | Command |
| --- | --- |
| `java-jar` | `java [jvm args] -jar JAR [args]` |
| `java-argfile` | `java [jvm args] @FILE… [args]` (the layout loader installers generate) |
| `native` | `EXECUTABLE [args]` for servers that are not Java programs |

Every definition also carries a **stop mode**: `console-stop` (graceful stop writes the single console line `stop`) or `unsupported`. A native definition **must** state its stop mode; it is never assumed. Strict factories validate the java command, every argument and every path (non-blank, ≤4,096 characters, no control or line-separator characters, ≤256 arguments, ≤16 argument files). Messages and `toString()` never echo values or paths, because arguments may carry secrets. A separate lenient `legacyJavaJar` factory reproduces the pre-existing `minecraft.*` configuration byte-for-byte, so no previously accepted configuration is newly rejected.

`WrapperConfig` exposes the effective definition as `minecraftLaunch`: either an explicit `Builder.minecraftLaunch(spec)` or the equivalent legacy `java -jar` definition. Supplying both an explicit definition and the legacy launch jar fails the build. Relative paths resolve against the working directory.

`ConfigLoader` accepts a strict `[launch]` table (`kind`, `java-command`, `jvm-args`, `jar`, `argfiles`, `executable`, `args`, `stop`). Only the keys valid for the chosen kind are allowed, with exact types; unknown keys, wrong types, missing required values, an invalid stop mode and every `LaunchSpec` validation failure fail closed with a fixed message that does not echo the value. The table cannot be combined with `minecraft.launch-jar`, `java-command`, `jvm-args` or `server-args` (even with an empty value), so two definitions are never merged or chosen between. A missing table leaves the legacy behavior unchanged. The new table has no implicit `nogui`; the legacy form keeps its default.

`MinecraftServerProcess` builds its command from the definition and keeps the generation, effect-gate and single-lock behavior unchanged. New known **no-effect** failure codes, raised before the gate and before any process exists: `launch_argfile_missing`, `launch_executable_missing`, `launch_executable_not_executable` (alongside the unchanged `launch_jar_missing`, `working_directory_missing`, `not_configured`). `snapshot()` additionally reports `launch_kind` and `graceful_stop_supported`.

**Unsupported stop is never invented.** For a definition with stop mode `unsupported`, `stopGeneration` throws `graceful_stop_unsupported` after the stale-generation check but before the gate, any state change or any console write; the legacy `stop()` returns `false` at once without a state change, wait or write; `restart()` and the legacy HTTP stop route refuse with a clear message and start nothing. The same canonical `game.stop` operation therefore fails as a known no-effect failure (`externalEffectStarted = false`, locks released) while the process keeps running. Force kill is unchanged: separate exact authority, exact generation and confirmation, never an automatic fallback.

## Parent-authored verification

New cases (all authored by the parent session; read-only agents wrote no code or tests): `LaunchSpecTest` 26, `ConfigLoaderLaunchTest` 60, `LaunchShapesProcessTest` 17 (one Unix-only execute-permission case skips on Windows), `AgentRuntimeLaunchShapesTest` 3, `LegacyPowerRouteLaunchTest` 2 — **108**. Coverage: exact commands for each shape; immutability and input copying; the strict value matrix and boundaries without echo; the lenient legacy path; path resolution; wire names; the full strict `[launch]` matrix (39 invalid shapes), every legacy-key conflict and builder conflicts; the legacy HTTP power route over real HTTP; and **real owned child JVMs** through every shape that record the exact arguments and working directory they received, including shell metacharacters (`;`, `&`, `$HOME`, `%PATH%`, `<`, `|`, backtick, spaces) passed literally with no shell side effects. Unsupported-stop tests use a fixture that would exit on a written `stop`, so a surviving process proves nothing was written; they also prove no gate call, no `STOPPING` state, no console output, immediate legacy return, refused restart, stale-generation precedence, working force kill, and a fresh generation afterward. Preflight failures are shown to create no process and leave no start marker.

Eight mutation spot-checks each made the relevant tests fail, and the sources were restored: removing the typed or the legacy stop guard, the legacy HTTP route guard, the config conflict check, the per-kind key strictness, the argument-file preflight, the control-character validation, or the builder's conflicting-definition guard.

The native shape is exercised by using the test JVM as a direct executable. This proves the direct-execution path, **not** compatibility with any native Minecraft server such as FerrumC.

Two test-infrastructure changes, no production behavior change: `FakeMinecraft` now records its received arguments and working directory; and `AgentRuntimeLifecycleTest` teardown retries deleting its temp tree, because a first run of the existing lifecycle class showed an intermittent Windows `@TempDir` cleanup failure (the game's working-directory handle released shortly after exit) in two tests that restart the game, and one more occurrence in three reruns. Whether that race predates this slice was not isolated; the retry makes the check robust and does not mask an assertion. Gradle's build cache can also replay old test results: stability and mutation runs used `--no-build-cache`.

Final fresh verification on **Windows, 2026-10-05**, after all production and test edits:

| Gate | Result |
| --- | --- |
| `:core:test :wrapper:test --rerun-tasks --no-build-cache --offline --console=plain` | **687 tests / 60 suites passed**: core 78 / 20, wrapper 609 / 40 (previously 501 / 35); zero failures or errors; **1 skipped** (the Unix-only execute-permission case). |
| Repeated real-process runs | `LaunchShapesProcessTest` passed on four forced reruns and `AgentRuntimeLaunchShapesTest` on three, with distinct timestamps and the build cache disabled. An earlier batch of repeats was replayed from Gradle's cache and is not counted. |
| `node --test scripts/requirements-ledger.test.mjs` / `node scripts/requirements-ledger.mjs --check` | **5 of 5 ledger tests passed**; all **370** entries consistent: **53 partially verified, 317 not verified, zero fully verified**. Only `installation-components-and-process-continuity-010` moved (not verified → partially verified) with narrow evidence; its sibling script-preset, takeover and readiness entries (011–014) are unchanged. |
| `git diff --cached --check` | Passed. |

An earlier run of a first draft of this slice failed two existing lifecycle tests in `@TempDir` cleanup (described above); a later rerun passed. A test of my own failed once on a start-marker race (the second JVM had not yet written its marker); it now waits for the marker. The route test above (`LegacyPowerRouteLaunchTest`, 2 cases) was added after noticing the legacy HTTP stop change had no coverage; disabling its guard made it fail.

No launcher checks were rerun: no launcher source changed. Existing Java 8 source/target and deprecation warnings remain. Tests used temporary directories and the owned fake-game JVM only; no real Minecraft server, installer, demo reset, service, firewall, signing configuration or public deployment was touched.

## Explicit remaining gates

- No installation, download or provider support; no software is selectable or certified. The wrapper still launches only what the local configuration names.
- No remote API creates or edits a launch definition today; the requirement that changing executables or code-loading JVM arguments is a separately authorized trusted capability still needs its canonical capability, local-only protection and tests. Local presets for `.bat`/`.ps1`/shell scripts, existing-service adapters and script-takeover onboarding are not implemented.
- No readiness model beyond process-alive, no per-software stop protocol beyond the console `stop` line, no save/flush adapter, no crash-restart policy.
- The environment is inherited from the wrapper; no environment control is added. Arguments containing double quotes on Windows follow Java's existing command-line quoting.
- Executable-permission refusal is verified only where the filesystem records an execute bit (not on this Windows host). Linux execution, real server software, loaders and FerrumC are not exercised.
- The wrapper's full shutdown still asks the game to stop when it can; with an unsupported stop it now leaves the game running. Independent-supervisor continuity, protected IPC, separate game identity and the other release gates are unchanged. Do not describe the product as secure, complete or deployable.

Progress commits remain unsigned under prior authorization; nothing is pushed.
