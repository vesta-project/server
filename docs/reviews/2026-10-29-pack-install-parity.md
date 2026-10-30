# Server pack import and launcher comparison

## Reproduction

CurseForge project `1714524`, file `9024763`, Echoes of the Sift Fabric,
Minecraft `26.3`, Fabric `0.19.5`.

The server first refused valid filenames containing `+` or an apostrophe. After
that was corrected, it refused the Sift file because its API `downloadUrl` was
null. The launcher already constructs the fixed CurseForge CDN path for this
case. A direct bounded download of file `9022781` returned 14,022,813 bytes and
matched the provider's SHA-1. The server now uses the same fallback.

## Compared behavior

| Area | Result |
| --- | --- |
| CurseForge resolution | Same CDN fallback as the launcher, with exact project/file identity and mandatory provider hash verification. Explicit unsafe URLs are rejected, not replaced with a fallback. |
| Publication and updates | Reuse that resolver policy, so a null API URL does not hide a downloadable version or make its client publication invalid. |
| Archive filenames | Portable ASCII mod names, internal spaces, brackets, parentheses, plus signs and apostrophes work. Traversal, backslashes, device aliases, edge spaces and ambiguous Unicode stay refused. |
| Modrinth side declarations | Server-only files are retained. Client-only files are excluded from the server review. Imported Vesta packs preserve these declarations too. |
| CurseForge side declarations | Provider metadata and bounded JAR inspection identify client-only files; unknown sides stay explicit review warnings. No software-specific player/log heuristics are involved. |
| Overrides | Server overrides take precedence over shared overrides. Client overrides stay out of server staging. Client publication uses shared plus client overrides. |
| Resource kinds | Resource packs and shaders stay out of server mod directories. Provider-identified datapacks are routed to the configured server world. |
| Local changes | Review records existing hashes, requires a replace/keep choice for conflicts, and leaves live files unchanged. Apply stages through the existing file journal and rechecks preconditions. |
| Loader/version | Pack review pins the required Minecraft version and loader/version. Server apply refuses a mismatched installed runtime. The client launcher installs its own game/runtime; server software changes continue through the existing separate Software workflow. |
| Errors | Fixed reasons identify metadata, download, checksum or source failure. Bounded public file identity survives HTTPS and native IPC. A typed upstream failure is distinct from an uncertain gateway response. |
| Latency | Metadata is reused within one bounded operation. Pack review has its own authenticated processing budget and install-worker admission, while lifecycle/status keep their ordinary capacity and deadlines. |

## Verification

The live review runs in an owned temporary database/game directory. It verifies
the Sift file is included, Iris and Sodium are skipped as client-only, the loader
is pinned, and no live files or server link change during review. A separate
fixture exercises config conflicts, override precedence, staging and idempotency.

Other scoped checks cover provider binding, hash failures, redirect allowlists,
path confinement, deadline expiry, cache bounds, error-envelope details, IPC field
validation, and a slow pinned-HTTPS review that exceeds the old 15-second timeout.
No modpack was applied to the user's dev server, and no Minecraft process was
started for these checks.

Verified results on Windows:

- 141 control pack/resource-pack tests passed, including the opt-in live Echoes
  review. The review selected 15 server files and skipped four client-only files.
- 35 content tests passed. The standalone live-key probe remained opt-in; the
  Echoes review exercised the configured key and actual verified downloads.
- 50 transport unit tests passed, including review processing deadlines and
  ordinary-worker availability under saturated install work.
- 34 native-client unit tests and 25 pinned-HTTPS tests passed. The HTTPS set
  includes a real 16-second response, typed 502 preservation and uncertain proxy
  failure handling.
- 93 frontend server/localization checks passed; desktop typecheck and scoped
  lint passed. The full desktop native `cargo check` passed with existing unused
  import/dead-code warnings. Protocol tests passed too.

## Scale constraints

This is download/import behavior parity, not unlimited pack-size parity. The
current server workspace still caps manifests at 256 entries, a staged install at
128 files / 64 MiB, and an individual verified file at 16 MiB. These limits suit
the current in-memory review design but will reject many large modpacks. Raising
them without changing storage would risk small hosts. Larger packs need a
separate disk-backed, content-addressed staging change, including resumable
downloads and bounded streaming verification. The new UI reports import limits
explicitly instead of asking users to correct form values.
