# Pack installation review follow-up

Optional server support is no longer reported as an unknown-side warning.
Previously the CurseForge adapter represented its absent side field as optional,
then install planning warned about every optional entry, including client-only
JARs it subsequently skipped. Explicit client-only JAR filtering still applies.
Provider-required dependency edges are checked against the pack inventory; a
missing dependency outside that inventory produces a readable review note.
The planner does not silently select additional arbitrary dependency versions.

Install can be clicked while preparation is running. It authorizes automatic
continuation only for a valid, compatible, unexpired, warning-free additive plan.
Conflicts, replacements, removals and warnings stop at review. File lists and
notes are expandable, with the existing change colors. Progress explicitly says
files are downloading and being verified; completion means actually staged.

The dialog refreshes software state on opening and replans on revision changes,
including changes occurring while an earlier request runs. Server comparisons
use the latest durable installation rather than runtime facts captured before
long downloads. Mismatch text shows the installed setup and the pack's pinned
loader version. A newer loader is still distinct from that exact requested pin.

A replan reuses payload references from the previous actor/device review after
rechecking the newly resolved provider digest. Missing or altered cache data
falls back to a normal verified download. Destinations, permissions, conflicts,
software state and review tokens are recomputed; mutable review decisions are
not cached.

Validation:
- Rust control library check passed.
- Frontend typecheck passed.
- Focused UI tests: 48 passed; a separate localization/pack run: 38 passed.
- Three explicit live provider tests passed, with server file hashes verified.
- CurseForge: first plan 10.802 s, cached replan 5.965 s (15 server files).
- Modrinth: first plan 12.901 s, cached replan 7.116 s (seven server files).
- Early-install policy tests cover runtime mismatches, expiry, warnings, removals
  and replacements. No Minecraft process was started.

Times are observations on this development machine, not latency guarantees.
Native visual confirmation remains unavailable in this session.

Deployment: the dev build completed at
C:/Users/eatha/AppData/Local/Temp/vesta-dev-builds/checkout-9baf1d08ed760a7d05ca949191f4b843f68813d0d55be7365b67eb7095423dd9/builds/541304ab-134f-4145-b647-050c985e7295.
The update command found no registered servers in the previously selected
C:/Users/eatha/AppData/Local/Temp/vesta-dev-server/servers registry. The old
instance directory remains, but `vesta list --json` returns an empty list.
No agent process was running at verification time. No registration was rebuilt
and no existing installation data was changed to work around that condition.
The backend fixes require deploying the new agent to the current registration.
