//! Durable owner import plan/review/apply jobs (`protocol/host-v1/import.md`).
//!
//! Jobs live in the host-private state database, keyed by random job, plan and
//! server ids; nothing assumes a single "first" server. Planning inspects the
//! admitted folder read-only, picks resources and Java, snapshots a stopped
//! source and binds the review to a hash and a random confirmation. Apply
//! re-validates the owner, plan, source identity/content, processes, port and
//! disk space, reserves the port/memory atomically, then copies the folder into
//! `<servers>/<serverId>/staging` through [`CopiedExport::copy_to_directory`]
//! (read-only source, verified hashes, resumable after a crash). Plain Rust
//! APIs; the router (H08) maps them to HTTP and runs long calls off the async
//! executor (`run_plan`/`run_apply` block and may drive a private runtime).
use super::{
    ImportError,
    browse::{FolderBrowser, OwnerBinding, display},
    inspection::{self, ProcessEvidence, SPACE_MARGIN_BYTES},
};
use crate::{
    files::{
        self, FileError, RelativePath,
        copied_export::{CopiedExport, CopyLimits, CopyProgress, CopySnapshot},
        native,
    },
    java::HostJavaPlan,
    machine::{
        HostState,
        owners::host_role_in,
        resources::{self, Reservations},
    },
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use uuid::Uuid;

pub const PLAN_TTL_MS: u64 = 10 * 60 * 1000;
/// Space kept free beyond the remaining copy while copying.
pub const COPY_RESERVE_BYTES: u64 = 64 * 1024 * 1024;
const PROGRESS_EVERY: u64 = 64;
const MAX_JOBS: i64 = 256;

/// Fixed error codes. Blocker codes from the contract are reused verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobError(pub &'static str);
impl JobError {
    pub const FORBIDDEN: Self = Self("forbidden");
    pub const NOT_FOUND: Self = Self("import_job_not_found");
    pub const CREATE_NOT_FOUND: Self = Self("create_job_not_found");
    pub const INVALID: Self = Self("invalid_request");
    pub const CONFLICT: Self = Self("conflict");
    pub const IDEMPOTENCY: Self = Self("idempotency_conflict");
    pub const EXPIRED: Self = Self("plan_expired");
    pub const CONFIRMATION: Self = Self("confirmation_invalid");
    pub const WARNINGS: Self = Self("warnings_unacknowledged");
    pub const EULA: Self = Self("eula_required");
    pub const SOURCE_STOP: Self = Self("source_stop_confirmation_required");
    pub const BLOCKED: Self = Self("plan_blocked");
    pub const CAPACITY: Self = Self("server_capacity_reached");
    pub const UNAVAILABLE: Self = Self("unavailable");
    pub fn code(self) -> &'static str {
        self.0
    }
}
impl From<ImportError> for JobError {
    fn from(error: ImportError) -> Self {
        Self(error.code())
    }
}
impl From<crate::ControlError> for JobError {
    fn from(error: crate::ControlError) -> Self {
        match error {
            crate::ControlError::Rejected {
                code: crate::IdentityErrorCode::Forbidden,
                ..
            } => Self::FORBIDDEN,
            _ => Self::UNAVAILABLE,
        }
    }
}
impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for JobError {}
pub type JobResult<T> = Result<T, JobError>;

/// Host observations and side inputs. The default reads this machine; tests
/// substitute deterministic values (processes, ports, disk, RAM, Java).
pub trait ImportEnvironment: Send + Sync {
    fn processes(&self, sources: &[PathBuf]) -> (Vec<ProcessEvidence>, &'static str) {
        inspection::processes(sources)
    }
    fn port_state(&self, port: u16) -> &'static str {
        inspection::port_state(port, "tcp", &BTreeSet::new()).state
    }
    fn free_bytes(&self, path: &Path) -> Option<u64> {
        native::volume_space(path)
            .ok()
            .map(|(available, _)| available)
    }
    fn memory_total_bytes(&self) -> Option<u64> {
        crate::metrics::host_memory_total_bytes()
    }
    fn plan_java(&self, minecraft_version: Option<&str>, excluded: &[PathBuf]) -> HostJavaPlan;
    /// Test seam: called before each copied entry.
    fn before_copy_entry(&self, _index: u64) {}
    /// Make the planned trusted runtime available; returns its executable.
    fn ensure_java(
        &self,
        runtime: &ReviewRuntime,
        excluded: &[PathBuf],
    ) -> Result<PathBuf, &'static str>;
}
/// Live host environment with the shared managed Java cache.
pub struct HostEnvironment {
    pub java_cache: PathBuf,
}
impl ImportEnvironment for HostEnvironment {
    fn ensure_java(
        &self,
        runtime: &ReviewRuntime,
        excluded: &[PathBuf],
    ) -> Result<PathBuf, &'static str> {
        let plan = HostJavaPlan {
            required_major: runtime.required_major,
            selected_runtime_id: runtime.selected_runtime_id.clone(),
            download_required: runtime.download_required,
            verified: runtime.verified,
            status: "resolved",
            blocker: None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "java_unresolved")?;
        runtime
            .block_on(crate::java::ensure_host_java(
                &self.java_cache,
                &plan,
                excluded,
            ))
            .map(|java| PathBuf::from(java.path))
            .map_err(|_| "java_unresolved")
    }
    fn plan_java(&self, minecraft_version: Option<&str>, excluded: &[PathBuf]) -> HostJavaPlan {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(crate::java::plan_host_java(
                &self.java_cache,
                minecraft_version,
                excluded,
            )),
            Err(_) => crate::java::plan_from(&[], None),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchChoice {
    /// `jar` or `java_argfile`.
    pub kind: String,
    pub relative_path: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanRequest {
    pub folder_token: String,
    pub name: String,
    pub game_port: Option<u16>,
    #[serde(rename = "memoryMiB", alias = "memoryMib")]
    pub memory_mib: Option<u64>,
    pub launch_choice: Option<LaunchChoice>,
    pub minecraft_version: Option<String>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlanAccepted {
    pub job_id: String,
    pub state: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyRequest {
    pub plan_id: String,
    pub confirmation: String,
    pub acknowledged_warnings: Vec<String>,
    pub eula_accepted: bool,
    pub source_stopped_confirmed: bool,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApplyAccepted {
    pub job_id: String,
    pub server_id: Uuid,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CancelResponse {
    pub state: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
}
/// Durable job envelope shared by imports and creates; only the review differs.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Job<R> {
    pub job_id: String,
    /// `planning|review|applying|ready|starting|online|blocked|failed|cancelled|interrupted`.
    pub state: String,
    /// Import: `inspect|conflicts|java|snapshot|copy|configure|register|start`.
    /// Create: `resolving|downloading_java|downloading_software|installing|registering|starting`.
    pub phase: String,
    pub error: Option<String>,
    pub progress: Progress,
    pub review: Option<R>,
    pub server_id: Option<Uuid>,
}
pub type ImportJob = Job<Review>;

/// Which flow owns a row of the shared job store. Every lookup is filtered by
/// kind, so an import id is unknown to the create routes and vice versa.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Import,
    Create,
}
impl Kind {
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Create => "create",
        }
    }
    pub(crate) fn not_found(self) -> JobError {
        match self {
            Self::Import => JobError::NOT_FOUND,
            Self::Create => JobError::CREATE_NOT_FOUND,
        }
    }
    fn saturated(self) -> JobError {
        match self {
            Self::Import => JobError("import_jobs_saturated"),
            Self::Create => JobError("create_jobs_saturated"),
        }
    }
    /// Phases before anything is committed: cancel releases the reservation.
    fn cancellable(self, phase: &str) -> bool {
        match self {
            Self::Import => matches!(phase, "inspect" | "conflicts" | "snapshot" | "copy"),
            Self::Create => matches!(
                phase,
                "resolving" | "downloading_java" | "downloading_software" | "installing"
            ),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    pub plan_id: String,
    /// Null when blocked or no longer awaiting review.
    pub confirmation: Option<String>,
    /// Milliseconds since the Unix epoch; transport renders RFC 3339.
    pub expires_at: u64,
    pub source: ReviewSource,
    pub detected: ReviewDetected,
    pub runtime: ReviewRuntime,
    pub destination: ReviewDestination,
    pub resources: ReviewResources,
    pub processes: Vec<ReviewProcess>,
    pub ports: Vec<ReviewPort>,
    pub inspection: ReviewInspection,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    /// As in the inspection report; explains `not_minecraft_server`.
    #[serde(default)]
    pub found: Vec<String>,
    #[serde(default)]
    pub missing: Vec<String>,
    #[serde(default)]
    pub nearby: Vec<inspection::Nearby>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSource {
    pub display_path: String,
    pub snapshot: Option<String>,
    pub files: u64,
    pub bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDetected {
    pub kind: String,
    pub instance_name: Option<String>,
    pub minecraft_version: Option<String>,
    pub loader: Option<String>,
    pub launch: Option<LaunchChoice>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRuntime {
    pub required_major: Option<u32>,
    pub selected_runtime_id: Option<String>,
    pub download_required: bool,
    pub verified: bool,
    /// A JDK (with `javac`) is required: CraftBukkit/Spigot source builds.
    /// Host-private; absent from responses unless set.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub jdk: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDestination {
    pub server_id: Uuid,
    pub display_path: String,
    pub free_bytes: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewResources {
    pub game_port: Option<u16>,
    #[serde(rename = "memoryMiB", alias = "memoryMib")]
    pub memory_mib: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewProcess {
    pub kind: String,
    pub pid: u32,
    pub name: String,
    pub start_identity: Option<String>,
    pub relation: String,
    pub state: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPort {
    pub port: u16,
    pub protocol: String,
    pub state: String,
    pub owner: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewInspection {
    pub processes: String,
    pub ports: String,
}

/// Host-private half of a plan; never returned to callers.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Private {
    pub source: PathBuf,
    pub source_identity: String,
    pub snapshot: Option<CopySnapshot>,
    pub plan_hash: String,
}

struct Inner {
    host: HostState,
    browser: Arc<FolderBrowser>,
    servers: PathBuf,
    env: Arc<dyn ImportEnvironment>,
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    running: Mutex<HashSet<String>>,
    /// Create uploads still streaming (their partial files are live).
    uploading: Mutex<HashSet<String>>,
}
/// Owner import job service. Construct once per host process.
#[derive(Clone)]
pub struct ImportJobs(Arc<Inner>);

struct Running<'a> {
    jobs: &'a ImportJobs,
    id: String,
}
impl Drop for Running<'_> {
    fn drop(&mut self) {
        if let Ok(mut running) = self.jobs.0.running.lock() {
            running.remove(&self.id);
        }
        if let Ok(mut cancels) = self.jobs.0.cancels.lock() {
            cancels.remove(&self.id);
        }
    }
}

/// Why a copy stopped, decided by the boundary rather than by I/O.
enum Stop {
    Cancelled,
    LowDisk,
}

impl ImportJobs {
    /// `servers` is the managed server storage root (for example
    /// `/var/lib/vesta/servers`); it must be listed as protected in `browser`.
    /// Work interrupted by a crash or restart becomes `interrupted`.
    pub fn open(
        host: HostState,
        browser: Arc<FolderBrowser>,
        servers: &Path,
        env: Arc<dyn ImportEnvironment>,
    ) -> JobResult<Self> {
        files::create_local_directory(servers, true).map_err(|_| JobError::UNAVAILABLE)?;
        let servers = fs::canonicalize(servers).map_err(|_| JobError::UNAVAILABLE)?;
        host.write(|c| {
            c.execute_batch(SCHEMA)?;
            // Creates share the store (H19); rows from before carry the import kind.
            let has_kind: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('machine_import_jobs') WHERE name='kind')",
                [],
                |r| r.get(0),
            )?;
            if !has_kind {
                c.execute_batch(
                    "ALTER TABLE machine_import_jobs ADD COLUMN kind TEXT NOT NULL DEFAULT 'import' CHECK(kind IN ('import','create'))",
                )?;
            }
            c.execute(
                "UPDATE machine_import_jobs SET state='interrupted',updated_ms=?1 WHERE state IN ('planning','applying','starting')",
                [host.now_ms()],
            )?;
            Ok(())
        })?;
        let jobs = Self(Arc::new(Inner {
            host,
            browser,
            servers,
            env,
            cancels: Mutex::new(HashMap::new()),
            running: Mutex::new(HashSet::new()),
            uploading: Mutex::new(HashSet::new()),
        }));
        jobs.sweep_abandoned();
        Ok(jobs)
    }

    /// Startup: a job that settled without committing (failed, cancelled or
    /// blocked) owns nothing on disk any more. Its destination is removed if a
    /// crash or I/O error left it behind: a create's whole never-registered
    /// directory, an import's staging copy (a quarantined copy is kept for the
    /// owner). Only directories named by such a job and never registered are
    /// touched; interrupted jobs keep theirs for resume, unknown folders stay.
    fn sweep_abandoned(&self) {
        let Ok(rows) = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT kind,server_id FROM machine_import_jobs WHERE server_id IS NOT NULL AND state IN ('failed','cancelled','blocked')",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }) else {
            return;
        };
        for (kind, server) in rows {
            let Ok(server) = Uuid::parse_str(&server) else {
                continue;
            };
            if !self.server_dir(server).exists() || !matches!(self.0.host.server(server), Ok(None))
            {
                continue;
            }
            if kind == "create" {
                self.remove_unregistered(server);
            } else {
                self.remove_staging(server);
            }
        }
    }
    pub fn servers_root(&self) -> &Path {
        &self.0.servers
    }
    /// Names held by committed servers and pending imports share one namespace.
    pub fn suggested_create_name(&self) -> JobResult<String> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT name FROM machine_servers WHERE state!='deleted'
                UNION SELECT json_extract(j.request,'$.name') FROM machine_import_jobs j
                JOIN machine_import_reservations r ON r.job_id=j.job_id WHERE r.released=0",
            )?;
            let names = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for n in 1..=100_000 {
                let name = if n == 1 {
                    "Minecraft Server".into()
                } else {
                    format!("Minecraft Server {n}")
                };
                if !names.iter().any(|v| v.trim().eq_ignore_ascii_case(&name)) {
                    return Ok(name);
                }
            }
            Err(JobError::CAPACITY)
        })
    }
    /// Shared committed and in-flight reservations, used by owner resource previews.
    pub fn resource_plans(
        &self,
        game_port: Option<u16>,
        memory_mib: Option<u64>,
    ) -> JobResult<(resources::MemoryPlan, resources::PortPlan)> {
        let held = self.read(|c| reservations_in(c, None))?;
        Ok((
            resources::plan_memory(self.0.env.memory_total_bytes(), &held, memory_mib),
            resources::plan_port_with(&held, game_port, |p| self.0.env.port_state(p)),
        ))
    }
    pub fn server_dir(&self, server_id: Uuid) -> PathBuf {
        self.0.servers.join(server_id.to_string())
    }

    /// Current owner on this host, matching the binding's host id.
    fn owner(&self, c: &Connection, binding: &OwnerBinding) -> JobResult<Uuid> {
        let account = Uuid::parse_str(&binding.account_id).map_err(|_| JobError::FORBIDDEN)?;
        let host = super::super::state::snapshot_in(c)?.host_id;
        if binding.host_id != host.to_string() || host_role_in(c, account)?.is_none() {
            return Err(JobError::FORBIDDEN);
        }
        Ok(account)
    }
    fn write<T>(&self, work: impl FnOnce(&Connection) -> JobResult<T>) -> JobResult<T> {
        let mut out = None;
        let result = self.0.host.write(|c| match work(c) {
            Ok(value) => {
                out = Some(Ok(value));
                Ok(())
            }
            Err(error) => {
                out = Some(Err(error));
                Err(crate::ControlError::rejected(
                    crate::IdentityErrorCode::Conflict,
                ))
            }
        });
        match (out, result) {
            (Some(Ok(value)), Ok(())) => Ok(value),
            (Some(Err(error)), _) => Err(error),
            (_, Err(error)) => Err(error.into()),
            (None, Ok(())) => Err(JobError::UNAVAILABLE),
        }
    }
    fn read<T>(&self, work: impl FnOnce(&Connection) -> JobResult<T>) -> JobResult<T> {
        let mut out = None;
        let result = self.0.host.read(|c| {
            out = Some(work(c));
            Ok(())
        });
        result?;
        out.unwrap_or(Err(JobError::UNAVAILABLE))
    }

    /// Plan synchronously: [`Self::begin_plan`] then [`Self::run_plan`].
    pub fn plan(&self, binding: &OwnerBinding, request: PlanRequest) -> JobResult<ImportJob> {
        let accepted = self.begin_plan(binding, request)?;
        self.run_plan(binding, &accepted.job_id)
    }

    /// Validate and record a planning job (the 202 response).
    pub fn begin_plan(
        &self,
        binding: &OwnerBinding,
        request: PlanRequest,
    ) -> JobResult<PlanAccepted> {
        validate_request(&request)?;
        let raw = serde_json::to_string(&request).map_err(|_| JobError::INVALID)?;
        self.insert_planning(binding, Kind::Import, "inspect", &raw)
    }

    /// Record a `planning` job of either kind, pruning stale attempts first.
    pub(crate) fn insert_planning(
        &self,
        binding: &OwnerBinding,
        kind: Kind,
        phase: &str,
        raw: &str,
    ) -> JobResult<PlanAccepted> {
        let id = Uuid::new_v4().to_string();
        let now = self.0.host.now_ms();
        self.write(|c| {
            self.owner(c, binding)?;
            // Prune stale reviews and finished attempts before enforcing the cap.
            c.execute(
                "DELETE FROM machine_import_jobs WHERE state IN ('review','blocked','cancelled','failed') AND apply_key IS NULL AND updated_ms<?1",
                [now - 3_600_000],
            )?;
            let total: i64 =
                c.query_row("SELECT count(*) FROM machine_import_jobs", [], |r| r.get(0))?;
            if total >= MAX_JOBS {
                return Err(kind.saturated());
            }
            c.execute(
                "INSERT INTO machine_import_jobs(job_id,account_id,device_id,state,phase,request,created_ms,updated_ms,kind) VALUES(?1,?2,?3,'planning',?6,?4,?5,?5,?7)",
                params![id, binding.account_id, binding.device_id, raw, now, phase, kind.id()],
            )?;
            audit(c, &format!("{}.plan", kind.id()), &binding.account_id, &id, now)?;
            Ok(())
        })?;
        Ok(PlanAccepted {
            job_id: id,
            state: "planning".into(),
        })
    }

    /// Inspect, plan resources/Java and snapshot; ends in `review` or `blocked`.
    pub fn run_plan(&self, binding: &OwnerBinding, job_id: &str) -> JobResult<ImportJob> {
        let raw: String = self.read(|c| {
            self.owner(c, binding)?;
            let row: Option<(String, String, String)> = c
                .query_row(
                    "SELECT request,state,account_id FROM machine_import_jobs WHERE job_id=?1 AND kind='import'",
                    [job_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            match row {
                Some((raw, state, account))
                    if state == "planning" && account == binding.account_id =>
                {
                    Ok(raw)
                }
                Some(_) => Err(JobError::CONFLICT),
                None => Err(JobError::NOT_FOUND),
            }
        })?;
        let request: PlanRequest = serde_json::from_str(&raw).map_err(|_| JobError::UNAVAILABLE)?;
        match self.compute_plan(binding, job_id, &request) {
            Ok((review, private)) => {
                let state = if review.blockers.is_empty() {
                    "review"
                } else {
                    "blocked"
                };
                let plan_id = review.plan_id.clone();
                self.write(|c| {
                    c.execute(
                        "UPDATE machine_import_jobs SET state=?1,phase='inspect',plan_id=?2,review=?3,private=?4,updated_ms=?5 WHERE job_id=?6 AND state='planning'",
                        params![
                            state,
                            plan_id,
                            serde_json::to_string(&review).map_err(|_| JobError::UNAVAILABLE)?,
                            serde_json::to_string(&private).map_err(|_| JobError::UNAVAILABLE)?,
                            self.0.host.now_ms(),
                            job_id
                        ],
                    )?;
                    Ok(())
                })?;
            }
            Err(error) => {
                self.write(|c| {
                    c.execute(
                        "UPDATE machine_import_jobs SET state='failed',error=?1,updated_ms=?2 WHERE job_id=?3 AND state='planning'",
                        params![error.code(), self.0.host.now_ms(), job_id],
                    )?;
                    Ok(())
                })?;
            }
        }
        self.status(binding, job_id)
    }

    fn compute_plan(
        &self,
        binding: &OwnerBinding,
        job_id: &str,
        request: &PlanRequest,
    ) -> JobResult<(Review, Private)> {
        let opened = self.0.browser.open(binding, &request.folder_token)?;
        let source = opened.path.clone();
        let mut blockers = BTreeSet::new();
        let mut warnings = BTreeSet::new();
        let report = inspection::inspect_path(&source, &mut blockers, &mut warnings);
        // Vanilla binds RCON to `server-ip` (the game address); there is no
        // separate loopback bind, so the copy disables it and says so.
        if report.server.rcon_port.is_some() {
            warnings.insert("rcon_disabled");
        }
        let mut detected = ReviewDetected {
            kind: report.detected.kind.into(),
            instance_name: report.detected.instance_name.clone(),
            minecraft_version: report.detected.minecraft_version.clone(),
            loader: report.detected.loader.and_then(|l| {
                serde_json::to_value(l)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
            }),
            launch: report.detected.launch.as_ref().map(|l| LaunchChoice {
                kind: l.kind.into(),
                relative_path: l.relative_path.clone(),
            }),
        };
        // Owner overrides resolve detection gaps only; they never add arguments.
        if let Some(version) = &request.minecraft_version {
            detected.minecraft_version = Some(version.clone());
        }
        if let Some(choice) = &request.launch_choice {
            let mut path = source.clone();
            path.extend(choice.relative_path.split('/'));
            let plain = fs::symlink_metadata(&path).is_ok_and(|m| {
                m.is_file() && !m.file_type().is_symlink() && !super::browse::reparse(&m)
            });
            if !plain {
                return Err(JobError::INVALID);
            }
            detected.launch = Some(choice.clone());
        }
        if detected.launch.is_some() {
            blockers.remove("launch_unresolved");
        }
        if detected.minecraft_version.is_some() && detected.launch.is_some() {
            blockers.remove("software_unresolved");
        }

        if let Some(launch) = &detected.launch {
            if launch.kind == "java_argfile"
                && super::arguments::parse(&source, &source.join(&launch.relative_path)).is_err()
            {
                blockers.insert("launch_arguments_unsupported");
            }
        }
        let (processes, process_state) = self.0.env.processes(std::slice::from_ref(&source));
        if processes
            .iter()
            .any(|p| p.relation == "source" && p.state == "running")
        {
            blockers.insert("source_running");
        } else if process_state != "complete" {
            warnings.insert("source_process_unknown");
        }
        if processes.iter().any(|p| p.relation == "amp_controller") {
            warnings.insert("amp_controller_running");
        }

        let managed = self.managed_ports()?;
        if let Some(code) = inspection::stopped_evidence(&source, report.server.game_port, &managed)
        {
            blockers.insert(code);
        }
        let reservations = self.read(|c| reservations_in(c, None))?;
        let env = self.0.env.clone();
        let port =
            resources::plan_port_with(&reservations, request.game_port, |p| env.port_state(p));
        if let Some(blocker) = port.blocker {
            blockers.insert(blocker);
        }
        let mut ports = Vec::new();
        if let Some(game_port) = port.game_port {
            let state = self.0.env.port_state(game_port);
            ports.push(ReviewPort {
                port: game_port,
                protocol: "tcp".into(),
                state: state.into(),
                owner: (state == "occupied").then(|| "unknown".into()),
            });
        }
        let memory = resources::plan_software_memory(
            self.0.env.memory_total_bytes(), &reservations, request.memory_mib, detected.loader.as_deref(),
        );
        warnings.extend(memory.warnings.iter().copied());
        if let Some(blocker) = memory.blocker {
            blockers.insert(blocker);
        }
        let java = self.0.env.plan_java(
            detected.minecraft_version.as_deref(),
            &[source.clone(), self.0.servers.clone()],
        );
        if let Some(blocker) = java.blocker {
            blockers.insert(blocker);
        }

        // Snapshot (full content hashes) only for a plan that could be applied:
        // a live or unreadable source's inventory would be discarded anyway.
        let mut snapshot = None;
        if blockers.is_empty() {
            let (files_total, bytes_total) = (report.size.files, report.size.bytes);
            match CopiedExport::open(&source, &[&self.0.servers]).and_then(|export| {
                export
                    .with_limits(CopyLimits::IMPORT)
                    .snapshot_observed(|p| self.report_hashing(job_id, p, files_total, bytes_total))
            }) {
                Ok(taken) => snapshot = Some(taken),
                Err(error) => {
                    blockers.insert(snapshot_blocker(&error));
                }
            }
        }
        if let (Some(snapshot), Some(launch)) = (&snapshot, &detected.launch)
            && !snapshot
                .entries
                .iter()
                .any(|e| !e.directory && e.path == launch.relative_path)
        {
            blockers.insert("launch_unresolved");
        }
        let (files_count, bytes) = match &snapshot {
            Some(s) => (
                s.entries.iter().filter(|e| !e.directory).count() as u64,
                s.bytes,
            ),
            None => (report.size.files, report.size.bytes),
        };
        let free = self.0.env.free_bytes(&self.0.servers);
        if report.size.complete
            && free.is_some_and(|free| free < bytes.saturating_add(SPACE_MARGIN_BYTES))
        {
            blockers.insert("insufficient_space");
        }

        let invalid_source = if blockers.contains("source_unreadable") {
            Some("source_unreadable")
        } else if !report.minecraft {
            Some("not_minecraft_server")
        } else {
            None
        };
        if let Some(cause) = invalid_source {
            blockers.clear();
            blockers.insert(cause);
            warnings.clear();
        }
        let server_id = Uuid::new_v4();
        let plan_id = Uuid::new_v4().to_string();
        let now = super::browse::now_ms();
        let mut review = Review {
            plan_id,
            confirmation: None,
            expires_at: now.saturating_add(PLAN_TTL_MS),
            source: ReviewSource {
                display_path: display(&source),
                snapshot: snapshot.as_ref().map(|s| s.sha256.clone()),
                files: files_count,
                bytes,
            },
            detected,
            runtime: ReviewRuntime {
                required_major: java.required_major,
                selected_runtime_id: java.selected_runtime_id.clone(),
                download_required: java.download_required,
                verified: java.verified,
                jdk: false,
            },
            destination: ReviewDestination {
                server_id,
                display_path: display(&self.server_dir(server_id).join("game")),
                free_bytes: free,
            },
            resources: ReviewResources {
                game_port: port.game_port,
                memory_mib: memory.memory_mib,
            },
            processes: processes
                .iter()
                .map(|p| ReviewProcess {
                    kind: p.kind.into(),
                    pid: p.pid,
                    name: p.name.clone(),
                    start_identity: p.start_identity.clone(),
                    relation: p.relation.into(),
                    state: p.state.into(),
                })
                .collect(),
            inspection: ReviewInspection {
                processes: process_state.into(),
                ports: if ports.iter().any(|p| p.state == "unknown") {
                    "unknown".into()
                } else {
                    "complete".into()
                },
            },
            ports,
            blockers: blockers.iter().map(|b| (*b).to_owned()).collect(),
            warnings: warnings.iter().map(|w| (*w).to_owned()).collect(),
            found: report.found.iter().map(|f| (*f).to_owned()).collect(),
            missing: report.missing.iter().map(|m| (*m).to_owned()).collect(),
            nearby: if invalid_source == Some("not_minecraft_server") {
                inspection::nearby(&self.0.browser, binding, &request.folder_token)
            } else {
                Vec::new()
            },
        };
        if review.blockers.is_empty() {
            review.confirmation = Some(random_token()?);
        }
        let private = Private {
            source,
            source_identity: opened.identity,
            snapshot,
            plan_hash: String::new(),
        };
        let private = Private {
            plan_hash: plan_hash(binding, &review, &private)?,
            ..private
        };
        Ok((review, private))
    }

    /// Durable job view for an owner.
    pub fn status(&self, binding: &OwnerBinding, job_id: &str) -> JobResult<ImportJob> {
        self.read(|c| {
            self.owner(c, binding)?;
            job_in(c, job_id)
        })
    }

    /// Exact retry of an earlier apply with this key (any state since), or a
    /// conflict when the key was used for a different body or job kind.
    pub(crate) fn apply_retry(
        c: &Connection,
        binding: &OwnerBinding,
        kind: Kind,
        idempotency_key: &str,
        body_hash: &str,
    ) -> JobResult<Option<ApplyAccepted>> {
        let retry: Option<(String, Option<String>, Option<String>, String)> = c
            .query_row(
                "SELECT job_id,server_id,apply_hash,kind FROM machine_import_jobs WHERE apply_key=?1 AND account_id=?2",
                params![idempotency_key, binding.account_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((job_id, server, hash, used_by)) = retry else {
            return Ok(None);
        };
        if hash.as_deref() != Some(body_hash) || used_by != kind.id() {
            return Err(JobError::IDEMPOTENCY);
        }
        let server_id = server
            .and_then(|s| Uuid::parse_str(&s).ok())
            .ok_or(JobError::UNAVAILABLE)?;
        Ok(Some(ApplyAccepted { job_id, server_id }))
    }

    /// Re-check name, port and memory against every live reservation and
    /// registered server, then reserve them for `job_id` in the caller's
    /// transaction. Racing plans for one port cannot both pass: the second
    /// sees the first's reservation (and the unique port index backs this up).
    pub(crate) fn reserve_in(
        &self,
        c: &Connection,
        job_id: &str,
        name: &str,
        server_id: Uuid,
        port: u16,
        memory: u64,
    ) -> JobResult<()> {
        let held = reservations_in(c, None)?;
        if name_in_use(c, name)? {
            return Err(JobError("name_in_use"));
        }
        if held.ports.contains(&port) {
            return Err(JobError("game_port_in_use"));
        }
        if resources::plan_memory(self.0.env.memory_total_bytes(), &held, Some(memory))
            .blocker
            .is_some()
        {
            return Err(JobError("memory_budget_exceeded"));
        }
        c.execute(
            "INSERT INTO machine_import_reservations(server_id,job_id,game_port,memory_mib) VALUES(?1,?2,?3,?4)",
            params![server_id.to_string(), job_id, port, memory as i64],
        )?;
        Ok(())
    }

    /// Owner confirms the exact reviewed plan. Reserves the destination server
    /// id, port and memory in one transaction; the same idempotency key and
    /// body returns the same job, a different body with that key conflicts.
    /// The copy itself runs in [`Self::run_apply`].
    pub fn apply(
        &self,
        binding: &OwnerBinding,
        request: &ApplyRequest,
        idempotency_key: &str,
    ) -> JobResult<ApplyAccepted> {
        if idempotency_key.is_empty()
            || idempotency_key.len() > 128
            || idempotency_key.chars().any(char::is_control)
        {
            return Err(JobError::INVALID);
        }
        let body_hash = hex::encode(Sha256::digest(
            serde_json::to_vec(request).map_err(|_| JobError::INVALID)?,
        ));
        let now = self.0.host.now_ms();
        self.write(|c| {
            let account = self.owner(c, binding)?;
            // Exact retries first, whatever state the job has reached since.
            if let Some(accepted) =
                Self::apply_retry(c, binding, Kind::Import, idempotency_key, &body_hash)?
            {
                return Ok(accepted);
            }
            #[allow(clippy::type_complexity)]
            let row: Option<(String, String, String, String, Option<String>, Option<String>)> = c
                .query_row(
                    "SELECT job_id,state,account_id,device_id,review,private FROM machine_import_jobs WHERE plan_id=?1 AND kind='import'",
                    [&request.plan_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .optional()?;
            let Some((job_id, state, job_account, job_device, review, private)) = row else {
                return Err(JobError::NOT_FOUND);
            };
            if job_account != binding.account_id || job_device != binding.device_id {
                return Err(JobError::FORBIDDEN);
            }
            match state.as_str() {
                "review" => {}
                "blocked" => return Err(JobError::BLOCKED),
                _ => return Err(JobError::CONFLICT),
            }
            let review: Review = decode(review)?;
            let private: Private = decode(private)?;
            if super::browse::now_ms() >= review.expires_at {
                return Err(JobError::EXPIRED);
            }
            let confirmation = review.confirmation.as_deref().ok_or(JobError::BLOCKED)?;
            if !constant_eq(confirmation.as_bytes(), request.confirmation.as_bytes())
                || plan_hash(binding, &review, &private)? != private.plan_hash
            {
                return Err(JobError::CONFIRMATION);
            }
            let acknowledged: BTreeSet<&str> =
                request.acknowledged_warnings.iter().map(String::as_str).collect();
            let expected: BTreeSet<&str> = review.warnings.iter().map(String::as_str).collect();
            if acknowledged != expected {
                return Err(JobError::WARNINGS);
            }
            if !request.eula_accepted {
                return Err(JobError::EULA);
            }
            if !request.source_stopped_confirmed {
                return Err(JobError::SOURCE_STOP);
            }
            let server_id = review.destination.server_id;
            let port = review.resources.game_port.ok_or(JobError::BLOCKED)?;
            let memory = review.resources.memory_mib.ok_or(JobError::BLOCKED)?;
            let raw: String = c.query_row("SELECT request FROM machine_import_jobs WHERE job_id=?1", [&job_id], |r| r.get(0))?;
            let requested: PlanRequest = serde_json::from_str(&raw).map_err(|_| JobError::UNAVAILABLE)?;
            self.reserve_in(c, &job_id, requested.name.trim(), server_id, port, memory)?;
            c.execute(
                "UPDATE machine_import_jobs SET state='applying',phase='conflicts',error=NULL,server_id=?1,apply_key=?2,apply_hash=?3,eula_accepted_ms=?4,eula_account=?5,updated_ms=?4 WHERE job_id=?6",
                params![server_id.to_string(), idempotency_key, body_hash, now, account.to_string(), job_id],
            )?;
            audit(c, "import.apply", &binding.account_id, &job_id, now)?;
            Ok(ApplyAccepted { job_id, server_id })
        })
    }

    /// Resume an `interrupted` apply after a crash/restart. Everything is
    /// re-validated; a changed source blocks and requires a new plan.
    pub fn resume(&self, binding: &OwnerBinding, job_id: &str) -> JobResult<ImportJob> {
        self.resume_kind(binding, job_id, Kind::Import)?;
        self.status(binding, job_id)
    }
    pub(crate) fn resume_kind(
        &self,
        binding: &OwnerBinding,
        job_id: &str,
        kind: Kind,
    ) -> JobResult<()> {
        let now = self.0.host.now_ms();
        self.write(|c| {
            self.owner(c, binding)?;
            let (state, phase, server): (String, String, Option<String>) = c
                .query_row(
                    "SELECT state,phase,server_id FROM machine_import_jobs WHERE job_id=?1 AND kind=?2",
                    params![job_id, kind.id()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .ok_or(kind.not_found())?;
            if state != "interrupted" {
                return Err(JobError::CONFLICT);
            }
            if server.is_none() {
                // Interrupted while planning: the owner replans.
                c.execute(
                    "UPDATE machine_import_jobs SET state='failed',error='replan_required',updated_ms=?1 WHERE job_id=?2",
                    params![now, job_id],
                )?;
                return Ok(());
            }
            let (state, phase) = match (kind, phase.as_str()) {
                (Kind::Import, "conflicts" | "snapshot" | "copy") => {
                    ("applying", "conflicts".to_owned())
                }
                // Every uncommitted install step restarts from a clean,
                // never-registered destination; artifacts are re-verified.
                (Kind::Create, p) if kind.cancellable(p) => ("applying", "resolving".to_owned()),
                // A start may already be in flight: never spawn again here; the
                // owner retries start, which reuses its idempotency key.
                (_, "start" | "starting") => ("ready", phase),
                _ => ("applying", phase),
            };
            c.execute(
                "UPDATE machine_import_jobs SET state=?4,phase=?1,error=NULL,cancel_requested=0,updated_ms=?2 WHERE job_id=?3",
                params![phase, now, job_id, state],
            )?;
            audit(c, &format!("{}.resume", kind.id()), &binding.account_id, job_id, now)?;
            Ok(())
        })
    }

    /// Cancel at a safe boundary. Never touches the source or other processes.
    /// An incomplete, unregistered destination is removed (audited).
    pub fn cancel(&self, binding: &OwnerBinding, job_id: &str) -> JobResult<CancelResponse> {
        self.cancel_kind(binding, job_id, Kind::Import)
    }
    pub(crate) fn cancel_kind(
        &self,
        binding: &OwnerBinding,
        job_id: &str,
        kind: Kind,
    ) -> JobResult<CancelResponse> {
        let now = self.0.host.now_ms();
        let (state, server) = self.write(|c| {
            self.owner(c, binding)?;
            let (state, phase, server): (String, String, Option<String>) = c
                .query_row(
                    "SELECT state,phase,server_id FROM machine_import_jobs WHERE job_id=?1 AND kind=?2",
                    params![job_id, kind.id()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .ok_or(kind.not_found())?;
            let copy_phase = kind.cancellable(&phase);
            let next = match state.as_str() {
                "cancelled" => return Ok(("cancelled".to_owned(), None)),
                "planning" | "review" | "blocked" => "cancelled",
                "interrupted" if copy_phase => "cancelled",
                "applying" if copy_phase => {
                    c.execute(
                        "UPDATE machine_import_jobs SET cancel_requested=1,updated_ms=?1 WHERE job_id=?2",
                        params![now, job_id],
                    )?;
                    audit(c, &format!("{}.cancel_request", kind.id()), &binding.account_id, job_id, now)?;
                    return Ok(("applying".to_owned(), None));
                }
                _ => return Err(JobError::CONFLICT),
            };
            c.execute(
                "UPDATE machine_import_jobs SET state=?1,error=NULL,updated_ms=?2 WHERE job_id=?3",
                params![next, now, job_id],
            )?;
            c.execute(
                "UPDATE machine_import_reservations SET released=1 WHERE job_id=?1",
                [job_id],
            )?;
            audit(c, &format!("{}.cancel", kind.id()), &binding.account_id, job_id, now)?;
            Ok((next.to_owned(), server))
        })?;
        if state == "applying"
            && let Some(flag) = self
                .0
                .cancels
                .lock()
                .map_err(|_| JobError::UNAVAILABLE)?
                .get(job_id)
        {
            flag.store(true, Ordering::Release);
        }
        if let Some(server) = server.and_then(|s| Uuid::parse_str(&s).ok()) {
            match kind {
                Kind::Import => self.remove_staging(server),
                Kind::Create => self.remove_unregistered(server),
            }
        }
        Ok(CancelResponse { state })
    }

    /// Run (or continue) an accepted apply until the copy is verified. Blocking.
    pub fn run_apply(&self, binding: &OwnerBinding, job_id: &str) -> JobResult<ImportJob> {
        let _running = {
            let mut running = self.0.running.lock().map_err(|_| JobError::UNAVAILABLE)?;
            if !running.insert(job_id.to_owned()) {
                return Err(JobError::CONFLICT);
            }
            Running {
                jobs: self,
                id: job_id.to_owned(),
            }
        };
        let flag = Arc::new(AtomicBool::new(false));
        self.0
            .cancels
            .lock()
            .map_err(|_| JobError::UNAVAILABLE)?
            .insert(job_id.to_owned(), flag.clone());
        let (phase, review, private) = self.read(|c| {
            self.owner(c, binding)?;
            let (state, phase, review, private): (String, String, Option<String>, Option<String>) =
                c.query_row(
                    "SELECT state,phase,review,private FROM machine_import_jobs WHERE job_id=?1 AND kind='import'",
                    [job_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?
                .ok_or(JobError::NOT_FOUND)?;
            if state != "applying" {
                return Err(JobError::CONFLICT);
            }
            Ok((
                phase,
                decode::<Review>(review)?,
                decode::<Private>(private)?,
            ))
        })?;
        if matches!(phase.as_str(), "conflicts" | "snapshot" | "copy") {
            match self.copy_phase(job_id, &review, &private, &flag) {
                Ok(()) => {}
                Err(outcome) => {
                    self.settle(job_id, review.destination.server_id, outcome)?;
                    return self.status(binding, job_id);
                }
            }
        }
        if let Err(error) = self.commit(job_id, &review) {
            // The verified copy stays; the owner resolves the error and resumes.
            let now = self.0.host.now_ms();
            self.write(|c| {
                c.execute(
                    "UPDATE machine_import_jobs SET state='interrupted',error=?1,updated_ms=?2 WHERE job_id=?3",
                    params![error.code(), now, job_id],
                )?;
                Ok(())
            })?;
        }
        self.status(binding, job_id)
    }

    fn set_phase(&self, job_id: &str, state: &str, phase: &str) -> JobResult<()> {
        let now = self.0.host.now_ms();
        self.write(|c| {
            c.execute(
                "UPDATE machine_import_jobs SET state=?1,phase=?2,updated_ms=?3 WHERE job_id=?4",
                params![state, phase, now, job_id],
            )?;
            Ok(())
        })
    }

    fn copy_phase(
        &self,
        job_id: &str,
        review: &Review,
        private: &Private,
        cancel: &AtomicBool,
    ) -> Result<(), Outcome> {
        let server_id = review.destination.server_id;
        let reviewed = private
            .snapshot
            .as_ref()
            .ok_or(Outcome::Failed("plan_blocked"))?;
        self.set_phase(job_id, "applying", "conflicts")
            .map_err(Outcome::from)?;
        // The admitted directory must be the reviewed one, not a swapped path.
        if self.0.browser.protected_overlap(&private.source)
            || native::directory_identity(&private.source).ok().as_deref()
                != Some(private.source_identity.as_str())
        {
            return Err(Outcome::Blocked("source_changed"));
        }
        let (processes, state) = self.0.env.processes(std::slice::from_ref(&private.source));
        if processes
            .iter()
            .any(|p| p.relation == "source" && p.state == "running")
        {
            return Err(Outcome::Blocked("source_running"));
        }
        let _ = state; // Owner explicitly confirmed stopped-ness at apply.
        let source_port = inspection::read_properties(&private.source.join("server.properties"))
            .ok()
            .and_then(|p| p.get("server-port").and_then(|v| v.parse().ok()))
            .or(Some(25565));
        let managed = self
            .managed_ports()
            .map_err(|_| Outcome::Interrupted("state_unavailable"))?;
        if let Some(code) = inspection::stopped_evidence(&private.source, source_port, &managed) {
            return Err(Outcome::Blocked(code));
        }
        let port = review
            .resources
            .game_port
            .ok_or(Outcome::Failed("plan_blocked"))?;
        match self.0.env.port_state(port) {
            "free" => {}
            "occupied" => return Err(Outcome::Blocked("game_port_in_use")),
            _ => return Err(Outcome::Blocked("port_state_unknown")),
        }
        // Snapshot again now that the source is confirmed stopped.
        self.set_phase(job_id, "applying", "snapshot")
            .map_err(Outcome::from)?;
        let export = CopiedExport::open(&private.source, &[&self.0.servers])
            .map_err(|e| Outcome::Blocked(snapshot_blocker(&e)))?
            .with_limits(CopyLimits::IMPORT);
        let reviewed_files = reviewed.entries.iter().filter(|e| !e.directory).count() as u64;
        let current = export
            .snapshot_observed(|p| self.report_hashing(job_id, p, reviewed_files, reviewed.bytes))
            .map_err(|e| Outcome::Blocked(snapshot_blocker(&e)))?;
        if current.sha256 != reviewed.sha256 {
            return Err(Outcome::Blocked("source_changed"));
        }
        let free = self.0.env.free_bytes(&self.0.servers).unwrap_or(0);
        let staging = self.server_dir(server_id).join("staging");
        let already = dir_bytes(&staging);
        if free.saturating_add(already) < reviewed.bytes.saturating_add(SPACE_MARGIN_BYTES) {
            return Err(Outcome::Blocked("insufficient_space"));
        }

        self.set_phase(job_id, "applying", "copy")
            .map_err(Outcome::from)?;
        files::create_local_directory(&self.server_dir(server_id), true)
            .and_then(|()| files::create_local_directory(&staging, true))
            .map_err(|_| Outcome::Interrupted("copy_failed"))?;
        remove_parts(&staging);
        let files_total = reviewed.entries.iter().filter(|e| !e.directory).count() as u64;
        self.progress(job_id, CopyProgress::default(), files_total, reviewed.bytes)
            .map_err(Outcome::from)?;
        let mut stop = None;
        let mut index = 0u64;
        let result = export.copy_to_directory(reviewed, &staging, |entry, progress| {
            self.0.env.before_copy_entry(index);
            index += 1;
            if cancel.load(Ordering::Acquire) || self.cancel_requested(job_id) {
                stop = Some(Stop::Cancelled);
                return Err(FileError::Unavailable);
            }
            let remaining = reviewed.bytes.saturating_sub(progress.bytes);
            if !entry.directory
                && self
                    .0
                    .env
                    .free_bytes(&staging)
                    .is_none_or(|free| free < remaining.saturating_add(COPY_RESERVE_BYTES))
            {
                stop = Some(Stop::LowDisk);
                return Err(FileError::InsufficientSpace);
            }
            if index.is_multiple_of(PROGRESS_EVERY) {
                let _ = self.progress(job_id, progress, files_total, reviewed.bytes);
            }
            Ok(())
        });
        match (result, stop) {
            (_, Some(Stop::Cancelled)) => Err(Outcome::Cancelled),
            (_, Some(Stop::LowDisk)) | (Err(FileError::InsufficientSpace), None) => {
                Err(Outcome::Blocked("insufficient_space"))
            }
            (Ok(progress), None) => {
                self.progress(job_id, progress, files_total, reviewed.bytes)
                    .map_err(Outcome::from)?;
                let game = self.server_dir(server_id).join("game");
                if fs::symlink_metadata(&game).is_ok() {
                    return Err(Outcome::Failed("destination_conflict"));
                }
                fs::rename(&staging, &game).map_err(|_| Outcome::Interrupted("copy_failed"))?;
                Ok(())
            }
            (
                Err(
                    FileError::Corrupt
                    | FileError::NotFound
                    | FileError::LinkedPath
                    | FileError::NotRegular
                    | FileError::TooLarge
                    | FileError::ProtectedPath,
                ),
                None,
            ) => Err(Outcome::SourceChangedDuringCopy),
            (Err(_), None) => Err(Outcome::Interrupted("copy_failed")),
        }
    }

    /// Throttled progress while hashing the source (files and bytes read).
    fn report_hashing(&self, job_id: &str, progress: CopyProgress, files: u64, bytes: u64) {
        if progress.files.is_multiple_of(PROGRESS_EVERY) {
            let _ = self.progress(
                job_id,
                progress,
                files.max(progress.files),
                bytes.max(progress.bytes),
            );
        }
    }

    fn cancel_requested(&self, job_id: &str) -> bool {
        self.0
            .host
            .read(|c| {
                Ok(c.query_row(
                    "SELECT cancel_requested FROM machine_import_jobs WHERE job_id=?1",
                    [job_id],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .unwrap_or(false)
    }

    fn progress(
        &self,
        job_id: &str,
        progress: CopyProgress,
        files_total: u64,
        bytes_total: u64,
    ) -> JobResult<()> {
        let now = self.0.host.now_ms();
        self.write(|c| {
            c.execute(
                "UPDATE machine_import_jobs SET files_done=?1,files_total=?2,bytes_done=?3,bytes_total=?4,updated_ms=?5 WHERE job_id=?6",
                params![
                    progress.files as i64,
                    files_total as i64,
                    progress.bytes as i64,
                    bytes_total as i64,
                    now,
                    job_id
                ],
            )?;
            Ok(())
        })
    }

    /// Record a non-success copy outcome and its destination disposition.
    fn settle(&self, job_id: &str, server_id: Uuid, outcome: Outcome) -> JobResult<()> {
        let now = self.0.host.now_ms();
        let (state, error, release) = match outcome {
            Outcome::Cancelled => ("cancelled", None, true),
            Outcome::Blocked(code) => ("blocked", Some(code), true),
            Outcome::SourceChangedDuringCopy => ("blocked", Some("source_changed"), true),
            Outcome::Failed(code) => ("failed", Some(code), true),
            // Keep the reservation and verified partial copy for `resume`.
            Outcome::Interrupted(code) => ("interrupted", Some(code), false),
        };
        match outcome {
            Outcome::Cancelled | Outcome::Blocked("insufficient_space") => {
                self.remove_staging(server_id)
            }
            Outcome::SourceChangedDuringCopy => self.quarantine_staging(server_id, now),
            _ => {}
        }
        self.write(|c| {
            c.execute(
                "UPDATE machine_import_jobs SET state=?1,error=?2,updated_ms=?3 WHERE job_id=?4",
                params![state, error, now, job_id],
            )?;
            if release {
                c.execute(
                    "UPDATE machine_import_reservations SET released=1 WHERE job_id=?1",
                    [job_id],
                )?;
            }
            audit(c, &format!("import.{state}"), "host", job_id, now)?;
            Ok(())
        })
    }

    /// Remove an incomplete, never-registered staging copy under managed storage.
    fn remove_staging(&self, server_id: Uuid) {
        let dir = self.server_dir(server_id);
        if !dir.starts_with(&self.0.servers) || dir == self.0.servers {
            return;
        }
        let _ = fs::remove_dir_all(dir.join("staging"));
        let _ = fs::remove_dir(&dir);
    }
    /// Remove a created server's directory while nothing references it: the
    /// id is not registered, so no worker, file API or launch can use it.
    pub(crate) fn remove_unregistered(&self, server_id: Uuid) {
        let dir = self.server_dir(server_id);
        if !dir.starts_with(&self.0.servers)
            || dir == self.0.servers
            || !matches!(self.0.host.server(server_id), Ok(None))
        {
            return;
        }
        let _ = fs::remove_dir_all(dir);
    }
    fn quarantine_staging(&self, server_id: Uuid, now: i64) {
        let dir = self.server_dir(server_id);
        let _ = fs::rename(dir.join("staging"), dir.join(format!("quarantine-{now}")));
    }
}

/// Non-success copy outcomes.
#[derive(Clone, Copy, Debug)]
enum Outcome {
    Cancelled,
    Blocked(&'static str),
    SourceChangedDuringCopy,
    Failed(&'static str),
    Interrupted(&'static str),
}
impl From<JobError> for Outcome {
    fn from(_: JobError) -> Self {
        Self::Interrupted("state_unavailable")
    }
}

fn snapshot_blocker(error: &FileError) -> &'static str {
    match error {
        FileError::TooLarge => "source_too_large",
        FileError::ProtectedPath
        | FileError::LinkedPath
        | FileError::NotRegular
        | FileError::InvalidRoot
        | FileError::InvalidPath => "unsafe_path",
        _ => "source_unreadable",
    }
}

/// Torn part files from a crashed copy; complete files are re-verified by hash.
fn remove_parts(staging: &Path) {
    let mut stack = vec![staging.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(crate::files::copied_export::PART_SUFFIX))
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}
fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

fn validate_request(request: &PlanRequest) -> JobResult<()> {
    let name = request.name.trim();
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(JobError::INVALID);
    }
    if request.folder_token.is_empty() || request.folder_token.len() > 128 {
        return Err(JobError::INVALID);
    }
    if request.game_port == Some(0) {
        return Err(JobError::INVALID);
    }
    if let Some(version) = &request.minecraft_version {
        let parts: Vec<&str> = version.split('.').collect();
        if version.len() > 16
            || !(2..=3).contains(&parts.len())
            || parts
                .iter()
                .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(JobError::INVALID);
        }
    }
    if let Some(choice) = &request.launch_choice {
        let relative = RelativePath::parse(&choice.relative_path).map_err(|_| JobError::INVALID)?;
        let lower = relative.as_str().to_ascii_lowercase();
        let ok = match choice.kind.as_str() {
            "jar" => lower.ends_with(".jar"),
            "java_argfile" => lower.ends_with("args.txt"),
            _ => false,
        };
        if !ok {
            return Err(JobError::INVALID);
        }
    }
    Ok(())
}

/// Binds actor, source identity/content, destination, launch/runtime,
/// resources and the warning set. EULA acceptance is explicit at apply.
fn plan_hash(binding: &OwnerBinding, review: &Review, private: &Private) -> JobResult<String> {
    let bound = serde_json::json!({
        "host": binding.host_id, "account": binding.account_id, "device": binding.device_id,
        "planId": review.plan_id, "expiresAt": review.expires_at,
        "source": private.source, "identity": private.source_identity,
        "snapshot": private.snapshot.as_ref().map(|s| &s.sha256),
        "destination": review.destination.server_id, "detected": review.detected,
        "runtime": review.runtime, "resources": review.resources,
        "warnings": review.warnings, "blockers": review.blockers,
        "confirmation": review.confirmation,
    });
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(&bound).map_err(|_| JobError::UNAVAILABLE)?,
    )))
}

fn random_token() -> JobResult<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| JobError::UNAVAILABLE)?;
    Ok(B64.encode(bytes))
}
fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
fn decode<T: serde::de::DeserializeOwned>(raw: Option<String>) -> JobResult<T> {
    serde_json::from_str(raw.as_deref().ok_or(JobError::UNAVAILABLE)?)
        .map_err(|_| JobError::UNAVAILABLE)
}

/// Names of registered servers and of unreleased reservations (imports and
/// creates alike) share one case-insensitive namespace.
pub(crate) fn name_in_use(c: &Connection, name: &str) -> JobResult<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM machine_servers WHERE state!='deleted' AND lower(name)=lower(?1))
         OR EXISTS(SELECT 1 FROM machine_import_jobs j JOIN machine_import_reservations r ON r.job_id=j.job_id
                   WHERE r.released=0 AND lower(trim(json_extract(j.request,'$.name')))=lower(?1))",
        [name.trim()],
        |r| r.get(0),
    )?)
}

/// Ports and memory held by registered servers and unreleased import
/// reservations, optionally excluding one job.
impl ImportJobs {
    /// Game ports of every server this host manages (registered, not deleted).
    /// An import source can never be one of these servers, so their ports are
    /// never evidence that a source is still running.
    pub fn managed_ports(&self) -> JobResult<BTreeSet<u16>> {
        self.read(|c| {
            let mut stmt =
                c.prepare("SELECT game_port FROM machine_servers WHERE state!='deleted'")?;
            let ports = stmt
                .query_map([], |r| r.get::<_, u16>(0))?
                .collect::<rusqlite::Result<BTreeSet<_>>>()?;
            Ok(ports)
        })
    }
}

pub(crate) fn reservations_in(
    c: &Connection,
    exclude_job: Option<&str>,
) -> JobResult<Reservations> {
    let mut reservations = Reservations::default();
    let mut stmt = c.prepare("SELECT game_port FROM machine_servers WHERE state!='deleted'")?;
    for port in stmt.query_map([], |r| r.get::<_, u16>(0))? {
        reservations.ports.insert(port?);
    }
    let mut stmt = c.prepare(
        "SELECT game_port,memory_mib FROM machine_import_reservations WHERE released=0 AND job_id!=?1",
    )?;
    for row in stmt.query_map([exclude_job.unwrap_or("")], |r| {
        Ok((r.get::<_, u16>(0)?, r.get::<_, i64>(1)?))
    })? {
        let (port, memory) = row?;
        reservations.ports.insert(port);
        reservations.memory_mib = reservations.memory_mib.saturating_add(memory.max(0) as u64);
    }
    Ok(reservations)
}

fn audit(c: &Connection, action: &str, actor: &str, target: &str, now: i64) -> JobResult<()> {
    c.execute(
        "INSERT INTO machine_audit(action,actor,target,at_ms,revision,auth_generation) SELECT ?1,?2,?3,?4,revision,auth_generation FROM machine_host WHERE singleton=1",
        params![action, actor, format!("import:{target}"), now],
    )?;
    Ok(())
}

pub(crate) fn job_in(c: &Connection, job_id: &str) -> JobResult<ImportJob> {
    job_in_kind(c, job_id, Kind::Import)
}
/// A review that carries a one-time confirmation, shown only while in `review`.
pub(crate) trait Reviewed: serde::de::DeserializeOwned {
    fn clear_confirmation(&mut self);
}
impl Reviewed for Review {
    fn clear_confirmation(&mut self) {
        self.confirmation = None;
    }
}
pub(crate) fn job_in_kind<R: Reviewed>(
    c: &Connection,
    job_id: &str,
    kind: Kind,
) -> JobResult<Job<R>> {
    type Row = (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        [i64; 4],
    );
    let row: Option<Row> = c
        .query_row(
            "SELECT state,phase,error,review,server_id,files_done,files_total,bytes_done,bytes_total FROM machine_import_jobs WHERE job_id=?1 AND kind=?2",
            params![job_id, kind.id()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    [r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?],
                ))
            },
        )
        .optional()?;
    let (state, phase, error, review, server, counts) = row.ok_or(kind.not_found())?;
    let mut review: Option<R> = review.map(|r| decode(Some(r))).transpose()?;
    if state != "review"
        && let Some(review) = &mut review
    {
        review.clear_confirmation();
    }
    let n = |v: i64| v.max(0) as u64;
    Ok(Job {
        job_id: job_id.to_owned(),
        state,
        phase,
        error,
        progress: Progress {
            files_done: n(counts[0]),
            files_total: n(counts[1]),
            bytes_done: n(counts[2]),
            bytes_total: n(counts[3]),
        },
        review,
        server_id: server.and_then(|s| Uuid::parse_str(&s).ok()),
    })
}

impl From<rusqlite::Error> for JobError {
    fn from(_: rusqlite::Error) -> Self {
        JobError::UNAVAILABLE
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS machine_import_jobs(
 job_id TEXT PRIMARY KEY, plan_id TEXT UNIQUE, account_id TEXT NOT NULL, device_id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('planning','review','applying','ready','starting','online','blocked','failed','cancelled','interrupted')),
 phase TEXT NOT NULL, error TEXT, request TEXT NOT NULL, review TEXT, private TEXT,
 server_id TEXT UNIQUE, apply_key TEXT, apply_hash TEXT, eula_accepted_ms INTEGER, eula_account TEXT,
 cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0,1)), start_attempt INTEGER NOT NULL DEFAULT 0,
 files_done INTEGER NOT NULL DEFAULT 0, files_total INTEGER NOT NULL DEFAULT 0,
 bytes_done INTEGER NOT NULL DEFAULT 0, bytes_total INTEGER NOT NULL DEFAULT 0,
 created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS machine_import_apply_key ON machine_import_jobs(account_id,apply_key) WHERE apply_key IS NOT NULL;
CREATE TABLE IF NOT EXISTS machine_import_reservations(server_id TEXT PRIMARY KEY, job_id TEXT NOT NULL UNIQUE,
 game_port INTEGER NOT NULL CHECK(game_port BETWEEN 1 AND 65535), memory_mib INTEGER NOT NULL CHECK(memory_mib>0),
 released INTEGER NOT NULL DEFAULT 0 CHECK(released IN (0,1)));
CREATE UNIQUE INDEX IF NOT EXISTS machine_import_reserved_port ON machine_import_reservations(game_port) WHERE released=0;
CREATE TABLE IF NOT EXISTS machine_create_uploads(upload_id TEXT PRIMARY KEY, account_id TEXT NOT NULL, device_id TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('modpack')), bytes INTEGER NOT NULL, sha256 TEXT NOT NULL,
 created_ms INTEGER NOT NULL, expires_ms INTEGER NOT NULL);
";

#[path = "commit.rs"]
mod commit;
pub use commit::owner_policy;

#[path = "create.rs"]
mod create;
pub use create::{
    CreateApplyRequest, CreateJob, CreateReview, CreateRuntime, CreateSummary, NewServerRequest,
    ServerFacts, SoftwareChoice, SummarySoftware,
};

#[path = "modpack.rs"]
mod modpack;
pub use modpack::{
    CreateUpload, ModpackServerRequest, PackPrivate, PackSourceRequest, UPLOAD_MAX_BYTES,
    UPLOAD_TTL_MS, UploadAccepted,
};

#[path = "restore.rs"]
mod restore;
pub use restore::{
    RestorePrivate, RestoreServerRequest, RestoreSoftware, RestoreSource, RestoreSourceRequest,
};

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
