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

Backups are off unless `host.toml` has a `[backups]` table with exactly one repository,
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

## Jobs

Backups, verification and restores are durable jobs (`backup_jobs`, schema v19) run one at
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
- **Restore** goes only into a new directory `<restore-root>/<job id>`, optionally limited
  to validated relative paths. Paths are literal: pattern characters
  (`* ? [ ] \ { }`) are refused. In-place restore (stop, pre-restore safety snapshot, exact
  confirmation) is not implemented yet. Nothing ever restores automatically.
- Submissions accept an idempotency key: repeating it returns the same job. A job found
  `running` after an agent restart becomes `uncertain` (`backup_interrupted`), because the
  snapshot may or may not exist. The supervisor's save lease expires by itself.
- Agent shutdown waits for a running backup instead of abandoning it mid-write.
- Listing restore points uses a separate 60-second timeout. Only one listing runs at a time;
  others get `backup_busy`.
- If a job's final state cannot be written, the worker retries it with backoff. It never moves
  on, and never reruns the job.

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
- Pre-restore safety snapshot and in-place restore are missing.
- Filesystem-snapshot acceleration (VSS/Btrfs/ZFS/LVM) and the write guard are missing.
- Storage quotas and capacity preflight are missing.
- No player-count or update events, no script presets, no external notification delivery.
- Linux is untested.
