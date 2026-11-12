//! Trusted host composition from one validated configuration.
//!
//! Opening validates native paths, parses the protected launch definition, opens the
//! canonical database and composes identity, pack, file and operation services. It
//! never binds a listener, bootstraps an owner, mints credentials or starts Minecraft:
//! the operation runtime only executes Start operations that were already admitted
//! and are still authorized. Failures before the runtime starts release everything
//! opened so far; shutdown is observation-only and keeps ownership on timeout.
use crate::ControlError;
use crate::automation::{Automation, AutomationEvent, Severity};
use crate::automation_host::HostActions;
use crate::backups::{BackupEvent, Backups, Environment as BackupEnvironment};
use crate::config::{ConfigError, HostConfiguration, PackAccessMode, PackSource, Supervision};
use crate::files::{self, FileBrowser, FileError};
use crate::identity::management::ManagementAccess;
use crate::identity::minecraft::{MinecraftChallenges, MinecraftError, ServerUuid};
use crate::identity::{IdentityService, PackPasswords};
use crate::minecraft_admission::{AdmissionDataError, BanList, ServerProperties};
use crate::operations::{OperationError, OperationRuntime, Operations, ShutdownOutcome};
use crate::pack_access::{PackAccessPolicy, PackSessions, SessionLimits};
use crate::packs::{
    MAX_PINNED_BYTES, MAX_PINNED_FILE_BYTES, MAX_SNAPSHOT_FILES, MAX_SNAPSHOT_SCAN, PackError,
    PackPreparation, PackReleases, PreparedPack, ReleaseError, ReleasePlan, SnapshotLimits,
    VestaBuildError,
};
use crate::whitelist::{Whitelist, WhitelistError};
use std::{
    fmt,
    path::Path,
    sync::{Arc, Mutex, RwLock, mpsc},
    thread,
    time::Duration,
};
use vesta_game_runtime::launch_config::{LaunchConfigError, LaunchConfiguration, MAX_CONFIG_BYTES};
use vesta_game_runtime::process::{GracefulStop, LaunchSpec};
use vesta_protocol::discovery::{
    self, DiscoveryRequest, Feature, Manifest, ProtocolError, Support,
};
use vesta_state::{Clock, StateDatabase, StateError};

const WHITELIST_FILE: &str = "whitelist.json";
const PROPERTIES_FILE: &str = "server.properties";
const BANS_FILE: &str = "banned-players.json";

#[derive(Clone, Eq, PartialEq)]
pub enum HostError {
    Config(ConfigError),
    Launch(LaunchConfigError),
    LaunchDocument(FileError),
    /// Launch cwd is not the configured game root, overlaps state, or cannot be
    /// safely observed. No native path or OS diagnostic is exposed.
    LaunchDirectory,
    State(StateError),
    Identity(ControlError),
    Minecraft(MinecraftError),
    Whitelist(WhitelistError),
    Admission(AdmissionDataError),
    Files(FileError),
    Pack(PackError),
    PackBuild(VestaBuildError),
    Release(ReleaseError),
    Operation(OperationError),
    /// Supervision mode and the supplied process owner disagree, or the supervisor
    /// is bound to a different launch definition. Fixed codes only.
    Supervisor(&'static str),
}
impl HostError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Config(error) => error.code(),
            Self::Launch(error) => error.code(),
            Self::LaunchDocument(_) => "host_launch_definition_unavailable",
            Self::LaunchDirectory => "host_launch_directory_invalid",
            Self::State(error) => error.code(),
            Self::Identity(error) => error.code(),
            Self::Minecraft(error) => error.code(),
            Self::Whitelist(error) => error.code(),
            Self::Admission(error) => error.code(),
            Self::Files(error) => error.code(),
            Self::Pack(error) => error.code(),
            Self::PackBuild(error) => error.code(),
            Self::Release(error) => error.code(),
            Self::Operation(error) => error.code(),
            Self::Supervisor(code) => code,
        }
    }
}
impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl fmt::Debug for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostError")
            .field("code", &self.code())
            .finish_non_exhaustive()
    }
}
impl std::error::Error for HostError {}

/// The composed host. Services share the one canonical database handle.
pub struct Host {
    config: HostConfiguration,
    db: StateDatabase,
    identity: IdentityService,
    management: ManagementAccess,
    passwords: Option<PackPasswords>,
    minecraft: MinecraftChallenges,
    server: ServerUuid,
    admission: AdmissionData,
    sessions: Arc<PackSessions>,
    network_admission: Arc<RwLock<AdmissionData>>,
    files: FileBrowser,
    preparations: Arc<Vec<PackPreparation>>,
    releases: Arc<PackReleases>,
    graceful_stop: GracefulStop,
    // Stop the coordinator before the runtime's observation-only destructor can
    // wait for a locally owned child.
    sleep_driver: Option<crate::sleep::Driver>,
    software: Option<crate::software::SoftwareService>,
    sftp: Arc<crate::sftp::SftpService>,
    resource_packs: Option<crate::resource_packs::hosting::Service>,
    runtime: OperationRuntime,
    backups: Option<Arc<Backups>>,
    automation: Arc<Automation>,
    driver: Option<AutomationDriver>,
    sleep: crate::sleep::Sleep,
}

struct DriverTelemetry {
    game_directory: std::path::PathBuf,
    metrics: crate::metrics::Sampler,
    notifications: crate::external_alerts::Settings,
}

/// Ticks schedules and turns runtime and backup facts into automation events.
/// Observation only: it never starts or stops anything itself, except the opt-in
/// start-on-agent-start request through the runtime's normal start path.
struct AutomationDriver {
    stop: Arc<std::sync::atomic::AtomicBool>,
    local: Arc<std::sync::OnceLock<crate::operations::LocalRuntime>>,
    thread: Option<thread::JoinHandle<()>>,
    alerts: Option<crate::external_alerts::Worker>,
}
impl AutomationDriver {
    fn start(
        automation: Arc<Automation>,
        runtime: Arc<std::sync::OnceLock<crate::operations::RuntimeHandle>>,
        backup_events: mpsc::Receiver<BackupEvent>,
        startup: (crate::config::OnAgentStart, crate::sleep::Sleep),
        actions: Arc<HostActions>,
        telemetry: DriverTelemetry,
    ) -> Result<Self, HostError> {
        let DriverTelemetry {
            game_directory,
            mut metrics,
            notifications,
        } = telemetry;
        let alerts =
            crate::external_alerts::Worker::start(automation.database_for_alerts(), notifications)
                .map_err(HostError::State)?;
        use std::sync::atomic::{AtomicBool, Ordering};
        let stop = Arc::new(AtomicBool::new(false));
        let local: Arc<std::sync::OnceLock<crate::operations::LocalRuntime>> = Arc::default();
        let (flag, attached) = (stop.clone(), local.clone());
        let thread = thread::Builder::new()
            .name("vesta-automation".into())
            .spawn(move || {
                // Wait for the runtime; composition fills these right after opening it.
                let (runtime, local) = loop {
                    if flag.load(Ordering::Acquire) {
                        return;
                    }
                    if let (Some(runtime), Some(local)) = (runtime.get(), attached.get()) {
                        break (runtime.clone(), local.clone());
                    }
                    thread::sleep(Duration::from_millis(20));
                };
                let _ = automation.publish(AutomationEvent::AgentStarted);
                let mut joins = crate::automation::joins::JoinTracker::new(game_directory.clone(), &runtime);
                let mut generation = runtime.game_snapshot().ok().and_then(|s| s.generation);
                let startup_policy = startup.1.on_agent_start(startup.0).unwrap_or(crate::config::OnAgentStart::StayStopped);
                let mut startup_allowed = true;
                if generation.is_none() && let Err(code) = startup.1.initialize(startup_policy) {
                    startup_allowed = false;
                    let _ = startup.1.initialize(crate::config::OnAgentStart::StayStopped);
                    let _ = automation.notify(Severity::Warning, "Agent startup policy refused", code, "agent");
                }
                if startup_allowed && startup_policy == crate::config::OnAgentStart::Start && generation.is_none() {
                    // The runtime's own start path refuses duplicates, orphans,
                    // unresolved launches and maintenance blocks; its outcome is audited.
                    if let Err(code) = local.request(crate::operations::LocalAction::Start) {
                        let _ = automation.notify(
                            Severity::Warning,
                            "Start on agent start refused",
                            code,
                            "agent",
                        );
                    }
                }
                let mut crash_loop = false;
                while !flag.load(Ordering::Acquire) {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    metrics.tick(&runtime, now);
                    joins.tick(&runtime, &automation);
                    let _ = automation.tick(now);
                    let _ = actions.expire_bans(&game_directory);
                    let _ = actions.sync_whitelist(&game_directory);
                    if let Ok(failed) = actions.tick_startup_whitelist(&runtime) {
                        for player in failed {
                            let _ = automation.notify(Severity::Warning,"Whitelist change needs attention",&format!("Could not verify the queued whitelist change for {player}. Review the whitelist and retry."),"players");
                        }
                    }
                    while let Ok(event) = backup_events.try_recv() {
                        let _ = automation.publish(match event {
                            BackupEvent::Completed { profile, .. } => {
                                AutomationEvent::BackupCompleted { profile }
                            }
                            BackupEvent::Failed { profile, .. } => {
                                AutomationEvent::BackupFailed { profile }
                            }
                        });
                    }
                    if let Ok(Some(health)) = runtime.supervision() {
                        if health.crash_loop && !crash_loop {
                            let _ = automation.notify(
                                Severity::Critical,
                                "Crash loop: automatic restarts suspended",
                                "The server crashed repeatedly and was left stopped. Investigate, then run vesta-agent reset-restarts.",
                                "supervisor",
                            );
                        }
                        crash_loop = health.crash_loop;
                    }
                    if let Ok(snapshot) = runtime.game_snapshot() {
                        if let Some(old) = generation
                            && snapshot.generation != Some(old)
                        {
                            // The old generation ended, even if a new one already started
                            // between observations (for example a crash restart).
                            let crashed = snapshot
                                .last_exit
                                .is_some_and(|exit| exit.generation == old && !exit.success);
                            let _ = automation.publish(if crashed {
                                AutomationEvent::ServerCrashed
                            } else {
                                AutomationEvent::ServerStopped
                            });
                        }
                        if snapshot.generation.is_some() && snapshot.generation != generation {
                            let _ = automation.publish(AutomationEvent::ServerStarted);
                        }
                        generation = snapshot.generation;
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            })
            .map_err(|_| HostError::Supervisor("host_automation_unavailable"))?;
        Ok(Self {
            stop,
            local,
            thread: Some(thread),
            alerts,
        })
    }
    fn attach(&self, local: crate::operations::LocalRuntime) {
        let _ = self.local.set(local);
    }
    fn stop(mut self) {
        self.halt();
    }
    fn halt(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.alerts.take();
    }
}
impl Drop for AutomationDriver {
    fn drop(&mut self) {
        self.halt();
    }
}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Host")
            .field("pack_access", &self.config.pack_access())
            .field("scan_roots", &self.preparations.len())
            .field("runtime", &self.runtime)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum HostShutdown {
    Complete,
    /// The runtime still owns live work; the host is returned so the caller keeps
    /// the database, driver and native handles attached and can observe again later.
    TimedOut(Box<Host>),
}

impl Host {
    pub fn open(config: HostConfiguration, clock: Arc<dyn Clock>) -> Result<Self, HostError> {
        Self::open_with(config, clock, None)
    }

    /// Open with the configured process owner. `Supervision::Separate` requires an
    /// attached supervisor bound to this configuration's exact launch definition;
    /// the in-process default refuses one. Opening never launches the game.
    pub fn open_with(
        config: HostConfiguration,
        clock: Arc<dyn Clock>,
        supervisor: Option<vesta_supervisor::RemoteProcess>,
    ) -> Result<Self, HostError> {
        let checked = check_launch(&config)?;
        let owner = match (config.supervision(), supervisor) {
            (Supervision::InProcess, None) => {
                // A game left running by a supervisor (or an unresolved supervised
                // launch) must never get a second, agent-owned launch beside it.
                if checked.state.path().is_dir()
                    && vesta_supervisor::instance_busy(&checked.state)
                        .map_err(HostError::Supervisor)?
                {
                    return Err(HostError::Supervisor("host_supervised_game_running"));
                }
                None
            }
            (Supervision::Separate, Some(remote)) => {
                let (state, identity) = remote.binding();
                let snapshot = remote.snapshot();
                let health = remote.health();
                let recovery = identity != checked.identity
                    && snapshot.generation.is_none()
                    && snapshot.phase == vesta_game_runtime::process::ProcessPhase::Stopped
                    && !health.orphan_running
                    && !health.launch_unresolved
                    && !health.observation_failed
                    && health.restart_pending_ms.is_none()
                    && software_recovery_binding(&config, identity)?;
                if state != checked.state.path().to_string_lossy()
                    || identity != checked.identity && !recovery
                {
                    return Err(HostError::Supervisor("host_supervisor_binding_mismatch"));
                }
                // The running supervisor adopts this configuration's restart policy, so
                // removing or changing [process.restart] takes effect on agent start.
                remote
                    .configure(restart_policy(&config)?)
                    .map_err(HostError::Supervisor)?;
                Some(remote)
            }
            (Supervision::Separate, None) => {
                return Err(HostError::Supervisor("host_supervisor_required"));
            }
            (Supervision::InProcess, Some(_)) => {
                return Err(HostError::Supervisor("host_supervisor_not_configured"));
            }
        };
        let db = StateDatabase::open(checked.state, clock).map_err(HostError::State)?;
        match compose(config, &db, checked.launch, owner) {
            Ok(host) => Ok(host),
            Err(error) => {
                // Nothing owned live work yet; closing cannot discard a running child.
                let _ = db.close();
                Err(error)
            }
        }
    }

    pub fn configuration(&self) -> &HostConfiguration {
        &self.config
    }
    pub fn database(&self) -> &StateDatabase {
        &self.db
    }
    pub fn management(&self) -> &ManagementAccess {
        &self.management
    }
    pub fn identity(&self) -> &IdentityService {
        &self.identity
    }
    /// Present only when the configured pack-access mode admits passwords.
    pub fn pack_passwords(&self) -> Option<&PackPasswords> {
        self.passwords.as_ref()
    }
    pub fn minecraft(&self) -> &MinecraftChallenges {
        &self.minecraft
    }
    /// Loaded at startup; operator edits need an explicit `reload_admission`.
    pub fn whitelist(&self) -> &Whitelist {
        &self.admission.whitelist
    }
    /// `None` when `server.properties` is absent: Minecraft policy then fails closed.
    pub fn server_properties(&self) -> Option<ServerProperties> {
        self.admission.properties
    }
    pub fn bans(&self) -> &BanList {
        &self.admission.bans
    }
    /// Reloads the server's whitelist, properties and bans together; a refused reload
    /// keeps the previous data in full.
    pub fn reload_admission(&mut self) -> Result<(), HostError> {
        let admission = AdmissionData::load(&self.config)?;
        *self
            .network_admission
            .write()
            .map_err(|_| HostError::Admission(AdmissionDataError::Unavailable))? =
            admission.clone();
        self.admission = admission;
        Ok(())
    }
    pub fn pack_sessions(&self) -> &PackSessions {
        &self.sessions
    }
    /// Local admission observation (explicitly refreshed by `reload_admission`).
    /// Network authorization instead uses `NetworkHost::with_policy`, which
    /// establishes current admission files independently on every decision.
    pub fn pack_policy(&self) -> Result<PackAccessPolicy<'_>, HostError> {
        let password = match &self.passwords {
            Some(passwords) => passwords.active_revision().map_err(HostError::Identity)?,
            None => None,
        };
        Ok(PackAccessPolicy {
            server: self.server,
            mode: self.config.pack_access(),
            properties: self.admission.properties,
            whitelist: &self.admission.whitelist,
            bans: &self.admission.bans,
            password,
        })
    }
    pub fn files(&self) -> &FileBrowser {
        &self.files
    }
    /// Selected local source preparation, or legacy scan roots in configuration order.
    /// Platform-reference mode composes no local pack reader.
    pub fn pack_preparations(&self) -> &[PackPreparation] {
        &self.preparations
    }
    /// Explicit backend action only. Opening a Host never scans, reads archives,
    /// fetches a provider, publishes a release or grants pack access.
    /// Legacy documents can use this API only with one unambiguous scan root.
    pub fn prepare_pack(
        &self,
        plan: &ReleasePlan,
        sources: &vesta_protocol::pack::sources::SourceRegistry,
    ) -> Result<PreparedPack, HostError> {
        let metadata = plan
            .manifest_metadata(self.config.pack())
            .map_err(HostError::Release)?;
        match self.config.pack_source() {
            Some(PackSource::Platform(reference)) => {
                PreparedPack::platform(reference.clone(), metadata).map_err(HostError::Pack)
            }
            Some(PackSource::Archive { path, .. }) => {
                let archive = self.preparations[0]
                    .archive(path)
                    .map_err(HostError::Pack)?;
                PreparedPack::archive(archive, metadata).map_err(HostError::Pack)
            }
            Some(PackSource::Scanned { .. }) | None => {
                if self.preparations.len() != 1 {
                    return Err(HostError::Pack(PackError::InvalidPath));
                }
                let snapshot = self.preparations[0]
                    .snapshot(metadata, sources)
                    .map_err(HostError::Pack)?;
                PreparedPack::scanned(snapshot, &plan.pack_metadata(self.config.pack()))
                    .map_err(HostError::PackBuild)
            }
        }
    }

    pub fn releases(&self) -> &PackReleases {
        &self.releases
    }
    pub fn runtime(&self) -> &OperationRuntime {
        &self.runtime
    }

    /// Mechanisms this host composed, bound to the canonical server ID. Describes, never
    /// authorizes: a supported feature is not a permission, readiness or a live process.
    pub fn discover(&self, request: &DiscoveryRequest) -> Result<Manifest, ProtocolError> {
        discover_components(self.db.server_id(), self.graceful_stop, request)
    }

    /// Cloneable device-session management facade; runtime admission is available,
    /// but OS-local bypass authority and native process ownership are not exposed.
    pub fn network_handle(&self) -> NetworkHost {
        NetworkHost {
            config: self.config.clone(),
            db: self.db.clone(),
            management: self.management.clone(),
            passwords: self.passwords.clone(),
            minecraft: self.minecraft.clone(),
            server: self.server,
            admission: self.network_admission.clone(),
            sessions: self.sessions.clone(),
            releases: self.releases.clone(),
            graceful_stop: self.graceful_stop,
            runtime: self.runtime().network_handle(),
            preparations: self.preparations.clone(),
            files: self.files.for_remote(),
            backups: self.backups.clone(),
            automation: self.automation.clone(),
            sleep: self.sleep.clone(),
            software: self
                .software
                .as_ref()
                .map(crate::software::SoftwareService::handle),
            sftp: self.sftp.clone(),
            resource_packs: self
                .resource_packs
                .as_ref()
                .expect("host services open")
                .handle(),
        }
    }
    /// Present only when `[backups]` is configured.
    pub fn backups(&self) -> Option<&Arc<Backups>> {
        self.backups.as_ref()
    }
    pub fn automation(&self) -> &Arc<Automation> {
        &self.automation
    }

    /// Stop dispatch and wait up to `timeout` for the runtime to finish. The database
    /// is closed only after the runtime has really completed.
    pub fn shutdown(mut self, timeout: Duration) -> Result<HostShutdown, HostError> {
        drop(self.sleep_driver.take());
        if let Some(driver) = self.driver.take() {
            driver.stop();
        }
        // A running backup is waited for, never abandoned mid-write.
        if let Some(backups) = &self.backups {
            backups.stop_claiming();
            if backups.busy() {
                thread::sleep(timeout.min(Duration::from_millis(500)));
                if backups.busy() {
                    return Ok(HostShutdown::TimedOut(Box::new(self)));
                }
            }
        }
        drop(self.resource_packs.take());
        drop(self.software.take());
        match self
            .runtime
            .shutdown(timeout)
            .map_err(HostError::Operation)?
        {
            ShutdownOutcome::Complete => {
                self.db.close().map_err(HostError::State)?;
                Ok(HostShutdown::Complete)
            }
            ShutdownOutcome::TimedOut(_) => Ok(HostShutdown::TimedOut(Box::new(self))),
        }
    }
}

fn compose(
    config: HostConfiguration,
    db: &StateDatabase,
    launch: LaunchConfiguration,
    supervisor: Option<vesta_supervisor::RemoteProcess>,
) -> Result<Host, HostError> {
    let protected: Vec<&Path> = config
        .host_protected_paths()
        .iter()
        .map(|path| path.as_path())
        .collect();
    let identity = IdentityService::new(db.clone()).map_err(HostError::Identity)?;
    let management = ManagementAccess::new(db.clone()).map_err(|_| {
        HostError::Identity(crate::ControlError::State(vesta_state::StateError::new(
            "management_unavailable",
        )))
    })?;
    let passwords = match config.pack_access() {
        PackAccessMode::Minecraft => None,
        PackAccessMode::MinecraftOrPassword => {
            Some(PackPasswords::new(db.clone()).map_err(HostError::Identity)?)
        }
    };
    let server = ServerUuid::parse(db.server_id()).map_err(HostError::Minecraft)?;
    let minecraft = MinecraftChallenges::new(server).map_err(HostError::Minecraft)?;
    let admission = AdmissionData::load(&config)?;
    let sessions = Arc::new(PackSessions::new(
        management.clone(),
        SessionLimits::default(),
    ));
    let files =
        FileBrowser::open(db.clone(), config.game(), &protected).map_err(HostError::Files)?;
    files.reconcile_pending().map_err(HostError::Files)?;
    files.purge_expired_uploads().map_err(HostError::Files)?;
    let limits = SnapshotLimits::new(
        MAX_PINNED_FILE_BYTES,
        MAX_PINNED_BYTES,
        MAX_SNAPSHOT_FILES,
        MAX_SNAPSHOT_SCAN,
    )
    .map_err(HostError::Pack)?;
    let roots = match config.pack_source() {
        Some(source) => source.root().into_iter().collect::<Vec<_>>(),
        None => config
            .scan_roots()
            .iter()
            .map(|root| root.as_path())
            .collect(),
    };
    let preparations = roots
        .into_iter()
        .map(|root| {
            let mut preparation = PackPreparation::open(db, root, &protected, limits)?
                .with_unresolved_disposition(config.pack().unresolved_disposition());
            if let Some(PackSource::Scanned {
                directories: Some(directories),
                ..
            }) = config.pack_source()
            {
                preparation = preparation.with_selected_directories(directories.clone());
            }
            Ok(preparation)
        })
        .collect::<Result<Vec<_>, PackError>>()
        .map_err(HostError::Pack)?;
    let releases = Arc::new(PackReleases::open(db).map_err(HostError::Release)?);
    let graceful_stop = launch.graceful_stop();
    let sftp = crate::sftp::SftpService::open(db.clone(), files.clone(), management.clone())
        .map_err(|_| HostError::Supervisor("sftp_state_unavailable"))?;
    let spec = config
        .isolate_launch(launch.into_launch_spec(None).map_err(HostError::Launch)?)
        .map_err(|e| HostError::Launch(LaunchConfigError::Preflight(e)))?;
    let tick_detection = crate::metrics::ticks::detect(&spec, config.game());
    let operations = Operations::new(db.clone())
        .map_err(HostError::Operation)?
        .with_content(files.clone());
    // The status probe target comes from server.properties at open time; a changed
    // port is picked up when the agent is restarted.
    let readiness = vesta_game_runtime::readiness::ReadinessConfig {
        probe: files::read_local_document(&config.game().join(PROPERTIES_FILE), 64 * 1024)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| crate::operations::readiness::probe_from_properties(&text)),
        ..Default::default()
    };
    let supervised = supervisor.is_some();
    // Everything fallible is built before the runtime starts, because once it runs a
    // persisted Start may launch the game and dropping the runtime would wait for it.
    // The services reach the runtime through a cell filled right after it opens.
    let cell: Arc<std::sync::OnceLock<crate::operations::RuntimeHandle>> = Arc::default();
    let gate = crate::operations::StartGate::default();
    let startup_gate = gate.block();
    let bind_address = files::read_local_document(&config.game().join(PROPERTIES_FILE), 64 * 1024)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| crate::sleep::bind_from_properties(&text));
    let sleep = crate::sleep::Sleep::new(
        db.clone(),
        gate.clone(),
        bind_address,
        readiness.probe,
        config.pack().name().into(),
    )
    .with_metadata(
        config.pack().minecraft_version(),
        config.pack().version(),
        config.pack().loader().map_or("vanilla", |l| l.version()),
    );
    let sleep_driver = sleep
        .start_driver(cell.clone())
        .map_err(HostError::Supervisor)?;
    let (event_sink, backup_events) = mpsc::channel();
    let default_backups = crate::config::BackupSettings::for_instance(config.base());
    let backups = {
        let settings = config.backups().unwrap_or(&default_backups);
        let observed = cell.clone();
        let event_sink = Mutex::new(event_sink);
        let state = db.directory().clone();
        Some(Arc::new(
            Backups::open(
                db.clone(),
                settings.clone(),
                config.game(),
                BackupEnvironment {
                    observe: Box::new(move || {
                        let runtime = observed.get().ok_or("backup_runtime_unavailable")?;
                        let snapshot = runtime
                            .game_snapshot()
                            .map_err(|_| "backup_game_state_unproven")?;
                        if let Some(health) = runtime
                            .supervision()
                            .map_err(|_| "backup_game_state_unproven")?
                            && (health.orphan_running
                                || health.launch_unresolved
                                || health.observation_failed)
                        {
                            return Err("backup_game_state_unproven");
                        }
                        Ok(snapshot.generation)
                    }),
                    supervisor: if supervised {
                        vesta_supervisor::SupervisorClient::new(&state).ok()
                    } else {
                        None
                    },
                    start_gate: gate.clone(),
                    events: Box::new(move |event| {
                        if let Ok(sink) = event_sink.lock() {
                            let _ = sink.send(event);
                        }
                    }),
                    protected: config.host_protected_paths().to_vec(),
                },
            )
            .map_err(|e| HostError::Supervisor(e.code()))?,
        ))
    };
    let actions = Arc::new(HostActions::new(
        db.clone(),
        cell.clone(),
        backups.clone(),
        files.clone(),
    ));
    let automation =
        Arc::new(Automation::new(db.clone(), actions.clone()).map_err(HostError::Identity)?);
    let driver = AutomationDriver::start(
        automation.clone(),
        cell.clone(),
        backup_events,
        (config.on_agent_start(), sleep.clone()),
        actions,
        DriverTelemetry {
            game_directory: config.game().to_path_buf(),
            metrics: crate::metrics::Sampler::new(
                db.clone(),
                config.metrics(),
                config.game().to_path_buf(),
                config.backups().and_then(|b| match &b.repository {
                    crate::config::BackupRepository::Local(path) => Some(path.clone()),
                    crate::config::BackupRepository::S3(_) => None,
                }),
            )
            .with_tick_detection(tick_detection),
            notifications: config.notifications().clone(),
        },
    )?;
    // Last: once the driver runs, failures must go through observation-only shutdown.
    let runtime = match supervisor {
        None => OperationRuntime::open_with_readiness(operations, spec, readiness, gate),
        Some(remote) => OperationRuntime::open_supervised(operations, remote, readiness, gate),
    }
    .map_err(HostError::Operation)?;
    let _ = cell.set(runtime.network_handle());
    let maintenance = Arc::new(tokio::sync::Mutex::new(()));
    let software = crate::host_software::open(
        &config,
        db,
        runtime.network_handle(),
        runtime.local_handle(),
        maintenance.clone(),
    )
    .map_err(|_| HostError::Supervisor("software_state_unavailable"))?;
    let resource_packs = crate::resource_packs::hosting::Service::open(
        db.clone(),
        Arc::new(crate::resource_packs::hosting::native::NativeHooks {
            recovery: Mutex::new(None),
            maintenance,
            db: db.clone(),
            files: files.for_remote(),
            runtime: runtime.network_handle(),
            software: software
                .as_ref()
                .map(crate::software::SoftwareService::handle),
            fallback_minecraft_version: config.pack().minecraft_version().into(),
            game: config.game().to_path_buf(),
        }),
    )
    .map_err(|_| HostError::Supervisor("resource_pack_state_unavailable"))?;
    driver.attach(runtime.local_handle());
    drop(startup_gate);
    Ok(Host {
        config,
        db: db.clone(),
        identity,
        management,
        passwords,
        minecraft,
        server,
        network_admission: Arc::new(RwLock::new(admission.clone())),
        admission,
        sessions,
        files,
        preparations: Arc::new(preparations),
        releases,
        graceful_stop,
        software,
        resource_packs: Some(resource_packs),
        sftp,
        runtime,
        backups,
        automation,
        driver: Some(driver),
        sleep,
        sleep_driver: Some(sleep_driver),
    })
}

struct CheckedLaunch {
    state: vesta_state::StateDirectory,
    launch: LaunchConfiguration,
    identity: String,
}

/// Shared by the agent and the supervisor: identical path, overlap and cwd checks.
fn check_launch(config: &HostConfiguration) -> Result<CheckedLaunch, HostError> {
    config.validate_host_paths().map_err(HostError::Config)?;
    let bytes = files::read_local_document(config.launch_definition(), MAX_CONFIG_BYTES)
        .map_err(HostError::LaunchDocument)?;
    let launch =
        LaunchConfiguration::parse_toml(&bytes, config.base()).map_err(HostError::Launch)?;
    // Both checks observe only: refuse before database preparation or ownership.
    // Retain the launch cwd in StateDirectory's later overlap rechecks as well.
    let state = vesta_state::StateDirectory::new(config.state(), Some(launch.working_directory()))
        .map_err(|_| HostError::LaunchDirectory)?;
    if !files::same_directory(config.game(), launch.working_directory())
        .map_err(|_| HostError::LaunchDirectory)?
    {
        return Err(HostError::LaunchDirectory);
    }
    let identity =
        crate::host_software::identity(config, &bytes, launch.working_directory(), state.path());
    Ok(CheckedLaunch {
        state,
        launch,
        identity,
    })
}

/// What a separate supervisor needs from a validated host configuration: its state
/// directory, the trusted launch specification and the launch-definition identity.
pub struct SupervisedLaunch {
    pub state: vesta_state::StateDirectory,
    pub spec: LaunchSpec,
    pub identity: String,
    pub restart: Option<vesta_supervisor::RestartPolicy>,
}
impl fmt::Debug for SupervisedLaunch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SupervisedLaunch")
    }
}

/// The configured crash-restart policy, validated; `None` when disabled.
fn restart_policy(
    config: &HostConfiguration,
) -> Result<Option<vesta_supervisor::RestartPolicy>, HostError> {
    config
        .restart()
        .map(|r| {
            let s = |n: u32| std::time::Duration::from_secs(u64::from(n));
            vesta_supervisor::RestartPolicy::new(
                r.crash_limit,
                s(r.crash_window_minutes.saturating_mul(60)),
                s(r.backoff_seconds),
                s(r.max_backoff_seconds),
            )
        })
        .transpose()
        .map_err(HostError::Supervisor)
}

/// Read-only startup preflight, including the executable and required launch files.
/// Does not open a database, acquire runtime ownership or start any process.
pub fn validate_launch(config: &HostConfiguration) -> Result<(), HostError> {
    let checked = check_launch(config)?;
    let spec = checked
        .launch
        .into_launch_spec(None)
        .map_err(HostError::Launch)?;
    config
        .isolate_launch(spec)
        .map_err(|e| HostError::Launch(LaunchConfigError::Preflight(e)))?;
    Ok(())
}

/// Validate a configuration for supervision without opening the database.
pub fn supervised_launch(config: &HostConfiguration) -> Result<SupervisedLaunch, HostError> {
    if config.supervision() != Supervision::Separate {
        return Err(HostError::Supervisor("host_supervisor_not_configured"));
    }
    let restart = restart_policy(config)?;
    let checked = check_launch(config)?;
    Ok(SupervisedLaunch {
        restart,
        state: checked.state,
        spec: config
            .isolate_launch(
                checked
                    .launch
                    .into_launch_spec(None)
                    .map_err(HostError::Launch)?,
            )
            .map_err(|e| HostError::Launch(LaunchConfigError::Preflight(e)))?,
        identity: checked.identity,
    })
}
/// Observation-only admission for a journaled, stopped ambiguous software switch.
pub fn software_recovery_binding(
    config: &HostConfiguration,
    identity: &str,
) -> Result<bool, HostError> {
    crate::host_software::recovery_binding(config, identity)
}

#[derive(Clone)]
struct AdmissionData {
    whitelist: Whitelist,
    properties: Option<ServerProperties>,
    bans: BanList,
}
impl AdmissionData {
    fn load(config: &HostConfiguration) -> Result<Self, HostError> {
        let game = config.game();
        let properties = match ServerProperties::load(&game.join(PROPERTIES_FILE)) {
            Ok(properties) => Some(properties),
            // Absent before the first server run; Minecraft policy fails closed meanwhile.
            Err(AdmissionDataError::Unavailable) => None,
            Err(error) => return Err(HostError::Admission(error)),
        };
        Ok(Self {
            whitelist: Whitelist::load(&game.join(WHITELIST_FILE)).map_err(HostError::Whitelist)?,
            properties,
            bans: BanList::load(&game.join(BANS_FILE)).map_err(HostError::Admission)?,
        })
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;

fn discover_components(
    server_id: &str,
    graceful_stop: GracefulStop,
    request: &DiscoveryRequest,
) -> Result<Manifest, ProtocolError> {
    let version = discovery::negotiate(request)?;
    let feature = |id: &str, unsupported: Option<&str>| {
        Feature::new(
            id.to_owned(),
            if unsupported.is_some() {
                Support::Unsupported
            } else {
                Support::Supported
            },
            unsupported.map(str::to_owned),
        )
    };
    let stop = match graceful_stop {
        GracefulStop::MinecraftCommand => None,
        GracefulStop::Unsupported => Some("unsupported_stop"),
    };
    Manifest::new(
        version,
        server_id.to_owned(),
        vec![
            feature("game.start", None)?,
            feature("game.stop", stop)?,
            feature("game.force_kill", None)?,
            feature("tasks.replay", None)?,
            feature("game.readiness", Some("not_implemented"))?,
            feature("supervisor.continuity", Some("not_implemented"))?,
        ],
    )
}

/// Cloneable management/pack facade; never exposes OS-local owner authority.
#[derive(Clone)]
pub struct NetworkHost {
    resource_packs: crate::resource_packs::hosting::Handle,
    software: Option<crate::software::SoftwareHandle>,
    sftp: Arc<crate::sftp::SftpService>,
    management: ManagementAccess,
    config: HostConfiguration,
    db: StateDatabase,
    passwords: Option<PackPasswords>,
    minecraft: MinecraftChallenges,
    server: ServerUuid,
    admission: Arc<RwLock<AdmissionData>>,
    sessions: Arc<PackSessions>,
    releases: Arc<PackReleases>,
    graceful_stop: GracefulStop,
    runtime: crate::operations::RuntimeHandle,
    preparations: Arc<Vec<PackPreparation>>,
    files: FileBrowser,
    backups: Option<Arc<Backups>>,
    automation: Arc<Automation>,
    sleep: crate::sleep::Sleep,
}
/// Owned local refresh composition. Dropping it signals and joins the worker,
/// including any in-progress bounded document read. No remote management route.
pub struct AdmissionRefresh {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Drop for AdmissionRefresh {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl NetworkHost {
    pub fn resource_packs(&self) -> &crate::resource_packs::hosting::Handle {
        &self.resource_packs
    }
    pub fn software(&self) -> Option<&crate::software::SoftwareHandle> {
        self.software.as_ref()
    }
    pub fn sftp(&self) -> &Arc<crate::sftp::SftpService> {
        &self.sftp
    }
    pub fn sleep(&self) -> &crate::sleep::Sleep {
        &self.sleep
    }
    pub fn backups(&self) -> Option<&Arc<Backups>> {
        self.backups.as_ref()
    }
    pub fn automation(&self) -> &Arc<Automation> {
        &self.automation
    }
    pub fn prepare_pack(
        &self,
        plan: &ReleasePlan,
        sources: &vesta_protocol::pack::sources::SourceRegistry,
    ) -> Result<PreparedPack, HostError> {
        let metadata = plan
            .manifest_metadata(self.config.pack())
            .map_err(HostError::Release)?;
        match self.config.pack_source() {
            Some(PackSource::Platform(reference)) => {
                PreparedPack::platform(reference.clone(), metadata).map_err(HostError::Pack)
            }
            Some(PackSource::Archive { path, .. }) => {
                let archive = self.preparations[0]
                    .archive(path)
                    .map_err(HostError::Pack)?;
                PreparedPack::archive(archive, metadata).map_err(HostError::Pack)
            }
            Some(PackSource::Scanned { .. }) | None => {
                if self.preparations.len() != 1 {
                    return Err(HostError::Pack(PackError::InvalidPath));
                }
                let snapshot = self.preparations[0]
                    .snapshot(metadata, sources)
                    .map_err(HostError::Pack)?;
                PreparedPack::scanned(snapshot, &plan.pack_metadata(self.config.pack()))
                    .map_err(HostError::PackBuild)
            }
        }
    }

    pub fn files(&self) -> &FileBrowser {
        &self.files
    }

    pub fn runtime(&self) -> &crate::operations::RuntimeHandle {
        &self.runtime
    }
    pub fn database(&self) -> &StateDatabase {
        &self.db
    }
    pub fn management(&self) -> &ManagementAccess {
        &self.management
    }
    pub fn start_admission_refresh(&self) -> Result<AdmissionRefresh, HostError> {
        let host = self.clone();
        let (stop, stopped) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("vesta-admission-refresh".into())
            .spawn(move || {
                loop {
                    match stopped.recv_timeout(Duration::from_secs(2)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if host.reload_admission().is_err() {
                        eprintln!("admission_refresh_failed");
                    }
                }
            })
            .map_err(|_| HostError::Admission(AdmissionDataError::Unavailable))?;
        Ok(AdmissionRefresh {
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// Atomically refresh local admission data. Any malformed document preserves
    /// the previous snapshot; this facade grants no remote management authority.
    pub fn reload_admission(&self) -> Result<(), HostError> {
        let admission = AdmissionData::load(&self.config)?;
        *self
            .admission
            .write()
            .map_err(|_| HostError::Admission(AdmissionDataError::Unavailable))? = admission;
        Ok(())
    }
    /// Port from the trusted game properties; absent if unreadable/malformed.
    pub fn game_port(&self) -> Option<u16> {
        let bytes =
            files::read_local_document(&self.config.game().join(PROPERTIES_FILE), 64 * 1024)
                .ok()?;
        let text = String::from_utf8(bytes).ok()?;
        crate::operations::readiness::probe_from_properties(&text).map(|address| address.port())
    }

    pub fn configuration(&self) -> &HostConfiguration {
        &self.config
    }
    pub fn passwords(&self) -> Option<&PackPasswords> {
        self.passwords.as_ref()
    }
    pub fn minecraft(&self) -> &MinecraftChallenges {
        &self.minecraft
    }
    pub fn sessions(&self) -> &PackSessions {
        &self.sessions
    }
    pub fn releases(&self) -> &PackReleases {
        &self.releases
    }
    pub fn discover(&self, request: &DiscoveryRequest) -> Result<Manifest, ProtocolError> {
        discover_components(self.db.server_id(), self.graceful_stop, request)
    }
    /// Re-read bounded protected admission files for each decision; KDF/provider
    /// I/O occurs first. Cached refresh snapshots never authorize pack sources.
    pub fn with_policy<T>(
        &self,
        decide: impl FnOnce(&PackAccessPolicy<'_>) -> T,
    ) -> Result<T, HostError> {
        let password = self
            .passwords
            .as_ref()
            .map(PackPasswords::active_revision)
            .transpose()
            .map_err(HostError::Identity)?
            .flatten();
        // Cached admission is an operator observation, not authorization. Every
        // decision uses bounded protected reads of the current local files.
        // Failure disables only the live whitelist source, never prolongs its
        // cached grant and never blocks independent explicit/password sources.
        let data = AdmissionData::load(&self.config).unwrap_or_else(|_| AdmissionData {
            properties: None,
            whitelist: Whitelist::default(),
            bans: BanList::default(),
        });
        Ok(decide(&PackAccessPolicy {
            server: self.server,
            mode: self.config.pack_access(),
            properties: data.properties,
            whitelist: &data.whitelist,
            bans: &data.bans,
            password,
        }))
    }
}

/// Explicit TLS material uses the protected, bounded no-follow document reader.
/// Never creates or regenerates trust material.
pub fn read_tls_document(path: &Path) -> Result<Vec<u8>, FileError> {
    files::read_local_document(path, 256 * 1024)
}
