# Server Java runtimes (D5)

Owner-only, capability-checked Java management follows the launcher's Settings ?
Java model: detected host runtimes, selected runtime, and managed runtimes per major.
`GET /api/v3/manage/java` returns `runtimes`, `selected` (nullable),
`custom_paths_allowed` and `managed_majors`. Each runtime has opaque `id`, host
`path`, `major`, full `version`, `vendor`, `arch`, and `managed`. Paths describe the
**agent host**, not the launcher's machine. Discovery probes up to 64 existing
candidates from JAVA_HOME, PATH, Windows Program Files vendor directories (Java /
Oracle, Adoptium / Temurin, Microsoft, Azul / Zulu), Linux /usr/lib/jvm, /usr/java,
/opt roots, macOS JavaVirtualMachines/Homebrew, ~/.jdks, and private managed storage.
Probes run `-XshowSettings:properties -version` with 5 s and 32 KiB per-stream limits,
no stdin, hidden Windows windows, and Java option-injection environment vars removed.

`POST /java/select` takes `{runtime_id,custom_path}` with exactly one non-null.
A custom executable is accepted only after canonical containment in an absolute,
locally configured `[java] custom-path-roots = ['C:/Java/approved']` allowlist and a
successful probe. The remote API cannot edit this allowlist. Detection IDs select
only a currently detected/probed executable. No shell command or arguments are
accepted. Owner + `content.install` is required for inventory and every mutation.

Selection returns a normal durable software job (`kind: java_select`) and uses the
software activation journal and stopped-game/supervisor hold. It updates the
protected launch TOML and the actual process owner's launch without starting
Minecraft. A running game refuses activation. The chosen path survives agent
restarts in protected launch/config state; failed/ambiguous activation follows
software recovery and rollback, never a blind retry. Future software plans read
the selected path, not the agent startup's original Java.

`POST /java/download {major}` returns a `java_download` job; poll existing
`GET /software/jobs`. It uses the official Adoptium latest GA HotSpot JRE API for
the agent OS/architecture, validates the archive SHA-256 from API metadata, stages
privately under state/managed-java, probes the requested major, then publishes a
single immutable temurin-MAJOR directory. Downloads never select or start Java.
Existing installs are reused after probing. HTTPS and each redirect are restricted
to Adoptium/GitHub release hosts; archives are bounded (256 MiB compressed, 1 GiB
expanded, 20,000 entries), reject traversal and unsupported special/link entries.
Unsupported archive layouts fail closed. Metadata/checksum provenance is retained.
API reference: https://github.com/adoptium/api.adoptium.net/blob/main/docs/cookbook.adoc

Software previews now report `java_compatible`, `selected_java_major`, and nullable
`managed_download_major` alongside the upstream `java` requirement. Incompatible
previews cannot be applied or approved. Download that major, select it after the
job succeeds, then re-plan. An unprobeable runtime remains an explicit error.

Native commands: server_java_inventory / server_java_select / server_java_download.
TS: native-java.ts ServerJavaRuntime / ServerJavaInventory, javaInventory / javaSelect /
javaDownload, and hosting-native.ts SoftwarePlan's compatibility fields. Fixtures
include Java 17/21/25, different vendors, allowlisted custom/rejected paths, a
stopped-only selection job, managed download, and Java-incompatible previews.

UI follow-up: show detected/managed choices and probe metadata, gate custom input
on customPathsAllowed, follow jobs, refresh inventory after success, and offer
Download Java N from incompatible plans. Disable Apply when javaCompatible=false.
