# Server modpack capacity changes

The former 128 staged files, 64 MiB total review payload and 16 MiB downloaded
file limits could reject ordinary packs. Server installation now streams verified
payloads to private disk files and keeps only digest/size references in review
JSON and pending effects. Available storage, including temporary replacement
copies and a 64 MiB reserve, controls admission.

Archive upload appends no longer concatenate a growing SQLite BLOB. Durable
received offsets allow retry after an unacknowledged filesystem tail. Final
length/hash checks run outside the database writer. Native dropped archives
stream from opened handles; authenticated chunks accept 512 KiB. Metadata remains
bounded to 65,536 entries / 32 MiB JSON, and archive uploads to 128 GiB. Those
technical bounds should be well beyond normal modpacks. Status/urgent worker
budgets and anonymous body limits remain separate.

Apply consumes every pending batch, retains canonical authorization and the file
journal, checks payload identity before replacing anything, and streams restore
copies. Pack receipts no longer consume the ordinary interactive file quota.
New review tokens supersede old reviews from the same actor/device. Payload
collection retains active reviews, uploads and pending effects and removes
unreferenced private payloads when no worker holds a lease.

Migration 29 preserves pre-existing workspace and upload rows while lifting their
old database constraints. Legacy stored reviews and uploads remain readable.
Custom client publication is separate and still has retained-byte budgets. Large
server installation does not require successfully building that optional client
projection. Existing trash retention quotas remain configured independently.

Validation so far:

- Pack/resource-pack backend suite: 150 passed, three live-provider tests excluded
  by default. Includes 70 MiB archive / 301-file upload-plan-apply with no SQL
  payload BLOBs, 1,100-mod inventory, retry recovery, tamper refusal and receipt
  quota isolation.
- Schema migration fixtures: two passed, including unfinished v28 upload recovery.
- Launcher client unit/management/scheduler/priority: 36 + 19 + 9 + 1 passed.
- Frontend: typecheck and 44 scoped pack tests passed.
- Native launcher cargo check passed; existing unrelated warnings remain.
- Strict server control/transport library Clippy passed.
- Broader file suite: 151 passed initially; Windows restore cleanup regression
  found and fixed by closing the source handle before removing its trash copy.
  A gzip corruption test returned file_unavailable under parallel contention and
  passed alone without a code change. Follow-up results are recorded below.

No real Minecraft process was started. Native visual testing remains unavailable
because this environment has no working computer-use native pipe.

Final follow-up results:

- Live provider tests: all three passed. CurseForge Echoes of the Sift installed
  and rehashed 15 server files; Modrinth Pure Vanilla Server installed and rehashed
  seven. The additional CurseForge planning-only test also passed.
- HTTPS: 12 existing selected tests passed; the new authenticated 512 KiB chunk
  test passed after correcting its anonymous expectation to the earlier 401
  rejection. Anonymous bodies are rejected before body I/O. Two transport
  scheduling/deadline unit tests passed.
- Catalogue protocol: all 23 tests passed. Aggregate-budget fixtures now derive
  their input counts from the expanded metadata budget.
- The corrected Windows restore test passed explicitly. The parallel gzip
  corruption failure passed in isolation; it remains a parallel contention caveat.
- Dev build succeeded and dev-server was updated to PID 51516. Minecraft remains
  stopped. The normal migration backup precedes schema 29 activation.
- Free space after validation: approximately 74 GiB on C and 39 GiB on V. Builds
  reused existing targets; no broad cleanup or user-content deletion was performed.

New native launcher upload behavior requires running a rebuilt launcher, in
addition to the updated agent. The native launcher check verifies compilation,
not that the user's already-open launcher process has reloaded its Rust binary.
