//! Trusted host composition from one validated configuration.
//!
//! Opening validates native paths, parses the protected launch definition, opens the
//! canonical database and composes identity, pack, file and operation services. It
//! never binds a listener, bootstraps an owner, mints credentials or starts Minecraft:
//! the operation runtime only executes Start operations that were already admitted
//! and are still authorized. Failures before the runtime starts release everything
//! opened so far; shutdown is observation-only and keeps ownership on timeout.
use crate::ControlError;
use crate::config::{ConfigError, HostConfiguration, PackAccessMode};
use crate::files::{self, FileBrowser, FileError};
use crate::identity::minecraft::{MinecraftChallenges, MinecraftError, ServerUuid};
use crate::identity::{IdentityService, PackPasswords};
use crate::minecraft_admission::{AdmissionDataError, BanList, ServerProperties};
use crate::operations::{OperationError, OperationRuntime, Operations, ShutdownOutcome};
use crate::pack_access::{PackAccessPolicy, PackSessions, SessionLimits};
use crate::packs::{
    MAX_PINNED_BYTES, MAX_PINNED_FILE_BYTES, MAX_SNAPSHOT_FILES, MAX_SNAPSHOT_SCAN, PackError,
    PackPreparation, PackReleases, ReleaseError, SnapshotLimits,
};
use crate::whitelist::{Whitelist, WhitelistError};
use std::{fmt, path::Path, sync::Arc, time::Duration};
use vesta_game_runtime::launch_config::{LaunchConfigError, LaunchConfiguration, MAX_CONFIG_BYTES};
use vesta_game_runtime::process::GracefulStop;
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
    State(StateError),
    Identity(ControlError),
    Minecraft(MinecraftError),
    Whitelist(WhitelistError),
    Admission(AdmissionDataError),
    Files(FileError),
    Pack(PackError),
    Release(ReleaseError),
    Operation(OperationError),
}
impl HostError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Config(error) => error.code(),
            Self::Launch(error) => error.code(),
            Self::LaunchDocument(_) => "host_launch_definition_unavailable",
            Self::State(error) => error.code(),
            Self::Identity(error) => error.code(),
            Self::Minecraft(error) => error.code(),
            Self::Whitelist(error) => error.code(),
            Self::Admission(error) => error.code(),
            Self::Files(error) => error.code(),
            Self::Pack(error) => error.code(),
            Self::Release(error) => error.code(),
            Self::Operation(error) => error.code(),
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
    passwords: Option<PackPasswords>,
    minecraft: MinecraftChallenges,
    server: ServerUuid,
    admission: AdmissionData,
    sessions: PackSessions,
    files: FileBrowser,
    preparations: Vec<PackPreparation>,
    releases: PackReleases,
    graceful_stop: GracefulStop,
    runtime: OperationRuntime,
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
        let state = config
            .validate_host_paths()
            .map_err(HostError::Config)?
            .into_state_directory();
        let launch = read_launch_definition(&config)?;
        let db = StateDatabase::open(state, clock).map_err(HostError::State)?;
        match compose(config, &db, launch) {
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
        self.admission = AdmissionData::load(&self.config)?;
        Ok(())
    }
    pub fn pack_sessions(&self) -> &PackSessions {
        &self.sessions
    }
    /// The current pack-access policy, read fresh for each decision.
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
    /// One explicit pack preparation per configured scan root, in configuration order.
    pub fn pack_preparations(&self) -> &[PackPreparation] {
        &self.preparations
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
        let stop = match self.graceful_stop {
            GracefulStop::MinecraftCommand => None,
            GracefulStop::Unsupported => Some("unsupported_stop"),
        };
        Manifest::new(
            version,
            self.db.server_id().to_owned(),
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

    /// Stop dispatch and wait up to `timeout` for the runtime to finish. The database
    /// is closed only after the runtime has really completed.
    pub fn shutdown(mut self, timeout: Duration) -> Result<HostShutdown, HostError> {
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
) -> Result<Host, HostError> {
    let protected: Vec<&Path> = config
        .host_protected_paths()
        .iter()
        .map(|path| path.as_path())
        .collect();
    let identity = IdentityService::new(db.clone()).map_err(HostError::Identity)?;
    let passwords = match config.pack_access() {
        PackAccessMode::Minecraft => None,
        PackAccessMode::MinecraftOrPassword => {
            Some(PackPasswords::new(db.clone()).map_err(HostError::Identity)?)
        }
    };
    let server = ServerUuid::parse(db.server_id()).map_err(HostError::Minecraft)?;
    let minecraft = MinecraftChallenges::new(server).map_err(HostError::Minecraft)?;
    let admission = AdmissionData::load(&config)?;
    let sessions = PackSessions::new(server, SessionLimits::default());
    let files =
        FileBrowser::open(db.clone(), config.game(), &protected).map_err(HostError::Files)?;
    let limits = SnapshotLimits::new(
        MAX_PINNED_FILE_BYTES,
        MAX_PINNED_BYTES,
        MAX_SNAPSHOT_FILES,
        MAX_SNAPSHOT_SCAN,
    )
    .map_err(HostError::Pack)?;
    let preparations = config
        .scan_roots()
        .iter()
        .map(|root| PackPreparation::open(db, root, &protected, limits))
        .collect::<Result<Vec<_>, _>>()
        .map_err(HostError::Pack)?;
    let releases = PackReleases::open(db).map_err(HostError::Release)?;
    let graceful_stop = launch.graceful_stop();
    let spec = launch.into_launch_spec(None).map_err(HostError::Launch)?;
    let operations = Operations::new(db.clone()).map_err(HostError::Operation)?;
    // Last: once the driver runs, failures must go through observation-only shutdown.
    let runtime = OperationRuntime::open(operations, spec).map_err(HostError::Operation)?;
    Ok(Host {
        config,
        db: db.clone(),
        identity,
        passwords,
        minecraft,
        server,
        admission,
        sessions,
        files,
        preparations,
        releases,
        graceful_stop,
        runtime,
    })
}

fn read_launch_definition(config: &HostConfiguration) -> Result<LaunchConfiguration, HostError> {
    let bytes = files::read_local_document(config.launch_definition(), MAX_CONFIG_BYTES)
        .map_err(HostError::LaunchDocument)?;
    LaunchConfiguration::parse_toml(&bytes, config.base()).map_err(HostError::Launch)
}

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
