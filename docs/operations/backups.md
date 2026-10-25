# Backups and automation (foundation)

Status: implemented and tested on Windows with local repositories (2026-10-12). The
S3-compatible store has been tested only against a local fake server; no real S3 provider
has been tried. This page covers backups (F25–F27 foundation), the save lease (F26) and how
automation (F29/F30) drives them.

## Engine

Backups use Vesta's own Rust engine; no third-party backup software is bundled or run.

- Files are split into fixed 1 MiB chunks. Each chunk is stored once under its SHA-256
  hash and compressed with deflate. Unchanged chunks are never stored twice.
- A file whose size and modification time match the profile's previous snapshot is reused
  without being read again.
- Each snapshot is a compressed manifest listing directories, files and their chunk hashes.
  Restores re-hash every chunk and refuse a mismatch (`backup_repository_corrupt`).
  Manifest paths are revalidated, so a tampered manifest cannot write outside the target.
- **Repositories are not encrypted.** Anyone who can read the repository can read the
  world. Use an encrypted disk or a private bucket if that matters. This is an owner
  decision (2026-10-12); see the amendment in `SERVER_SOFTWARE_REQUIREMENTS.md`.
- Layout: `config.json`, `chunks/<2 hex>/<sha256>`, `snapshots/<id>` and `tags/<id>`.
  Local writes go to a temporary file and are then renamed into place. Objects are never
  modified after they are written, except tag files.
- Bounds: 1,000,000 files per snapshot, 10,000 snapshots, 64 MiB per stored object.
- Symbolic links, junctions and other special files in the game directory are skipped.

## Configuration

Backups can be configured remotely even when `[backups]` is absent. Until profiles are
saved, `backups_configured` is false and no backup runs. The default local repository is
`<instance base>/backups/repository`, with inspection restores in `backups/restores`.
Opening the agent or reading configuration does not initialize that repository.

A host may override this location using `[backups]`, with exactly one repository,
either a local directory:

```toml
[backups]
repository = "D:/vesta-backups/survival"   # outside the game and state directories
restore-root = "D:/vesta-restores"         # optional; enables inspection restores

[[backups.profile]]
name = "nightly"              # a-z, 0-9, '-'; up to 16 profiles
mode = "online"               # or "stopped"
keep-last = 7                 # retention; all zero = keep everything
keep-daily = 7
keep-weekly = 4
keep-monthly = 6
on-mutation = "fail-world"    # "warn" | "fail-world" (default) | "fail-any"
warn-seconds = 60             # in-game warning before an online backup (default 60, max 600)
max-snapshots = 30            # optional cap on this profile's restore points

[backups.limits]              # optional; see "Limits" below
max-snapshots = 100           # restore points across all profiles
max-repository-size = "50 GB" # B, KB, MB, GB, TB (powers of 1000) or KiB, MiB, GiB, TiB
min-free-space = "5 GiB"      # default 1 GiB; "0" turns the reserve off
```

or an S3-compatible bucket (AWS S3, MinIO, Backblaze B2, Cloudflare R2, …):

```toml
[backups]
restore-root = "D:/vesta-restores"

[backups.s3]
endpoint = "https://s3.us-east-1.amazonaws.com"
region = "us-east-1"
bucket = "my-vesta-backups"
prefix = "survival/"                  # optional
access-key-id = "AKIA..."
secret-key-file = "C:/vesta/private/s3-secret.key"   # protected from the file API
path-style = true                     # default true; false for virtual-hosted buckets
```

The S3 endpoint must use HTTPS. Plain HTTP is accepted only for a loopback address with
path-style addressing, for local testing. Redirects and proxies are not followed, and
requests that fail with a 5xx error are retried up to three times.

A local repository and the restore root may not overlap the game or state directories.
This is checked lexically at configuration load and again on their real locations
(following links and junctions) when the agent starts and before every restore.

Set up the repository once with `vesta-agent backup-init <name>`. It is idempotent and
prints `backup_repository_initialized`. There is no repository key to keep.

## Editing profiles and limits at runtime

`host.toml` gives the starting profiles, retention and limits. An owner or administrator can replace them
through the management API (`PUT backups/policy`). The change applies to the next job
without a restart.

- **Stored:** the edited policy is stored in the agent database (`backup_policy`, schema
  v21) and survives restarts. It takes over from the profiles and limits in `host.toml`
  until it is reset (`POST backups/policy/reset`).
- **Not editable:** the repository, S3 settings and restore root stay `host.toml`-only.
  Clients never choose paths or credentials.
- **Revision checks:** each save names the revision it replaces (`null` while `host.toml`
  applies). A stale save fails with `backup_policy_revision_conflict` instead of
  overwriting someone else's change.
- **Reserved authority:** editing needs `backups.configure`, reserved to Owner and
  Administrator (schema v26), never remotely delegable, because
  lowering retention removes restore points at the next backup.
- **Validation and audit:** the same rules as `host.toml` apply, and every save and reset
  is audited.
- **Removing a profile:** its queued jobs fail with `backup_profile_unknown`. Its existing
  restore points stay in the repository but are no longer listed. Adding the profile back
  under the same name lists them again.
- **Fail closed:** if the stored policy is ever invalid (for example, edited by hand), the
  backup service refuses to open rather than guessing.

## Launcher setup and profile editing

Use `GET /api/v3/manage/backups/config` to load an editor, including for a server with
no host backup table or profiles. Both GET and PUT require `backups.configure`.
The response contains the effective profiles, limits, source, revision, repository
kind/location/readiness, `backups_configured`, and `schedule_owner:"automation"`.
S3 credentials and endpoint details are never returned.

`PUT backups/config` accepts a complete replacement profile set and optional repository
initialization in one request:

```json
{
  "expected_revision": null,
  "initialize_repository": true,
  "profiles": [{
    "name": "daily", "mode": "stopped", "keep_last": 7,
    "keep_daily": 7, "keep_weekly": 4, "keep_monthly": 3,
    "include": [], "exclude": []
  }],
  "limits": {"min_free_bytes": 1073741824}
}
```

Add an entry to create a profile, replace it to edit/rename, or omit it to delete it.
Zero to sixteen profiles are accepted; `[]` disables backups without deleting snapshots
or repository data. `backups_configured` tracks the effective nonempty profile set
immediately, including after reset. Initialization alone can use an empty profile set.
Initialization does not take a backup, start/stop Minecraft, or install a schedule.

Names use lowercase ASCII letters, digits and hyphens (1–32 characters, no leading
hyphen). Each retention field is 0–1000; all zero keeps everything. Mode is `online`
or `stopped`; online still requires a running separately supervised game. Warnings
are 0–600 seconds. Defaults are `on_mutation:"fail-world"`, `warn_seconds:60`, no
snapshot/byte cap and a 1 GiB free-space reserve.

`include` defaults to `[]`, meaning the whole game directory. This includes worlds
with custom names and game configs, without guessing `level-name`. A nonempty list
selects literal relative files/directories, at most 64; directories include their
descendants. Absolute paths, traversal, aliases and wildcard roots are refused.
`exclude` defaults to `[]`: up to 64 single-line gitignore-subset patterns, 512 bytes
each, applied after `.vesta-backupignore`. Exclusion negations/comments are refused
in the API. Host profile TOML supports the same fields. In-place restore obeys the
current profile selection and existing protected paths. Inspection restores copy
the selected snapshot's content into a new directory.

Every PUT requires a normal `Idempotency-Key` and the current revision (null while
the host/default policy applies). Success is `{revision:integer}`; stale edits return
409 `backup_policy_revision_conflict`, changed requests under a reused key return
409 `backup_idempotency_conflict`. Receipts are bound to server, user, device, route
and key; current authority is checked before replay. Retries return the original
revision without rolling back newer changes. Receipts and audit are committed with
the policy; the shared receipt store is bounded to 4096 entries, with 90-day retention.
Every accepted change records `backups.config.update` or
`backups.repository.configure`; replays do not duplicate audit.

Repository initialization is an idempotent filesystem effect. A crash or database
failure after initialization may leave a valid empty repository; retry the same
request to commit the policy. Invalid/stale/forbidden requests never initialize it.
No arbitrary paths, credentials, restore roots or S3 URLs are accepted remotely.
Only the fixed instance default or the host-selected repository can be initialized.
Initialization rechecks real-location separation from game/state and refuses local
symlinks/junctions/reparse points in repository ancestors. Host administrators must
protect the instance and repository directories against concurrent local replacement;
the API is not a sandbox against a hostile local filesystem administrator. Storage
is unencrypted: world and config contents may contain secrets; secure the host/disk.

Schedules belong to Automation. Offer a link to create/edit an authorized workflow
with an interval or cron trigger and a backup step referencing the saved profile
name. This separately requires `automation.manage`; editing a profile does not
silently change workflows. Renaming/removing a profile leaves references intact,
and future steps fail with `backup_profile_unknown` until the workflow is updated.

## Limits

Limits stop new backups. They never delete restore points to make room.

- **`max-snapshots`** (per profile, and across all profiles in `[backups.limits]`) is
  checked against what would remain after the profile's retention. A profile with
  `keep-last = 7` and `max-snapshots = 7` therefore keeps working, because retention
  removes the oldest each time. A profile that keeps everything stops at its cap. Safety
  snapshots don't count.
- **`max-repository-size`** is checked before a backup starts and enforced while it
  writes. A backup that would pass it stops partway. The data it already stored is not a
  restore point and is cleaned up by a later prune. Prune only runs after a successful
  backup or explicit maintenance, so the repository can sit slightly above the limit until
  then.
- **`min-free-space`** is kept free on a local repository's disk (not checked for S3) and,
  during an in-place restore, on the game disk. If free space can't be measured, the job is
  refused.
- When a limit stops a backup, the job fails with `backup_snapshot_limit_reached`,
  `backup_storage_limit_reached` or `backup_disk_space_low`. The usual backup-failed
  event fires, so a workflow can notify about it. These checks run before any in-game
  warning or save suspension.
- `GET backups/storage` reports repository size, restore-point and safety-snapshot
  counts, free space, the configured limits, and `backups_stopped` (the limit code from
  the most recent backup, if it was refused). Backups resume once the owner raises a limit,
  tightens retention or frees space.
- Safety snapshots taken before a restore ignore the count and size limits, since they are
  capped at five per profile, so a full repository never blocks a restore. They still keep
  the free-space reserve.
- Sizes are approximate budgets, not exact guarantees. Usage counts every stored object.

## Jobs

Backups, verification and restores are durable jobs (`backup_jobs`, schema v20) run one at
a time by the agent. Each job records the user and device that submitted it. That subject must
hold `backups.create`, `backups.verify` or `backups.restore` when submitting, and again
when the worker starts the job. A job whose submitter lost that authority while queued fails with
`backup_authority_revoked`. At most 32 jobs may be queued or running. Finished jobs are kept
for 90 days, up to 1,000.

- **Backup, online** (needs `[process] supervision = "separate"` and a running game):
  1. Optional in-game `say` warning.
  2. The supervisor takes a **save lease** (below).
  3. The lease is renewed every 2 minutes across both mutation scans and the read.
  4. The lease is released.

  The world is save-consistent. If any renewal fails, consistency is reported as `unproven`
  and the backup fails (`backup_consistency_lost`). Logs and plugin data that keep writing are
  best-effort.
- **Backup, stopped**:
  - The agent's start gate is taken first. It is atomic with any start already in progress,
    so the backup either sees that game or the start is refused. Every later start (operations,
    local CLI, restart continuation) is refused with `maintenance_active`.
  - With a separate supervisor, a supervisor **maintenance hold** is also taken. It refuses
    starts and keeps crash restarts pending.
  - The backup then needs affirmative evidence the game is stopped: no generation, no orphan,
    no unresolved launch, and healthy observation. Otherwise it refuses with
    `backup_requires_stopped_server`.
- During any backup, Vesta-managed file changes (file API, uploads, content installs) wait on
  the shared file mutation lock. Independent writes cannot be blocked. A metadata scan
  before and after the read finds them: paths under a directory with `level.dat` are world
  data. The scan is bounded: 500,000 entries, 10 minutes. An incomplete scan counts as a
  possible world change. `fail-world` fails the backup when world data changed, `fail-any`
  fails on any change, and `warn` only reports. Results list up to 50 changed paths.
- Snapshots are written as profile **candidates**. Only after all checks pass is a snapshot
  tagged into its profile. Rejected snapshots are tagged `rejected`, are never listed as
  restore points and never count toward retention.
- The file lock, the lease or hold and the start gate are released as soon as reading
  finishes. Tagging, retention and pruning run afterwards, holding none of them.
- `.vesta-backupignore` in the game root (max 64 KiB, 1,000 patterns) uses a gitignore
  subset: `#` comments, `!` re-includes, a trailing `/` for directories only, a `/`
  inside the pattern anchors it to the game root, `*` and `?` within a name, `**` across
  directories. Character classes are not supported.
- Retention runs per profile after a successful backup. It keeps the newest N snapshots
  plus the newest one in each of the last N days, ISO weeks and months (UTC). Pruning then
  deletes unreferenced chunks older than one hour, so a backup running at the same time is
  never cut short. It also deletes unpromoted candidates older than one day.
- **Verify** checks that every snapshot manifest decodes and every referenced chunk exists.
  It does not re-read chunk data; restores do.
- **Inspection restore** goes only into a new directory `<restore-root>/<job id>`,
  optionally limited to validated relative paths. Paths are literal: pattern characters
  (`* ? [ ] \ { }`) are refused. Nothing ever restores automatically.
- **In-place restore** is described in its own section below.
- Submissions accept an idempotency key: repeating it returns the same job. A job found
  `running` after an agent restart becomes `uncertain` (`backup_interrupted`), because the
  snapshot may or may not exist. The supervisor's save lease expires by itself.
- Agent shutdown waits for a running backup instead of abandoning it mid-write.
- Listing restore points uses a separate 60-second timeout. Only one listing runs at a time;
  others get `backup_busy`.
- If a job's final state cannot be written, the worker retries it with backoff. It never moves
  on, and never reruns the job.

## In-place restore

An in-place restore makes the game directory, or only some paths in it, match a snapshot.

1. **Prepare.** The request names the profile, the snapshot and optional paths. It is stored
   as `awaiting_confirmation` and returns a one-time confirmation token valid for two
   minutes. Nothing is changed and nothing is queued yet.
2. **Confirm.** Only the same user and device can confirm, once, before the token expires.
   A wrong token cancels the request (`backup_confirmation_invalid`). Authority
   (`backups.restore`) is checked when preparing, when confirming and again when the job
   starts. Prepare, confirm and resolve are audited.
3. **Checks before any change.** The job fails without touching anything when:
   - the snapshot is not one of the profile's accepted restore points or safety snapshots
     (`backup_snapshot_not_restorable`, `backup_snapshot_unknown`); candidate and rejected
     snapshots are never restorable.
   - a requested path is not in the snapshot (`backup_restore_path_not_in_snapshot`).
   - **the game is running.** Vesta does not stop it for you. Stop the server first. The job
     takes the start gate, the supervisor hold and the file mutation lock, then needs the
     same affirmative stopped facts as a stopped backup (`backup_requires_stopped_server`).
   - a supervisor crash restart is pending, or a launch is unresolved
     (`backup_restore_restart_pending`).
   - a referenced chunk is missing, the manifest is malformed, or the target holds a
     symbolic link, junction or special file in the restored area
     (`backup_repository_corrupt`, `backup_restore_target_unsafe`).
4. **Safety snapshot.** Unless the request explicitly sets `skip_safety_snapshot`, the
   current directory is backed up first (same ignore rules), tagged as a safety snapshot of
   the profile. Safety snapshots are listed separately. They are not profile restore points
   and do not count toward profile retention. The newest five per profile are kept, and
   restoring one undoes the restore.
5. **Apply.** Inside the restored area, files and directories that are not in the snapshot
   are removed. Snapshot files are written to a temporary file next to the target, checked
   against their hashes, then renamed into place. Files whose content already matches are
   left alone. Paths excluded by `.vesta-backupignore` and protected paths (configuration,
   credentials and other files Vesta protects from the file API) are never written or
   removed.

**Interruption.** Each file is replaced atomically, but the directory as a whole is not. The
job is durably marked as applying before the first change. If it fails after changing
anything, or the agent dies meanwhile, the job becomes `recovery_required`
(`backup_restore_interrupted` after a crash). While any restore needs recovery, every game
start is refused, also after the agent restarts. It clears when a later **full** in-place
restore succeeds, for example of the same snapshot or of the safety snapshot, or when a user
with `backups.restore` explicitly resolves it and accepts the current files. Nothing is ever
rolled back or restored automatically, and automation workflows cannot restore.

## Save lease (supervisor)

- **Taking the lease:** `save_suspend` durably records a lease before sending `save-off`.
  The lease is tied to one exact game generation and carries a random token.
- **Durability:** the record uses two alternating files with sequence numbers. A renewal
  only ever overwrites the older one, so a failed write keeps the previous lease.
- **Acknowledgements:** the supervisor waits for the server's reply to `save-off`, then
  sends `save-all flush` and waits for "Saved the game", read from after the `save-off`
  reply. Acknowledgements are taken only from real server log lines, never chat.
- **The save window:** while it is open, other save commands sent through the console API
  are refused. A generation change, console gap or lease expiry aborts the waits.
- **Owner's autosave choice:** the reply to `save-off` reveals whether autosave was already
  off. If it was, nothing ever turns it back on.
- **No acknowledgement:** the request is refused, but the supervisor keeps an **unknown**
  obligation. It never sends `save-on` for it, refuses new leases and reports
  `save_state_unknown` until that game exits.
- **Renew and release:** both need the lease token. When the lease is released or expires
  (10 s–30 min, renewable), the supervisor sends `save-on` to that same generation only if
  autosave was on before, with or without an agent. The lease stays in an "ending" state
  until the write is actually delivered, with retries, and only then is restoration reported.
- **Other generations:** a different or exited generation is never sent anything.
- **Supervisor loss:** if the supervisor itself dies, its replacement reports the lease as
  **stranded** while that game runs, because nothing holds its stdin. The lease clears once
  the game exits; a restarted Minecraft starts with autosave on. An idle supervisor holding a
  lease refuses to shut down.
- **Limitation:** Minecraft logs carry no command IDs. A save command typed in-game or sent by
  a plugin, or a forged server INFO line, can still interfere with acknowledgement matching.

## Maintenance hold (supervisor)

A stopped backup takes a token-fenced, expiring hold. While held, the supervisor refuses
starts and keeps any pending crash restart pending. A hold is refused if a game, orphan or
unresolved launch exists, so a granted hold proves the game is stopped. Holds are in-memory:
they end if the supervisor exits, and a new supervisor never starts the game by itself.

## Automation

Workflows (F29) and the notification inbox (F30) are durable in schema v18. Triggers are
manual, cron (5-field with an explicit UTC offset; daylight saving is not handled), interval,
agent start, server started/stopped/crashed, backup completed/failed. Player-count and
update triggers exist in the model but nothing publishes them yet. Steps cover backup,
verify backup, start, stop, restart, console command, clean expired trash, delay and
notify. Each step has retries, a timeout and forward-only success/failure branches. Missed
schedules default to skip-and-notify.

Each workflow acts as its execution principal through a keyless **"Vesta automation"
device**, created on first use. It cannot sign in, it appears in that user's device list,
and revoking it, or reducing that user's roles, removes the schedules' authority at their next step.
Every mutating step is re-authorized against current policy and runs through the normal
idempotent paths: game actions are ordinary operations, backups are backup jobs. Script
presets are not implemented (`unsupported`). The agent's automation driver ticks
schedules every second and publishes server and backup events. It never acts on its own.

## Not done yet

- Real S3 providers are untested; only a local fake S3 server is.
- Two agents writing to one repository are not coordinated. Give each server its own
  repository or prefix.
- In-place restore does not stop the game itself.
- Filesystem-snapshot acceleration (VSS/Btrfs/ZFS/LVM) and the write guard are missing.
- No player-count or update events, no script presets, no external notification delivery.
- Linux is untested.
