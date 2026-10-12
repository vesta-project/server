//! Backups (F25–F27 foundation): durable jobs, built-in engine, save-lease coordination.
//!
//! - Jobs (backup, verify, inspection restore) are durable rows processed one at a time
//!   by an owned worker. Each job records the subject (principal and device) that
//!   submitted it; current authority is rechecked when the worker starts it. A job
//!   found `running` after an agent restart is marked `uncertain`.
//! - Online backups need the separate supervisor: a bounded save lease (`save-off`,
//!   acknowledged `save-all flush`) covers the whole read, including both mutation
//!   scans, and is renewed meanwhile; a failed renewal makes consistency unproven.
//! - Stopped backups need affirmative evidence that the game is stopped. Every start is
//!   blocked first: the agent's start gate (atomic with in-flight starts) and, with a
//!   separate supervisor, a supervisor maintenance hold that also suppresses crash
//!   restarts.
//! - While a backup reads, Vesta-managed file mutations wait on the shared file
//!   mutation admission lock. A bounded metadata scan before and after detects
//!   independent writes; incomplete scans fail closed; the profile's policy decides.
//! - Snapshots are created as candidates and only tagged into their profile after the
//!   checks pass, so rejected snapshots are never restore points or retention input.
//!   Consistency resources are released before repository maintenance (prune).
//! - Inspection restores go only to a new directory under the configured restore root,
//!   whose real location is checked against the game and state directories. In-place
//!   restores (see `restore`) need exact confirmation and an already stopped game.
//!   Nothing here ever restores automatically, stops or kills the game.
pub mod engine;
pub mod ignore;
mod limits;
mod restore;
pub mod s3;
pub mod space;
pub mod store;

pub use limits::{DISK_SPACE_LOW, SNAPSHOT_LIMIT, STORAGE_LIMIT, StorageStatus};
pub use restore::{PreparedRestore, recovery_jobs, resolve_recovery_locally};

use crate::{
    config::{BackupMode, BackupProfile, BackupRepository, BackupSettings, MutationPolicy},
    identity::{Capability, DeviceId, PrincipalId, Subject},
    operations::StartGate,
};
use engine::{Engine, EngineError, Retention};
use ignore::Ignore;
use rusqlite::{OptionalExtension, params};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant, UNIX_EPOCH},
};
use store::{LocalStore, Store};
use vesta_game_runtime::process::GenerationId;
use vesta_state::{StateDatabase, StateDirectory};

pub const IGNORE_FILE: &str = ".vesta-backupignore";
const MAX_IGNORE_BYTES: u64 = 64 * 1024;
const MAX_SCAN_ENTRIES: usize = 500_000;
const MAX_SCAN_PENDING: usize = 100_000;
const MAX_SCAN_TIME: Duration = Duration::from_secs(600);
const LEASE: Duration = Duration::from_secs(600);
const RENEW_EVERY: Duration = Duration::from_secs(120);
const MAX_RESULT_JSON: usize = 16 * 1024;
const MAX_REPORTED_CHANGES: usize = 50;
pub const MAX_ACTIVE_JOBS: i64 = 32;
const MAX_FINISHED_JOBS: i64 = 1000;
const FINISHED_RETENTION_MS: i64 = 90 * 24 * 60 * 60 * 1000;
const CANDIDATE: &str = "candidate";
const REJECTED: &str = "rejected";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackupError {
    Disabled,
    UnknownProfile,
    UnknownJob,
    Invalid,
    Busy,
    Unavailable,
    State,
    /// The idempotency key was already used for a different request.
    IdempotencyConflict,
    /// The subject does not currently hold the job's capability.
    Forbidden,
    /// The confirmation is unknown, expired, already used or not the subject's.
    ConfirmationInvalid,
}
impl BackupError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Disabled => "backups_not_configured",
            Self::UnknownProfile => "backup_profile_unknown",
            Self::UnknownJob => "backup_job_unknown",
            Self::Invalid => "backup_invalid_request",
            Self::Busy => "backup_busy",
            Self::Unavailable => "backup_unavailable",
            Self::State => "backup_state_unavailable",
            Self::IdempotencyConflict => "backup_idempotency_conflict",
            Self::Forbidden => "backup_forbidden",
            Self::ConfirmationInvalid => "backup_confirmation_invalid",
        }
    }
}
impl From<vesta_state::StateError> for BackupError {
    fn from(_: vesta_state::StateError) -> Self {
        Self::State
    }
}
impl From<rusqlite::Error> for BackupError {
    fn from(_: rusqlite::Error) -> Self {
        Self::State
    }
}
pub type Result<T> = std::result::Result<T, BackupError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobKind {
    Backup,
    Verify,
    /// Restore `snapshot` (optionally only `paths`) into a new inspection directory.
    Restore {
        snapshot: String,
        paths: Vec<String>,
    },
}
impl JobKind {
    fn wire(&self) -> &'static str {
        match self {
            Self::Backup => "backup",
            Self::Verify => "verify",
            Self::Restore { .. } => "restore",
        }
    }
}

/// The capability a job needs, both at submission and again when it starts.
fn capability(kind: &str) -> Option<Capability> {
    Some(match kind {
        "backup" => Capability::BackupsCreate,
        "verify" => Capability::BackupsVerify,
        "restore" | "restore_in_place" => Capability::BackupsRestore,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobState {
    /// An in-place restore waiting for its exact confirmation.
    AwaitingConfirmation,
    Queued,
    Running,
    Succeeded,
    Failed,
    Uncertain,
    /// An in-place restore stopped after changing the game directory; starts are
    /// blocked until it is resolved.
    RecoveryRequired,
}
impl JobState {
    fn wire(self) -> &'static str {
        match self {
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Uncertain => "uncertain",
            Self::RecoveryRequired => "recovery_required",
        }
    }
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "uncertain" => Self::Uncertain,
            "awaiting_confirmation" => Self::AwaitingConfirmation,
            "recovery_required" => Self::RecoveryRequired,
            _ => return None,
        })
    }
    pub fn finished(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Uncertain | Self::RecoveryRequired
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: String,
    pub profile: String,
    pub kind: String,
    pub state: JobState,
    pub requested_by: String,
    pub snapshot_id: Option<String>,
    pub result: Option<serde_json::Value>,
    pub error_code: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}
impl Job {
    pub fn state_wire(&self) -> &'static str {
        self.state.wire()
    }
}

/// Finished-job notifications for automation triggers and audit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackupEvent {
    Completed { profile: String, job: String },
    Failed { profile: String, job: String },
}

/// Facts the service needs from the rest of the host, supplied by composition.
pub struct Environment {
    /// Affirmative lifecycle facts: `Ok(None)` only when the game is known stopped
    /// (no generation, no orphan, no unresolved launch, observation healthy).
    pub observe:
        Box<dyn Fn() -> std::result::Result<Option<GenerationId>, &'static str> + Send + Sync>,
    /// Present only in separate-supervision mode; online backups require it.
    pub supervisor: Option<vesta_supervisor::SupervisorClient>,
    /// The runtime's start gate, blocked while a stopped backup reads.
    pub start_gate: StartGate,
    pub events: Box<dyn Fn(BackupEvent) + Send + Sync>,
    /// Paths an in-place restore never writes or removes (secrets, Vesta's own
    /// configuration); only those inside the game directory matter.
    pub protected: Vec<PathBuf>,
}

struct Inner {
    db: StateDatabase,
    settings: BackupSettings,
    engine: Engine,
    /// Short-timeout engine for interactive observation (snapshot listing).
    listing: Mutex<()>,
    game: PathBuf,
    env: Environment,
    stopping: AtomicBool,
    wake: (Mutex<bool>, Condvar),
    /// Held while an interrupted in-place restore needs recovery.
    recovery: Mutex<Option<crate::operations::StartBlock>>,
    /// `env.protected` relative to the game directory.
    protected: Vec<String>,
}

pub struct Backups {
    inner: Arc<Inner>,
    worker: Option<JoinHandle<()>>,
}
impl std::fmt::Debug for Backups {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Backups")
    }
}

/// Open the configured repository storage.
pub fn open_store(settings: &BackupSettings) -> Result<Box<dyn Store>> {
    match &settings.repository {
        BackupRepository::Local(path) => Ok(Box::new(LocalStore::new(path.clone()))),
        BackupRepository::S3(s3) => open_s3(s3),
    }
}

/// Build the S3 store; the secret key comes from its own small protected file.
fn open_s3(repository: &crate::config::S3Repository) -> Result<Box<dyn Store>> {
    let secret = crate::files::read_local_document(&repository.secret_key_file, 1024)
        .map_err(|_| BackupError::Unavailable)?;
    let secret = String::from_utf8(secret)
        .map_err(|_| BackupError::Invalid)?
        .trim()
        .to_owned();
    if secret.is_empty() {
        return Err(BackupError::Invalid);
    }
    let endpoint = url::Url::parse(&repository.endpoint).map_err(|_| BackupError::Invalid)?;
    let store = s3::S3Store::new(s3::S3Config {
        endpoint,
        region: repository.region.clone(),
        bucket: repository.bucket.clone(),
        prefix: repository.prefix.clone(),
        access_key_id: repository.access_key_id.clone(),
        secret_access_key: secret,
        path_style: repository.path_style,
    })
    .map_err(|_| BackupError::Invalid)?;
    Ok(Box::new(store))
}

/// The real location of `path`: the nearest existing ancestor resolved natively (links
/// and junctions followed) plus the remaining, not-yet-existing components.
fn real_location(path: &Path) -> Option<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = fs::canonicalize(&existing) {
            let mut real = real;
            for part in rest.iter().rev() {
                real.push(part);
            }
            return Some(real);
        }
        let name = existing.file_name()?.to_owned();
        rest.push(name);
        if !existing.pop() {
            return None;
        }
    }
}

fn components(path: &Path) -> Vec<String> {
    path.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| {
            let text = c.as_os_str().to_string_lossy().into_owned();
            if cfg!(windows) {
                text.to_lowercase()
            } else {
                text
            }
        })
        .collect()
}

/// Whether two real locations are the same or nested in either direction.
fn overlaps(left: &Path, right: &Path) -> bool {
    let (l, r) = (components(left), components(right));
    let n = l.len().min(r.len());
    l[..n] == r[..n]
}

/// Refuse a repository or restore root that really lies inside (or contains) the game
/// or state directory, after following links and junctions.
fn check_locations(settings: &BackupSettings, game: &Path, state: &Path) -> Result<()> {
    let game = real_location(game).ok_or(BackupError::Invalid)?;
    let state = real_location(state).ok_or(BackupError::Invalid)?;
    let local = match &settings.repository {
        BackupRepository::Local(path) => Some(path),
        BackupRepository::S3(_) => None,
    };
    for path in local.into_iter().chain(settings.restore_root.as_ref()) {
        let real = real_location(path).ok_or(BackupError::Invalid)?;
        if overlaps(&real, &game) || overlaps(&real, &state) {
            return Err(BackupError::Invalid);
        }
    }
    Ok(())
}

/// Local owner setup without a running agent: create the repository (an existing
/// one is accepted unchanged).
pub fn initialize_repository(
    state: &StateDirectory,
    settings: &BackupSettings,
    game: &Path,
) -> std::result::Result<(), &'static str> {
    state.prepare().map_err(|_| BackupError::State.code())?;
    check_locations(settings, game, state.path()).map_err(BackupError::code)?;
    Engine::new(open_store(settings).map_err(BackupError::code)?)
        .init()
        .map_err(EngineError::code)
}

impl Backups {
    /// Open the service and recover interrupted jobs. Does not run anything yet.
    pub fn open(
        db: StateDatabase,
        settings: BackupSettings,
        game: &Path,
        env: Environment,
    ) -> Result<Self> {
        let store = open_store(&settings)?;
        Self::open_with_store(db, settings, store, game, env)
    }

    /// As `open`, with an explicitly supplied repository store.
    pub fn open_with_store(
        db: StateDatabase,
        settings: BackupSettings,
        store: Box<dyn Store>,
        game: &Path,
        env: Environment,
    ) -> Result<Self> {
        check_locations(&settings, game, db.directory().path())?;
        let engine = Engine::new(store);
        let now = db.clock().millis();
        db.write(|c| {
            // Agent died while an in-place restore was changing the game directory.
            c.execute(
                "UPDATE backup_jobs SET state='recovery_required',error_code='backup_restore_interrupted',finished_at=? WHERE state='running' AND kind='restore_in_place' AND applying=1",
                params![now],
            )?;
            // Agent died mid-job: the snapshot may or may not exist.
            c.execute(
                "UPDATE backup_jobs SET state='uncertain',error_code='backup_interrupted',finished_at=? WHERE state='running'",
                params![now],
            )?;
            // Confirmations do not survive an agent restart.
            restore::expire_confirmations(c, now, i64::MAX)?;
            Ok(())
        })?;
        let protected = restore::protected_inside(game, &env.protected);
        let inner = Arc::new(Inner {
            db,
            settings,
            engine,
            listing: Mutex::new(()),
            game: game.to_owned(),
            env,
            stopping: AtomicBool::new(false),
            wake: (Mutex::new(false), Condvar::new()),
            recovery: Mutex::new(None),
            protected,
        });
        // Before the worker or any caller can start the game.
        inner.refresh_recovery();
        let worker = {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name("vesta-backups".into())
                .spawn(move || inner.work())
                .map_err(|_| BackupError::Unavailable)?
        };
        Ok(Self {
            inner,
            worker: Some(worker),
        })
    }

    pub fn settings(&self) -> &BackupSettings {
        &self.inner.settings
    }

    /// Create the repository; an existing valid repository is accepted unchanged.
    pub fn init_repository(&self) -> std::result::Result<(), EngineError> {
        self.inner.engine.init()
    }

    /// Queue a job for `subject`, who must currently hold the job's capability; it is
    /// checked again when the job starts. An existing job with the same idempotency key
    /// and the same request is returned; a different request is a conflict.
    pub fn submit(
        &self,
        subject: &Subject,
        profile: &str,
        kind: JobKind,
        idempotency_key: Option<&str>,
    ) -> Result<String> {
        if self.inner.settings.profile(profile).is_none() {
            return Err(BackupError::UnknownProfile);
        }
        if idempotency_key.is_some_and(|k| k.is_empty() || k.len() > 200) {
            return Err(BackupError::Invalid);
        }
        let target = match &kind {
            JobKind::Backup | JobKind::Verify => None,
            JobKind::Restore { snapshot, paths } => {
                if self.inner.settings.restore_root.is_none() {
                    return Err(BackupError::Disabled);
                }
                if !engine::valid_snapshot_id(snapshot)
                    || paths.len() > 64
                    || paths.iter().any(|p| {
                        crate::files::RelativePath::parse(p).is_err() || !engine::literal_path(p)
                    })
                {
                    return Err(BackupError::Invalid);
                }
                Some(serde_json::json!({"snapshot":snapshot,"paths":paths}).to_string())
            }
        };
        let kind_text = kind.wire();
        let required = capability(kind_text).ok_or(BackupError::Invalid)?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = self.inner.db.clock().millis();
        let principal = subject.principal.as_str();
        let device = subject.device.as_str();
        let id = self.inner.db.write(|c| {
            if !allowed(c, &self.inner.db, subject, required)? {
                return Ok(Err(BackupError::Forbidden));
            }
            if let Some(key) = idempotency_key
                && let Some((existing, same)) = c
                    .query_row(
                        "SELECT id, profile=? AND kind=? AND target IS ? AND requested_by=? AND requested_device=? FROM backup_jobs WHERE idempotency_key=?",
                        params![profile, kind_text, target, principal, device, key],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
                    )
                    .optional()?
            {
                return Ok(if same {
                    Ok(existing)
                } else {
                    Err(BackupError::IdempotencyConflict)
                });
            }
            let active: i64 = c.query_row(
                "SELECT count(*) FROM backup_jobs WHERE state IN ('awaiting_confirmation','queued','running')",
                [],
                |r| r.get(0),
            )?;
            if active >= MAX_ACTIVE_JOBS {
                return Ok(Err(BackupError::Busy));
            }
            c.execute(
                "INSERT INTO backup_jobs (id,profile,kind,state,requested_by,requested_device,idempotency_key,target,created_at) VALUES (?,?,?,'queued',?,?,?,?,?)",
                params![id, profile, kind_text, principal, device, idempotency_key, target, now],
            )?;
            Ok(Ok(id))
        })??;
        self.inner.notify();
        Ok(id)
    }

    pub fn job(&self, id: &str) -> Result<Option<Job>> {
        Ok(self.inner.db.read(|c| {
            c.query_row(&format!("{JOB_COLUMNS} WHERE id=?"), params![id], job_row)
                .optional()
                .map_err(Into::into)
        })?)
    }

    /// Newest first. `before` is an exclusive `(created_at, id)` cursor.
    pub fn jobs(&self, before: Option<(i64, &str)>, limit: usize) -> Result<Vec<Job>> {
        let limit = limit.clamp(1, 200) as i64;
        let (at, id) = before.unwrap_or((i64::MAX, ""));
        Ok(self.inner.db.read(|c| {
            let mut s = c.prepare(&format!(
                "{JOB_COLUMNS} WHERE created_at < ?1 OR (created_at = ?1 AND ?2 <> '' AND id < ?2) ORDER BY created_at DESC, id DESC LIMIT ?3"
            ))?;
            s.query_map(params![at, id, limit], job_row)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })?)
    }

    /// Validated restore points for one profile (candidates and rejected snapshots are
    /// excluded). Bounded by a short timeout; one listing at a time, otherwise `Busy`.
    pub fn snapshots(&self, profile: &str) -> Result<Vec<engine::Snapshot>> {
        if self.inner.settings.profile(profile).is_none() {
            return Err(BackupError::UnknownProfile);
        }
        let _single = self
            .inner
            .listing
            .try_lock()
            .map_err(|_| BackupError::Busy)?;
        self.inner
            .engine
            .snapshots(&profile_tag(profile))
            .map_err(|error| match error {
                EngineError::Unavailable => BackupError::Busy,
                _ => BackupError::Unavailable,
            })
    }

    /// Stop claiming queued jobs (they stay queued for the next start). A job already
    /// running continues; `busy` reports it until it finishes.
    pub fn stop_claiming(&self) {
        self.inner.stopping.store(true, Ordering::Release);
        self.inner.notify();
    }

    /// Whether the worker is still running (a job may be in progress).
    pub fn busy(&self) -> bool {
        self.worker.as_ref().is_some_and(|w| !w.is_finished())
    }
}
impl Drop for Backups {
    fn drop(&mut self) {
        self.inner.stopping.store(true, Ordering::Release);
        self.inner.notify();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Current policy for `subject` in this transaction.
fn allowed(
    c: &rusqlite::Connection,
    db: &StateDatabase,
    subject: &Subject,
    capability: Capability,
) -> std::result::Result<bool, vesta_state::StateError> {
    let policy = crate::identity::policy_in(c, db.server_id(), db.clock().millis(), Some(subject))
        .map_err(|_| vesta_state::StateError::new("state_policy_unavailable"))?;
    Ok(matches!(
        policy.check(capability, None),
        crate::identity::Decision::Allow
    ))
}

const JOB_COLUMNS: &str = "SELECT id,profile,kind,state,requested_by,snapshot_id,result_json,error_code,created_at,started_at,finished_at FROM backup_jobs";

fn job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let state: String = r.get(3)?;
    let result: Option<String> = r.get(6)?;
    Ok(Job {
        id: r.get(0)?,
        profile: r.get(1)?,
        kind: r.get(2)?,
        state: JobState::parse(&state).unwrap_or(JobState::Uncertain),
        requested_by: r.get(4)?,
        snapshot_id: r.get(5)?,
        result: result.and_then(|text| serde_json::from_str(&text).ok()),
        error_code: r.get(7)?,
        created_at: r.get(8)?,
        started_at: r.get(9)?,
        finished_at: r.get(10)?,
    })
}

fn profile_tag(profile: &str) -> String {
    format!("profile={profile}")
}

struct Claimed {
    id: String,
    profile: String,
    kind: String,
    target: Option<String>,
    principal: String,
    device: String,
}

struct Outcome {
    state: JobState,
    snapshot: Option<String>,
    result: serde_json::Value,
    error: Option<&'static str>,
}
impl Outcome {
    fn failed(code: &'static str) -> Self {
        Self {
            state: JobState::Failed,
            snapshot: None,
            result: serde_json::Value::Null,
            error: Some(code),
        }
    }
}

/// Lease renewal across the whole consistency interval, from a bounded helper thread.
struct Renewer {
    done: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Renewer {
    fn start(
        state: StateDirectory,
        renew: impl Fn(&vesta_supervisor::SupervisorClient) -> vesta_supervisor::Result<()>
        + Send
        + 'static,
    ) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let thread = {
            let (done, lost) = (done.clone(), lost.clone());
            std::thread::spawn(move || {
                let Ok(client) = vesta_supervisor::SupervisorClient::new(&state) else {
                    lost.store(true, Ordering::Release);
                    return;
                };
                let mut waited = Duration::ZERO;
                while !done.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(200));
                    waited += Duration::from_millis(200);
                    if waited >= RENEW_EVERY {
                        waited = Duration::ZERO;
                        if renew(&client).is_err() {
                            lost.store(true, Ordering::Release);
                        }
                    }
                }
            })
        };
        Self {
            done,
            lost,
            thread: Some(thread),
        }
    }
    /// Stop renewing; returns whether continuity was kept throughout.
    fn finish(mut self) -> bool {
        self.done.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        !self.lost.load(Ordering::Acquire)
    }
}

/// What holds the game consistent during a read; released before maintenance.
enum Consistency<'a> {
    Online {
        client: &'a vesta_supervisor::SupervisorClient,
        generation: GenerationId,
        token: String,
        renewer: Renewer,
    },
    Stopped {
        _block: crate::operations::StartBlock,
        hold: Option<(&'a vesta_supervisor::SupervisorClient, String, Renewer)>,
    },
}
impl Consistency<'_> {
    /// Release the lease or hold; returns whether continuity held throughout.
    fn release(self) -> bool {
        match self {
            Consistency::Online {
                client,
                generation,
                token,
                renewer,
            } => {
                let kept = renewer.finish();
                let _ = vesta_supervisor::save::resume(client, generation, &token);
                kept
            }
            Consistency::Stopped { _block, hold } => match hold {
                Some((client, token, renewer)) => {
                    let kept = renewer.finish();
                    let _ = vesta_supervisor::save::release_hold(client, &token);
                    kept
                }
                None => true,
            },
        }
    }
}

impl Inner {
    fn notify(&self) {
        let (lock, condvar) = &self.wake;
        *lock.lock().unwrap_or_else(|p| p.into_inner()) = true;
        condvar.notify_all();
    }

    fn work(self: Arc<Self>) {
        while !self.stopping.load(Ordering::Acquire) {
            match self.claim() {
                Ok(Some(job)) => {
                    let outcome = self.execute(&job);
                    // Never advance past an uncommitted outcome: the backup is not rerun.
                    let mut delay = Duration::from_millis(200);
                    while self.finish(&job, &outcome).is_err() {
                        std::thread::sleep(delay);
                        delay = (delay * 2).min(Duration::from_secs(30));
                    }
                }
                Ok(None) | Err(_) => {
                    let (lock, condvar) = &self.wake;
                    let mut woken = lock.lock().unwrap_or_else(|p| p.into_inner());
                    if !*woken {
                        woken = condvar
                            .wait_timeout(woken, Duration::from_secs(1))
                            .map(|(guard, _)| guard)
                            .unwrap_or_else(|p| p.into_inner().0);
                    }
                    *woken = false;
                }
            }
        }
    }

    fn claim(&self) -> Result<Option<Claimed>> {
        let now = self.db.clock().millis();
        Ok(self.db.write(|c| {
            let next = c
                .query_row(
                    "SELECT id,profile,kind,target,requested_by,requested_device FROM backup_jobs WHERE state='queued' ORDER BY created_at,id LIMIT 1",
                    [],
                    |r| {
                        Ok(Claimed {
                            id: r.get(0)?,
                            profile: r.get(1)?,
                            kind: r.get(2)?,
                            target: r.get(3)?,
                            principal: r.get(4)?,
                            device: r.get(5)?,
                        })
                    },
                )
                .optional()?;
            if let Some(job) = &next {
                c.execute(
                    "UPDATE backup_jobs SET state='running',started_at=? WHERE id=?",
                    params![now, job.id],
                )?;
            }
            Ok(next)
        })?)
    }

    fn finish(&self, job: &Claimed, outcome: &Outcome) -> Result<()> {
        let now = self.db.clock().millis();
        let mut result = outcome.result.to_string();
        if result.len() > MAX_RESULT_JSON {
            result = serde_json::json!({"truncated":true}).to_string();
        }
        self.db.write(|c| {
            c.execute(
                "UPDATE backup_jobs SET state=?,snapshot_id=?,result_json=?,error_code=?,finished_at=? WHERE id=?",
                params![outcome.state.wire(), outcome.snapshot, result, outcome.error, now, job.id],
            )?;
            // Bounded history: drop old finished jobs, then cap the finished count.
            c.execute(
                "DELETE FROM backup_jobs WHERE state IN ('succeeded','failed','uncertain') AND finished_at < ?",
                params![now.saturating_sub(FINISHED_RETENTION_MS)],
            )?;
            c.execute(
                "DELETE FROM backup_jobs WHERE id IN (SELECT id FROM backup_jobs WHERE state IN ('succeeded','failed','uncertain') ORDER BY finished_at DESC, id DESC LIMIT -1 OFFSET ?)",
                params![MAX_FINISHED_JOBS],
            )?;
            Ok(())
        })?;
        if job.kind == "backup" {
            let event = if outcome.state == JobState::Succeeded {
                BackupEvent::Completed {
                    profile: job.profile.clone(),
                    job: job.id.clone(),
                }
            } else {
                BackupEvent::Failed {
                    profile: job.profile.clone(),
                    job: job.id.clone(),
                }
            };
            (self.env.events)(event);
        }
        Ok(())
    }

    /// Current authority of the submitting subject, checked when the job starts.
    fn authorized(&self, job: &Claimed) -> bool {
        let (Ok(principal), Ok(device), Some(required)) = (
            PrincipalId::parse(&job.principal),
            DeviceId::parse(&job.device),
            capability(&job.kind),
        ) else {
            return false;
        };
        let subject = Subject { principal, device };
        self.db
            .read(|c| Ok(allowed(c, &self.db, &subject, required).unwrap_or(false)))
            .unwrap_or(false)
    }

    fn execute(&self, job: &Claimed) -> Outcome {
        let Some(profile) = self.settings.profile(&job.profile).cloned() else {
            return Outcome::failed(BackupError::UnknownProfile.code());
        };
        if !self.authorized(job) {
            return Outcome::failed("backup_authority_revoked");
        }
        match job.kind.as_str() {
            "backup" => self.backup(&profile),
            "verify" => match self.engine.check(None) {
                Ok(()) => Outcome {
                    state: JobState::Succeeded,
                    snapshot: None,
                    result: serde_json::json!({"verified":true}),
                    error: None,
                },
                Err(error) => Outcome::failed(error.code()),
            },
            "restore" => self.restore(&job.id, job.target.as_deref()),
            "restore_in_place" => self.restore_in_place(job, &profile),
            _ => Outcome::failed(BackupError::Invalid.code()),
        }
    }

    /// Establish consistency for the read: an online save lease, or a blocked and
    /// affirmatively stopped game (with a supervisor hold when supervised).
    fn consistency(
        &self,
        profile: &BackupProfile,
    ) -> std::result::Result<Consistency<'_>, &'static str> {
        let state = self.db.directory().clone();
        match profile.mode {
            BackupMode::Stopped => self.stopped(),
            BackupMode::Online => {
                let Some(client) = &self.env.supervisor else {
                    return Err("backup_online_requires_supervisor");
                };
                let Ok(Some(generation)) = (self.env.observe)() else {
                    return Err("backup_online_requires_running_server");
                };
                if profile.warn_seconds > 0 {
                    let _ = say(
                        client,
                        generation,
                        &format!("Backup starting in {} seconds", profile.warn_seconds),
                    );
                    std::thread::sleep(Duration::from_secs(u64::from(profile.warn_seconds)));
                }
                let token = vesta_supervisor::save::suspend(client, generation, LEASE, None)
                    .map(|(token, _)| token)
                    .map_err(|code| match code {
                        "supervisor_save_refused" => "backup_save_suspend_refused",
                        _ => "backup_save_suspend_failed",
                    })?;
                let renew_token = token.clone();
                let renewer = Renewer::start(state, move |client| {
                    vesta_supervisor::save::renew(client, generation, &renew_token, LEASE)
                });
                Ok(Consistency::Online {
                    client,
                    generation,
                    token,
                    renewer,
                })
            }
        }
    }

    /// Block every start (and, when supervised, take a maintenance hold), then
    /// require affirmative evidence that the game is stopped.
    fn stopped(&self) -> std::result::Result<Consistency<'_>, &'static str> {
        let block = self.env.start_gate.block();
        let hold = match &self.env.supervisor {
            Some(client) => {
                let token = vesta_supervisor::save::hold(client, LEASE)
                    .map_err(|_| "backup_requires_stopped_server")?;
                let renew_token = token.clone();
                let renewer = Renewer::start(self.db.directory().clone(), move |client| {
                    vesta_supervisor::save::renew_hold(client, &renew_token, LEASE)
                });
                Some((client, token, renewer))
            }
            None => None,
        };
        let stopped = matches!((self.env.observe)(), Ok(None));
        let consistency = Consistency::Stopped {
            _block: block,
            hold,
        };
        if !stopped {
            consistency.release();
            return Err("backup_requires_stopped_server");
        }
        Ok(consistency)
    }

    fn backup(&self, profile: &BackupProfile) -> Outcome {
        let ignore = match ignore_file(&self.game) {
            Ok(ignore) => ignore,
            Err(code) => return Outcome::failed(code),
        };
        let online = profile.mode == BackupMode::Online;
        // Budgets first: a refused backup never warns players or suspends saving.
        let budget = match self.capacity(profile) {
            Ok(budget) => budget,
            Err(code) => return Outcome::failed(code),
        };
        // Phase 1: the consistent read, holding file admission and lease/hold/block.
        let (result, before, after, kept) = {
            let admission = match crate::files::mutation_admission(self.db.directory().path()) {
                Ok(admission) => admission,
                Err(_) => return Outcome::failed(BackupError::Unavailable.code()),
            };
            let _files = admission.lock().unwrap_or_else(|p| p.into_inner());
            let consistency = match self.consistency(profile) {
                Ok(consistency) => consistency,
                Err(code) => return Outcome::failed(code),
            };
            let before = scan(&self.game);
            let tags = ["vesta".to_owned(), format!("{CANDIDATE}={}", profile.name)];
            let result = self.engine.backup_within(
                &self.game,
                &ignore,
                &tags,
                Some(&profile_tag(&profile.name)),
                budget.bytes,
            );
            let after = scan(&self.game);
            let kept = consistency.release();
            (result, before, after, kept)
        };
        // Phase 2: judge the snapshot; repository maintenance holds nothing else.
        let summary = match result {
            Ok(summary) => summary,
            Err(EngineError::LimitReached) => return Outcome::failed(budget.exceeded()),
            Err(error) => return Outcome::failed(error.code()),
        };
        let complete = before.complete && after.complete;
        let changes = compare(&before.files, &after.files);
        let world_changed = changes.iter().any(|c| c.world) || !kept || !complete;
        let fail = match profile.on_mutation {
            MutationPolicy::Warn => !kept,
            MutationPolicy::FailWorld => world_changed,
            MutationPolicy::FailAny => !changes.is_empty() || !kept || !complete,
        };
        let promoted = if fail {
            let _ = self
                .engine
                .tag_add(&summary.snapshot_id, &[REJECTED.to_owned()]);
            false
        } else {
            self.engine
                .tag_add(&summary.snapshot_id, &[profile_tag(&profile.name)])
                .is_ok()
        };
        let mut retention_error = None;
        let retention = Retention {
            keep_last: profile.keep_last,
            keep_daily: profile.keep_daily,
            keep_weekly: profile.keep_weekly,
            keep_monthly: profile.keep_monthly,
        };
        if promoted && !retention.is_empty() {
            retention_error = self
                .engine
                .forget(&profile_tag(&profile.name), retention)
                .err()
                .map(EngineError::code);
        }
        let consistency = match (online, kept) {
            (_, false) => "unproven",
            (true, true) => "world_save_consistent",
            (false, true) => "stopped",
        };
        let result = serde_json::json!({
            "snapshot_id": summary.snapshot_id,
            "files_new": summary.files_new,
            "files_changed": summary.files_changed,
            "files_unmodified": summary.files_unmodified,
            "data_added": summary.data_added,
            "total_bytes_processed": summary.total_bytes_processed,
            "consistency": consistency,
            "scan_complete": complete,
            "source_changes": changes.len(),
            "world_changed": world_changed,
            "changed_paths": changes.iter().take(MAX_REPORTED_CHANGES).map(|c| &c.path).collect::<Vec<_>>(),
            "retention_error": retention_error,
        });
        let error = if fail {
            Some(if kept {
                "backup_source_changed"
            } else {
                "backup_consistency_lost"
            })
        } else if !promoted {
            Some("backup_tag_failed")
        } else {
            None
        };
        Outcome {
            state: if error.is_none() {
                JobState::Succeeded
            } else {
                JobState::Failed
            },
            snapshot: Some(summary.snapshot_id),
            result,
            error,
        }
    }

    fn restore(&self, id: &str, target: Option<&str>) -> Outcome {
        let Some(root) = &self.settings.restore_root else {
            return Outcome::failed(BackupError::Disabled.code());
        };
        let Some(request) = target.and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        else {
            return Outcome::failed(BackupError::Invalid.code());
        };
        let snapshot = request["snapshot"].as_str().unwrap_or_default().to_owned();
        let paths: Vec<String> = request["paths"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        // Re-check the real root now: a link created since startup must not redirect it.
        if check_locations(&self.settings, &self.game, self.db.directory().path()).is_err() {
            return Outcome::failed("backup_restore_target_unavailable");
        }
        // A fresh directory per job: restores never overwrite anything.
        let directory = root.join(id);
        if fs::create_dir_all(root).is_err() || fs::create_dir(&directory).is_err() {
            return Outcome::failed("backup_restore_target_unavailable");
        }
        match self.engine.restore(&snapshot, &directory, &paths) {
            Ok(()) => Outcome {
                state: JobState::Succeeded,
                snapshot: Some(snapshot),
                result: serde_json::json!({"restored_to": directory.to_string_lossy()}),
                error: None,
            },
            Err(error) => Outcome::failed(error.code()),
        }
    }
}

fn say(
    client: &vesta_supervisor::SupervisorClient,
    generation: GenerationId,
    message: &str,
) -> vesta_supervisor::Result<()> {
    client
        .call(
            vesta_supervisor::wire::Op::Command {
                generation: generation.to_hex(),
                line: format!("say {message}"),
            },
            Duration::from_secs(15),
        )
        .map(|_| ())
}

/// The game root's `.vesta-backupignore` (see [`ignore`]), if present.
fn ignore_file(game: &Path) -> std::result::Result<Ignore, &'static str> {
    let path = game.join(IGNORE_FILE);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Ignore::default()),
        Ok(meta) if meta.is_file() && meta.len() <= MAX_IGNORE_BYTES => {
            let text = fs::read_to_string(&path).map_err(|_| "backup_ignore_file_invalid")?;
            Ignore::parse(&text)
        }
        _ => Err("backup_ignore_file_invalid"),
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Change {
    path: String,
    world: bool,
}

type Files = BTreeMap<String, (u64, u128)>;

struct Scan {
    files: Files,
    /// False when any directory or entry could not be read or a bound was reached.
    complete: bool,
}

/// Bounded metadata scan of regular files; links and specials are not followed.
fn scan(root: &Path) -> Scan {
    let started = Instant::now();
    let mut files = BTreeMap::new();
    let mut complete = true;
    let mut seen = 0usize;
    let mut stack = vec![PathBuf::new()];
    while let Some(relative) = stack.pop() {
        if started.elapsed() > MAX_SCAN_TIME {
            return Scan {
                files,
                complete: false,
            };
        }
        let Ok(entries) = fs::read_dir(root.join(&relative)) else {
            complete = false;
            continue;
        };
        for entry in entries {
            seen += 1;
            if seen > MAX_SCAN_ENTRIES || stack.len() > MAX_SCAN_PENDING {
                return Scan {
                    files,
                    complete: false,
                };
            }
            let Ok(entry) = entry else {
                complete = false;
                continue;
            };
            let Ok(meta) = entry.path().symlink_metadata() else {
                complete = false;
                continue;
            };
            let path = relative.join(entry.file_name());
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                files.insert(
                    path.to_string_lossy().replace('\\', "/"),
                    (meta.len(), modified),
                );
            }
        }
    }
    Scan { files, complete }
}

/// Paths under a directory containing `level.dat` are world data; logs, crash
/// reports and caches are volatile; anything else is "other".
fn is_world(path: &str, worlds: &[String]) -> bool {
    worlds
        .iter()
        .any(|w| path.starts_with(w.as_str()) && path[w.len()..].starts_with('/'))
}

fn compare(before: &Files, after: &Files) -> Vec<Change> {
    let worlds: Vec<String> = after
        .keys()
        .chain(before.keys())
        .filter_map(|p| p.strip_suffix("/level.dat").map(str::to_owned))
        .collect();
    let mut changes = Vec::new();
    for (path, meta) in after {
        if before.get(path) != Some(meta) {
            changes.push(path);
        }
    }
    for path in before.keys() {
        if !after.contains_key(path) {
            changes.push(path);
        }
    }
    changes
        .into_iter()
        .map(|path| Change {
            world: is_world(path, &worlds),
            path: path.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests;
