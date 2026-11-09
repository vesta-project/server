//! Strict trusted-host application descriptions, not Java wrapper compatibility.
//!
//! Parsing is pure: an explicit absolute base resolves path descriptions without
//! opening files, creating state, binding listeners, launching processes, minting
//! credentials or granting administration. There is no missing-document fallback.
//! The persisted database server identity remains authoritative: this schema has
//! no server ID, owner, role, OP, loopback privilege, password secret, executable,
//! argument vector or URL-authority field. Minecraft identity is the common pack
//! access flow; a persisted pack-password gate may be an alternative, not an admin.
//!
//! Schema 1 requires `schema-version = 1`, `[paths]`, `[pack]`, `[pack-access]` and
//! `[listener]`. Launch configuration remains a separate protected document, parsed
//! by `vesta_game_runtime::launch_config::LaunchConfiguration` by host composition;
//! neither this parser nor loading this document reads or compiles that definition.
//!
//! TOML path values use forward slashes with no empty, dot or parent components.
//! Native absolute `base`/`source` arguments are permitted on the current platform.
//! Paths are bounded lexical descriptions, not proof of confinement. The separate
//! `validate_host_paths` uses the existing observing `StateDirectory` constructor
//! to reject native state/game/scan overlap before any database preparation. Its
//! observations are not a lease against future same-account alias replacement.
//! Config, launch and TLS credential documents must be excluded by host file/pack
//! composition using `host_protected_paths`, including while those files are absent.

mod metrics;
pub use metrics::MetricsSettings;

use crate::files::{self, FileError, RelativePath};
use crate::packs::SelectedDirectories;
use serde::de::{self, MapAccess, SeqAccess, Visitor, value::MapAccessDeserializer};
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    error::Error,
    fmt, fs,
    net::IpAddr,
    path::{Component, Path, PathBuf},
};
use vesta_protocol::pack::catalogue::{Platform, PlatformPack};
use vesta_protocol::pack::join::JoinDescriptor;
use vesta_state::{StateDirectory, StateError};

/// Trusted backend choice for files without a hash-bound provider reference.
/// No permissive legacy aliases or case/whitespace normalization.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UnresolvedDisposition {
    #[default]
    Refuse,
    EmbedPublic,
    ExcludePrivate,
}
impl<'de> Deserialize<'de> for UnresolvedDisposition {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        // A derived enum also accepts table-form unit variants in TOML. The host
        // contract deliberately permits only exact string values.
        match String::deserialize(deserializer)?.as_str() {
            "refuse" => Ok(Self::Refuse),
            "embed-public" => Ok(Self::EmbedPublic),
            "exclude-private" => Ok(Self::ExcludePrivate),
            _ => Err(de::Error::custom("invalid unresolved disposition")),
        }
    }
}

pub const SCHEMA_VERSION: u16 = 1;
pub const MAX_CONFIG_BYTES: usize = 65_536;
pub const MAX_SCAN_ROOTS: usize = 8;
pub const MAX_PATH_BYTES: usize = 4_096;
pub const MAX_COMPONENT_BYTES: usize = 255;
pub const MAX_TOTAL_PATH_BYTES: usize = 32_768;
pub const MAX_TEXT_BYTES: usize = 256;
pub const MAX_DESCRIPTION_BYTES: usize = 2_048;
/// Matches the validated pack catalogue's positive, exactly representable revisions.
pub const MAX_REVISION: u64 = (1_u64 << 53) - 1;
pub const MAX_CONNECTIONS: u16 = 1_024;
pub const MAX_REQUEST_BYTES: u32 = 1_048_576;
pub const MAX_REQUEST_TIMEOUT_SECONDS: u16 = 300;

pub type Result<T> = std::result::Result<T, ConfigError>;

/// Fixed codes only: parser, native path, SQL, credential and input text are erased.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigError {
    InvalidDocument,
    DocumentTooLarge,
    UnsupportedSchema,
    InvalidPath,
    PathBudgetExceeded,
    DuplicateScanRoot,
    InvalidPackMetadata,
    InvalidRevision,
    InvalidListener,
    MissingDocument,
    LinkedDocument,
    InvalidDocumentFile,
    DocumentUnavailable,
    StateOverlap,
    InvalidHostPaths,
}
impl ConfigError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidDocument => "host_config_invalid",
            Self::DocumentTooLarge => "host_config_too_large",
            Self::UnsupportedSchema => "host_config_schema_unsupported",
            Self::InvalidPath => "host_config_path_invalid",
            Self::PathBudgetExceeded => "host_config_path_budget_exceeded",
            Self::DuplicateScanRoot => "host_config_scan_root_duplicate",
            Self::InvalidPackMetadata => "host_config_pack_invalid",
            Self::InvalidRevision => "host_config_revision_invalid",
            Self::InvalidListener => "host_config_listener_invalid",
            Self::MissingDocument => "host_config_missing",
            Self::LinkedDocument => "host_config_document_linked",
            Self::InvalidDocumentFile => "host_config_document_not_regular",
            Self::DocumentUnavailable => "host_config_document_unavailable",
            Self::StateOverlap => "host_config_state_overlap",
            Self::InvalidHostPaths => "host_config_host_paths_invalid",
        }
    }
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Host configuration failed")
    }
}
impl Error for ConfigError {}

/// Who owns the game process. `Separate` delegates it to the per-server supervisor so
/// the game survives agent restarts; `InProcess` (the default) keeps the agent as owner.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Supervision {
    #[default]
    InProcess,
    Separate,
}
impl<'de> Deserialize<'de> for Supervision {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "in-process" => Ok(Self::InProcess),
            "separate" => Ok(Self::Separate),
            _ => Err(de::Error::custom("invalid supervision")),
        }
    }
}

pub const MAX_BACKUP_PROFILES: usize = 16;
pub const MAX_RETENTION: u32 = 1000;
pub const MAX_WARN_SECONDS: u32 = 600;

/// Online backups suspend autosave under a supervisor lease; stopped backups need
/// the game stopped and keep it from starting while they read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackupMode {
    Online,
    Stopped,
}
impl<'de> Deserialize<'de> for BackupMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "online" => Ok(Self::Online),
            "stopped" => Ok(Self::Stopped),
            _ => Err(de::Error::custom("invalid backup mode")),
        }
    }
}

/// What a detected source change during the scan does to the backup result.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MutationPolicy {
    Warn,
    /// Fail when world data changed; warn for logs and caches (the safe preset).
    #[default]
    FailWorld,
    FailAny,
}
impl<'de> Deserialize<'de> for MutationPolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "warn" => Ok(Self::Warn),
            "fail-world" => Ok(Self::FailWorld),
            "fail-any" => Ok(Self::FailAny),
            _ => Err(de::Error::custom("invalid mutation policy")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupProfile {
    pub name: String,
    pub mode: BackupMode,
    pub keep_total: u32,
    pub keep_hourly: u32,
    pub keep_last: u32,
    pub keep_daily: u32,
    pub keep_weekly: u32,
    pub keep_monthly: u32,
    pub on_mutation: MutationPolicy,
    /// Seconds of in-game warning before an online backup (recommended 60).
    pub warn_seconds: u32,
    /// Most restore points this profile may hold after retention; a backup that would
    /// exceed it is refused (nothing is deleted to make room).
    pub max_snapshots: Option<u32>,
    /// Literal game-relative roots; empty includes worlds, configs and other game files.
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

/// Default free space kept on the repository disk and, during restores, the game disk.
pub const DEFAULT_MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;

/// Repository-wide budgets. Reaching one stops new backups; nothing is deleted to make
/// room. Safety snapshots before a restore are exempt from the count and size limits
/// (they are bounded separately) but not from the free-space reserve.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupLimits {
    /// Most restore points across all profiles, after retention.
    pub max_snapshots: Option<u32>,
    /// Most bytes stored in the repository.
    pub max_repository_bytes: Option<u64>,
    /// Free space kept on a local repository's disk, and on the game disk by restores.
    pub min_free_bytes: u64,
}
impl Default for BackupLimits {
    fn default() -> Self {
        Self {
            max_snapshots: None,
            max_repository_bytes: None,
            min_free_bytes: DEFAULT_MIN_FREE_BYTES,
        }
    }
}

/// `"500 MB"`, `"50GB"`, `"2 TiB"`, `"0"`: a whole number with an optional unit
/// (B, KB/MB/GB/TB in powers of 1000, KiB/MiB/GiB/TiB in powers of 1024).
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, unit) = text.split_at(split);
    if digits.is_empty() || digits.len() > 20 {
        return None;
    }
    let value: u64 = digits.parse().ok()?;
    let multiplier: u64 = match unit.trim() {
        "" | "B" => 1,
        "KB" => 1000,
        "MB" => 1000_u64.pow(2),
        "GB" => 1000_u64.pow(3),
        "TB" => 1000_u64.pow(4),
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        _ => return None,
    };
    value.checked_mul(multiplier)
}

/// S3-compatible repository location. The secret key is read from a separate file
/// (owner-protected like the host configuration), never written into this document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct S3Repository {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub access_key_id: String,
    pub secret_key_file: PathBuf,
    pub path_style: bool,
}

/// Where backups are stored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackupRepository {
    /// A local or mounted directory.
    Local(PathBuf),
    S3(S3Repository),
}

/// Trusted repository configuration. Without host settings, the instance default is used.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupSettings {
    pub repository: BackupRepository,
    pub restore_root: Option<PathBuf>,
    pub profiles: Vec<BackupProfile>,
    pub limits: BackupLimits,
}
impl BackupSettings {
    pub fn for_instance(base: &Path) -> Self {
        Self {
            repository: BackupRepository::Local(base.join("backups/repository")),
            restore_root: Some(base.join("backups/restores")),
            profiles: Vec::new(),
            limits: BackupLimits::default(),
        }
    }
    pub fn profile(&self, name: &str) -> Option<&BackupProfile> {
        self.profiles.iter().find(|p| p.name == name)
    }
}

pub const DEFAULT_WARN_SECONDS: u32 = 60;

/// The rules for backup profiles and limits, wherever they come from (host.toml or a
/// policy edited at runtime): 0 to 16 uniquely named profiles, bounded retention and
/// warnings, and no zero caps (a zero cap could only ever refuse; omit it instead).
pub fn validate_backup_policy(profiles: &[BackupProfile], limits: &BackupLimits) -> Result<()> {
    if profiles.len() > MAX_BACKUP_PROFILES {
        return Err(ConfigError::InvalidDocument);
    }
    for (index, p) in profiles.iter().enumerate() {
        if !crate::backups::ignore::valid_selection(&p.include, &p.exclude)
            || !valid_profile_name(&p.name)
            || profiles[..index].iter().any(|q| q.name == p.name)
            || [p.keep_total, p.keep_hourly, p.keep_last, p.keep_daily, p.keep_weekly, p.keep_monthly]
                .iter()
                .any(|&k| k > MAX_RETENTION)
            || p.warn_seconds > MAX_WARN_SECONDS
            || p.max_snapshots == Some(0)
        {
            return Err(ConfigError::InvalidDocument);
        }
    }
    if limits.max_snapshots == Some(0) || limits.max_repository_bytes == Some(0) {
        return Err(ConfigError::InvalidDocument);
    }
    Ok(())
}

fn valid_profile_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
}

/// Lexical containment (either direction), used before any native check.
fn nested(left: &Path, right: &Path) -> bool {
    let (l, r): (Vec<_>, Vec<_>) = (left.components().collect(), right.components().collect());
    let n = l.len().min(r.len());
    same_spelling(
        &l[..n].iter().collect::<PathBuf>(),
        &r[..n].iter().collect::<PathBuf>(),
    )
}

/// Opt-in crash restart settings, validated again by the supervisor policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestartSettings {
    pub crash_limit: u32,
    pub crash_window_minutes: u32,
    pub backoff_seconds: u32,
    pub max_backoff_seconds: u32,
}

/// Pack access preference only. Neither mode supplies canonical control capabilities.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum PackAccessMode {
    Minecraft,
    MinecraftOrPassword,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LoaderKind {
    Fabric,
    Quilt,
    Forge,
    Neoforge,
}

#[derive(Clone, Eq, PartialEq)]
pub struct LoaderDescription {
    kind: LoaderKind,
    version: String,
}
impl LoaderDescription {
    pub fn kind(&self) -> LoaderKind {
        self.kind
    }
    pub fn version(&self) -> &str {
        &self.version
    }
}
impl fmt::Debug for LoaderDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoaderDescription")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct PackMetadata {
    name: String,
    version: String,
    revision: u64,
    minimum_revision: u64,
    minecraft_version: String,
    description: Option<String>,
    loader: Option<LoaderDescription>,
    unresolved_disposition: UnresolvedDisposition,
    join: Option<JoinDescriptor>,
}
impl PackMetadata {
    pub(crate) fn workspace(
        base: &Self,
        manifest: &crate::packs::workspace::PackManifest,
        revision: u64,
        minimum_revision: u64,
    ) -> Self {
        Self::linked(manifest, revision, minimum_revision, base.join.clone())
    }
    pub(crate) fn linked(
        manifest: &crate::packs::workspace::PackManifest,
        revision: u64,
        minimum_revision: u64,
        join: Option<JoinDescriptor>,
    ) -> Self {
        use crate::packs::workspace::ClientLoader;
        let kind = match manifest.loader {
            ClientLoader::None => None,
            ClientLoader::Fabric => Some(LoaderKind::Fabric),
            ClientLoader::Quilt => Some(LoaderKind::Quilt),
            ClientLoader::Forge => Some(LoaderKind::Forge),
            ClientLoader::Neoforge => Some(LoaderKind::Neoforge),
        };
        Self {
            name: manifest.name.clone(),
            version: manifest.display_version.clone(),
            revision,
            minimum_revision,
            minecraft_version: manifest.minecraft_version.clone(),
            description: Some(manifest.summary.clone()),
            loader: kind.map(|kind| LoaderDescription {
                kind,
                version: manifest.loader_version.clone().unwrap_or_default(),
            }),
            unresolved_disposition: UnresolvedDisposition::Refuse,
            join,
        }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn minimum_revision(&self) -> u64 {
        self.minimum_revision
    }
    pub fn minecraft_version(&self) -> &str {
        &self.minecraft_version
    }
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
    pub fn loader(&self) -> Option<&LoaderDescription> {
        self.loader.as_ref()
    }
    pub fn unresolved_disposition(&self) -> UnresolvedDisposition {
        self.unresolved_disposition
    }
    /// Descriptive player join information, never transport or access authority.
    pub fn join(&self) -> Option<&JoinDescriptor> {
        self.join.as_ref()
    }
    /// The release ledger, not configuration, is authoritative for published revisions.
    pub(crate) fn with_revisions(&self, revision: u64, minimum_revision: u64) -> Self {
        Self {
            revision,
            minimum_revision,
            ..self.clone()
        }
    }
}
impl fmt::Debug for PackMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PackMetadata")
            .field("revision", &self.revision)
            .field("minimum_revision", &self.minimum_revision)
            .finish_non_exhaustive()
    }
}

/// An explicit TLS-only listener description, never a listening socket or authority.
#[derive(Clone, Eq, PartialEq)]
pub struct TlsListener {
    bind: IpAddr,
    port: u16,
    certificate: PathBuf,
    private_key: PathBuf,
    max_connections: u16,
    max_request_bytes: u32,
    request_timeout_seconds: u16,
    public_endpoint: Option<String>,
    peer_requests_per_minute: u32,
    peer_connections_per_minute: u32,
    session_requests_per_minute: u32,
}
impl TlsListener {
    /// Operator-advertised HTTPS base URL; never inferred from bind or peer headers.
    pub fn public_endpoint(&self) -> Option<&str> {
        self.public_endpoint.as_deref()
    }
    pub fn peer_requests_per_minute(&self) -> u32 {
        self.peer_requests_per_minute
    }
    pub fn peer_connections_per_minute(&self) -> u32 {
        self.peer_connections_per_minute
    }
    pub fn session_requests_per_minute(&self) -> u32 {
        self.session_requests_per_minute
    }
    pub fn bind(&self) -> IpAddr {
        self.bind
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    /// Trusted local composition/tests may explicitly request a kernel-assigned port.
    /// Changes only the port; preserves validated TLS/limits and grants no authority.
    /// Parsed operator configuration still requires an explicit nonzero port.
    pub fn on_ephemeral_port(&self) -> Self {
        let mut listener = self.clone();
        listener.port = 0;
        listener
    }
    pub fn certificate(&self) -> &Path {
        &self.certificate
    }
    pub fn private_key(&self) -> &Path {
        &self.private_key
    }
    pub fn max_connections(&self) -> u16 {
        self.max_connections
    }
    pub fn max_request_bytes(&self) -> u32 {
        self.max_request_bytes
    }
    pub fn request_timeout_seconds(&self) -> u16 {
        self.request_timeout_seconds
    }
}
impl fmt::Debug for TlsListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TlsListener[<redacted>]")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListenerDescription {
    Disabled,
    Tls(TlsListener),
}

/// Backend-only whole-pack acquisition. No URLs, fetching or archive extraction.
#[derive(Clone, Eq, PartialEq)]
pub enum PackSource {
    Scanned {
        root: PathBuf,
        directories: Option<SelectedDirectories>,
    },
    Archive {
        root: PathBuf,
        path: RelativePath,
    },
    Platform(PlatformPack),
}
impl fmt::Debug for PackSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Scanned { .. } => "PackSource::Scanned[<redacted>]",
            Self::Archive { .. } => "PackSource::Archive[<redacted>]",
            Self::Platform(_) => "PackSource::Platform[<redacted>]",
        })
    }
}
impl PackSource {
    pub fn root(&self) -> Option<&Path> {
        match self {
            Self::Scanned { root, .. } | Self::Archive { root, .. } => Some(root),
            Self::Platform(_) => None,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct HostConfiguration {
    base: PathBuf,
    source: PathBuf,
    state: PathBuf,
    game: PathBuf,
    scan_roots: Vec<PathBuf>,
    launch_definition: PathBuf,
    pack: PackMetadata,
    pack_source: Option<PackSource>,
    pack_access: PackAccessMode,
    listener: ListenerDescription,
    protected: Vec<PathBuf>,
    supervision: Supervision,
    restart: Option<RestartSettings>,
    backups: Option<BackupSettings>,
    metrics: MetricsSettings,
    notifications: crate::external_alerts::Settings,
    downloads: DownloadSettings,
    on_agent_start: OnAgentStart,
    java_allowlist: Vec<PathBuf>,
}
impl fmt::Debug for HostConfiguration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostConfiguration")
            .field("schema_version", &SCHEMA_VERSION)
            .field("scan_root_count", &self.scan_roots.len())
            .field("pack_access", &self.pack_access)
            .field("listener", &self.listener)
            .finish_non_exhaustive()
    }
}

impl HostConfiguration {
    /// Pure parsing. Native overlap and directory checks deliberately occur in the
    /// separate `validate_host_paths`; callers cannot get a prepared database here.
    /// An explicit source path is required even when bytes were supplied directly.
    pub fn parse_toml(bytes: &[u8], base: &Path, source: &Path) -> Result<Self> {
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError::DocumentTooLarge);
        }
        check_absolute(base)?;
        let source = resolve_native(base, source)?;
        let text = std::str::from_utf8(bytes).map_err(|_| ConfigError::InvalidDocument)?;
        let document = toml::from_str::<Table<Document>>(text)
            .map_err(|_| ConfigError::InvalidDocument)?
            .0;
        let paths = document.paths.0;
        if document.schema_version != SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedSchema);
        }
        let state = resolve_value(base, &paths.state)?;
        let game = resolve_value(base, &paths.game)?;
        let launch_definition = resolve_value(base, &paths.launch_definition)?;
        let mut scan_roots: Vec<PathBuf> = Vec::new();
        for value in paths.scan_roots.0 {
            let path = resolve_value(base, &value)?;
            if scan_roots
                .iter()
                .any(|existing| same_spelling(existing, &path))
            {
                return Err(ConfigError::DuplicateScanRoot);
            }
            scan_roots.push(path);
        }
        if same_spelling(&source, &launch_definition) {
            return Err(ConfigError::InvalidPath);
        }
        let mut wire_pack = document.pack.0;
        let pack_source = wire_pack
            .source
            .take()
            .map(|Table(source)| source.validate(base))
            .transpose()?;
        let pack = wire_pack.validate()?;
        let mut protected = vec![source.clone(), launch_definition.clone()];
        let listener = match document.listener.0 {
            WireListener::Disabled {} => ListenerDescription::Disabled,
            WireListener::Tls {
                bind,
                port,
                certificate,
                private_key,
                max_connections,
                max_request_bytes,
                request_timeout_seconds,
                public_endpoint,
                peer_requests_per_minute,
                peer_connections_per_minute,
                session_requests_per_minute,
            } => {
                let bind = bind
                    .parse::<IpAddr>()
                    .map_err(|_| ConfigError::InvalidListener)?;
                if port == 0
                    || !(1..=60_000).contains(&peer_requests_per_minute)
                    || !(1..=60_000).contains(&peer_connections_per_minute)
                    || !(1..=60_000).contains(&session_requests_per_minute)
                    || max_connections == 0
                    || max_connections > MAX_CONNECTIONS
                    || max_request_bytes == 0
                    || max_request_bytes > MAX_REQUEST_BYTES
                    || request_timeout_seconds == 0
                    || request_timeout_seconds > MAX_REQUEST_TIMEOUT_SECONDS
                {
                    return Err(ConfigError::InvalidListener);
                }
                let public_endpoint = public_endpoint
                    .map(|text| {
                        let url =
                            url::Url::parse(&text).map_err(|_| ConfigError::InvalidListener)?;
                        if text.len() > 2048
                            || url.as_str().len() > 2048
                            || text.trim() != text
                            || text.chars().any(char::is_control)
                            || url.scheme() != "https"
                            || url.host_str().is_none()
                            || !url.username().is_empty()
                            || url.password().is_some()
                            || url.query().is_some()
                            || url.fragment().is_some()
                        {
                            return Err(ConfigError::InvalidListener);
                        }
                        Ok(url.to_string())
                    })
                    .transpose()?;
                let certificate = resolve_value(base, &certificate)?;
                let private_key = resolve_value(base, &private_key)?;
                if same_spelling(&certificate, &private_key)
                    || protected.iter().any(|path| {
                        same_spelling(path, &certificate) || same_spelling(path, &private_key)
                    })
                {
                    return Err(ConfigError::InvalidListener);
                }
                protected.push(certificate.clone());
                protected.push(private_key.clone());
                ListenerDescription::Tls(TlsListener {
                    bind,
                    port,
                    certificate,
                    private_key,
                    max_connections,
                    max_request_bytes,
                    request_timeout_seconds,
                    public_endpoint,
                    peer_requests_per_minute,
                    peer_connections_per_minute,
                    session_requests_per_minute,
                })
            }
        };
        let backups = document
            .backups
            .map(|Table(wire)| -> Result<BackupSettings> {
                let restore_root = wire
                    .restore_root
                    .map(|root| resolve_value(base, &root))
                    .transpose()?;
                let repository = match (wire.repository, wire.s3) {
                    (Some(path), None) => BackupRepository::Local(resolve_value(base, &path)?),
                    (None, Some(Table(s3))) => {
                        let text_ok = |t: &str, max: usize| {
                            !t.is_empty() && t.len() <= max && !t.chars().any(char::is_control)
                        };
                        if !text_ok(&s3.endpoint, 512)
                            || !text_ok(&s3.region, 64)
                            || !text_ok(&s3.bucket, 63)
                            || !text_ok(&s3.access_key_id, 128)
                            || s3.prefix.as_deref().is_some_and(|p| !text_ok(p, 128))
                        {
                            return Err(ConfigError::InvalidDocument);
                        }
                        let secret_key_file = resolve_value(base, &s3.secret_key_file)?;
                        protected.push(secret_key_file.clone());
                        BackupRepository::S3(S3Repository {
                            endpoint: s3.endpoint,
                            region: s3.region,
                            bucket: s3.bucket,
                            prefix: s3.prefix.unwrap_or_default(),
                            access_key_id: s3.access_key_id,
                            secret_key_file,
                            path_style: s3.path_style.unwrap_or(true),
                        })
                    }
                    // Exactly one repository location.
                    _ => return Err(ConfigError::InvalidDocument),
                };
                // Backups must never contain their own repository, restores or the
                // protected state, and must not be written into the game tree.
                let local = match &repository {
                    BackupRepository::Local(path) => Some(path),
                    BackupRepository::S3(_) => None,
                };
                for path in local.into_iter().chain(restore_root.as_ref()) {
                    if nested(path, &game) || nested(path, &state) {
                        return Err(ConfigError::InvalidPath);
                    }
                }
                if let (Some(root), Some(local)) = (&restore_root, local)
                    && nested(root, local)
                {
                    return Err(ConfigError::InvalidPath);
                }
                if wire.profile.len() > MAX_BACKUP_PROFILES {
                    return Err(ConfigError::InvalidDocument);
                }
                let profiles: Vec<BackupProfile> = wire
                    .profile
                    .into_iter()
                    .map(|Table(p)| BackupProfile {
                        name: p.name,
                        mode: p.mode,
                        keep_total: p.keep_total,
                        keep_hourly: p.keep_hourly,
                        keep_last: p.keep_last,
                        keep_daily: p.keep_daily,
                        keep_weekly: p.keep_weekly,
                        keep_monthly: p.keep_monthly,
                        on_mutation: p.on_mutation,
                        warn_seconds: p.warn_seconds.unwrap_or(DEFAULT_WARN_SECONDS),
                        max_snapshots: p.max_snapshots,
                        include: p.include,
                        exclude: p.exclude,
                    })
                    .collect();
                let limits = match wire.limits {
                    None => BackupLimits::default(),
                    Some(Table(l)) => {
                        let size = |t: Option<String>| -> Result<Option<u64>> {
                            t.map(|t| parse_size(&t).ok_or(ConfigError::InvalidDocument))
                                .transpose()
                        };
                        BackupLimits {
                            max_snapshots: l.max_snapshots,
                            max_repository_bytes: size(l.max_repository_size)?,
                            min_free_bytes: size(l.min_free_space)?
                                .unwrap_or(DEFAULT_MIN_FREE_BYTES),
                        }
                    }
                };
                validate_backup_policy(&profiles, &limits)?;
                Ok(BackupSettings {
                    repository,
                    restore_root,
                    profiles,
                    limits,
                })
            })
            .transpose()?;
        let on_agent_start = document
            .process
            .as_ref()
            .map(|Table(p)| {
                if p.on_agent_start.is_some() && p.start_on_agent_start.is_some() {
                    return Err(ConfigError::InvalidDocument);
                }
                Ok(p.on_agent_start
                    .unwrap_or(if p.start_on_agent_start == Some(true) {
                        OnAgentStart::Start
                    } else {
                        OnAgentStart::StayStopped
                    }))
            })
            .transpose()?
            .unwrap_or_default();
        let (supervision, restart) = match document.process {
            None => (Supervision::default(), None),
            Some(Table(process)) => {
                let restart = process.restart.map(|Table(restart)| RestartSettings {
                    crash_limit: restart.crash_limit.unwrap_or(3),
                    crash_window_minutes: restart.crash_window_minutes.unwrap_or(10),
                    backoff_seconds: restart.backoff_seconds.unwrap_or(10),
                    max_backoff_seconds: restart.max_backoff_seconds.unwrap_or(300),
                });
                // Without a separate supervisor, an agent restart would also end the
                // game; automatic crash restart is only offered where it is owned.
                if restart.is_some() && process.supervision != Supervision::Separate {
                    return Err(ConfigError::InvalidDocument);
                }
                (process.supervision, restart)
            }
        };
        let mut path_bytes = 0usize;
        for path in std::iter::once(base)
            .chain([
                source.as_path(),
                state.as_path(),
                game.as_path(),
                launch_definition.as_path(),
            ])
            .chain(scan_roots.iter().map(PathBuf::as_path))
            .chain(pack_source.as_ref().and_then(PackSource::root))
            .chain(protected.iter().skip(2).map(PathBuf::as_path))
        {
            path_bytes = path_bytes
                .checked_add(path_text(path)?.len())
                .ok_or(ConfigError::PathBudgetExceeded)?;
            if path_bytes > MAX_TOTAL_PATH_BYTES {
                return Err(ConfigError::PathBudgetExceeded);
            }
        }
        if let Some(Table(java)) = &document.java
            && (java.custom_path_roots.len() > 32
                || java.custom_path_roots.iter().any(|p| {
                    p.len() > 4096 || !Path::new(p).is_absolute() || p.chars().any(char::is_control)
                }))
        {
            return Err(ConfigError::InvalidPath);
        }
        Ok(Self {
            base: base.to_owned(),
            source,
            state,
            game,
            scan_roots,
            launch_definition,
            pack,
            pack_source,
            pack_access: document.pack_access.0.mode,
            listener,
            protected,
            supervision,
            restart,
            backups,
            downloads: document.downloads.map(|v| v.0).unwrap_or_default(),
            notifications: {
                let settings=document.notifications.map(|v|v.0).unwrap_or_default();
                if !settings.valid(){return Err(ConfigError::InvalidDocument);}
                settings
            },
            metrics: document
                .metrics
                .map(|v| v.0)
                .unwrap_or_default()
                .validated()?,
            on_agent_start,
            java_allowlist: document
                .java
                .map(|t| {
                    t.0.custom_path_roots
                        .into_iter()
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// Observe a trusted local document with the shared native adapter. Missing,
    /// linked, oversized or malformed files never produce legacy defaults.
    /// The helper checks ancestors/leaf/held handle; it is not race-free confinement.
    pub fn load_file(source: &Path, base: &Path) -> Result<Self> {
        check_absolute(base)?;
        let source = resolve_native(base, source)?;
        let bytes =
            files::read_local_document(&source, MAX_CONFIG_BYTES).map_err(document_error)?;
        Self::parse_toml(&bytes, base, &source)
    }

    /// Observe native aliases before any state preparation/creation. State must be
    /// disjoint from both game and every scan root, in both directions. Public
    /// game/scan roots may intentionally coincide or contain one another.
    /// This is a point-in-time validation, not a pin against future filesystem edits.
    pub fn validate_host_paths(&self) -> Result<ValidatedHostPaths> {
        let state = StateDirectory::new(&self.state, Some(&self.game)).map_err(host_path_error)?;
        check_absolute(state.path()).map_err(|_| ConfigError::InvalidHostPaths)?;
        directory_or_missing(&self.game)?;
        for scan in &self.scan_roots {
            StateDirectory::new(&self.state, Some(scan)).map_err(host_path_error)?;
            directory_or_missing(scan)?;
        }
        if let Some(root) = self.pack_source.as_ref().and_then(PackSource::root) {
            StateDirectory::new(&self.state, Some(root)).map_err(host_path_error)?;
            directory_or_missing(root)?;
        }
        Ok(ValidatedHostPaths { state })
    }

    pub fn base(&self) -> &Path {
        &self.base
    }
    pub fn source(&self) -> &Path {
        &self.source
    }
    pub fn state(&self) -> &Path {
        &self.state
    }
    pub fn game(&self) -> &Path {
        &self.game
    }
    pub fn scan_roots(&self) -> &[PathBuf] {
        &self.scan_roots
    }
    pub fn launch_definition(&self) -> &Path {
        &self.launch_definition
    }
    pub fn pack(&self) -> &PackMetadata {
        &self.pack
    }
    /// Absent in existing schema-1 documents: retain the legacy scan-root APIs.
    pub fn pack_source(&self) -> Option<&PackSource> {
        self.pack_source.as_ref()
    }
    pub fn pack_access(&self) -> PackAccessMode {
        self.pack_access
    }
    pub fn supervision(&self) -> Supervision {
        self.supervision
    }
    pub fn restart(&self) -> Option<RestartSettings> {
        self.restart
    }
    /// Opt-in: start the game when the agent starts, only if nothing is running.
    pub fn java_allowlist(&self) -> &[PathBuf] {
        &self.java_allowlist
    }
    pub fn on_agent_start(&self) -> OnAgentStart {
        self.on_agent_start
    }
    pub fn start_on_agent_start(&self) -> bool {
        self.on_agent_start == OnAgentStart::Start
    }
    pub fn downloads(&self) -> DownloadSettings { self.downloads }
    pub fn metrics(&self) -> MetricsSettings {
        self.metrics
    }
    pub fn notifications(&self) -> &crate::external_alerts::Settings { &self.notifications }
    pub fn backups(&self) -> Option<&BackupSettings> {
        self.backups.as_ref()
    }
    pub fn listener(&self) -> &ListenerDescription {
        &self.listener
    }

    /// Mandatory exclusions for file browsing and pack scanning/publication. Host
    /// composition must apply these, even when files are missing. StateDirectory
    /// privacy remains separately enforced; these paths are never download grants.
    pub fn host_protected_paths(&self) -> &[PathBuf] {
        &self.protected
    }
}

/// An observing StateDirectory descriptor only; creation still requires the
/// existing database/host APIs. Debug intentionally does not print its paths.
pub struct ValidatedHostPaths {
    state: StateDirectory,
}
impl ValidatedHostPaths {
    pub fn state_directory(&self) -> &StateDirectory {
        &self.state
    }
    pub fn into_state_directory(self) -> StateDirectory {
        self.state
    }
}
impl fmt::Debug for ValidatedHostPaths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ValidatedHostPaths[<redacted>]")
    }
}

fn host_path_error(error: StateError) -> ConfigError {
    if error.code() == "state_dir_overlaps_game_root" {
        ConfigError::StateOverlap
    } else {
        ConfigError::InvalidHostPaths
    }
}

fn document_error(error: FileError) -> ConfigError {
    match error {
        FileError::NotFound => ConfigError::MissingDocument,
        FileError::LinkedPath => ConfigError::LinkedDocument,
        FileError::NotRegular | FileError::NotDirectory => ConfigError::InvalidDocumentFile,
        FileError::TooLarge => ConfigError::DocumentTooLarge,
        FileError::InvalidPath | FileError::InvalidRoot => ConfigError::InvalidPath,
        _ => ConfigError::DocumentUnavailable,
    }
}

fn directory_or_missing(path: &Path) -> Result<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(ConfigError::InvalidHostPaths),
    }
}

fn validated_text(value: String, max: usize) -> Result<String> {
    if value.is_empty()
        || value.len() > max
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(ConfigError::InvalidPackMetadata)
    } else {
        Ok(value)
    }
}

fn path_text(path: &Path) -> Result<&str> {
    let text = path.to_str().ok_or(ConfigError::InvalidPath)?;
    if text.is_empty()
        || text.len() > MAX_PATH_BYTES
        || text.trim() != text
        || text.chars().any(char::is_control)
    {
        return Err(ConfigError::InvalidPath);
    }
    Ok(text)
}

fn check_components(path: &Path) -> Result<()> {
    path_text(path)?;
    for component in path.components() {
        match component {
            Component::ParentDir | Component::CurDir => return Err(ConfigError::InvalidPath),
            Component::Normal(name) => {
                let text = name.to_str().ok_or(ConfigError::InvalidPath)?;
                if text.len() > MAX_COMPONENT_BYTES
                    || text.starts_with(' ')
                    || text.ends_with(['.', ' '])
                    || text.bytes().any(|c| b"<>:\"|?*\\".contains(&c))
                {
                    return Err(ConfigError::InvalidPath);
                }
                let stem = text
                    .split('.')
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(' ')
                    .to_ascii_uppercase();
                if matches!(
                    stem.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
                ) || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.as_bytes()[3].is_ascii_digit())
                {
                    return Err(ConfigError::InvalidPath);
                }
            }
            #[cfg(windows)]
            Component::Prefix(prefix)
                if !matches!(
                    prefix.kind(),
                    std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
                ) =>
            {
                return Err(ConfigError::InvalidPath);
            }
            _ => {}
        }
    }
    Ok(())
}

fn check_absolute(path: &Path) -> Result<()> {
    check_components(path)?;
    if !path.is_absolute() {
        Err(ConfigError::InvalidPath)
    } else {
        Ok(())
    }
}

fn resolve_native(base: &Path, path: &Path) -> Result<PathBuf> {
    check_components(path)?;
    if !path.is_absolute()
        && (path.has_root() || matches!(path.components().next(), Some(Component::Prefix(_))))
    {
        return Err(ConfigError::InvalidPath);
    }
    let resolved = if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    };
    check_absolute(&resolved)?;
    Ok(resolved)
}

fn resolve_value(base: &Path, text: &str) -> Result<PathBuf> {
    // No native normalization of TOML separators, empty/dot components or canceled
    // ancestors. In particular there is no current-drive or process-CWD fallback.
    if text.contains('\\') {
        return Err(ConfigError::InvalidPath);
    }
    let path = Path::new(text);
    path_text(path)?;
    let mut components = text.split('/');
    if path.is_absolute() {
        components.next();
    }
    if components.any(|part| part.is_empty() || part == "." || part == "..") {
        return Err(ConfigError::InvalidPath);
    }
    resolve_native(base, path)
}

fn same_spelling(left: &Path, right: &Path) -> bool {
    // Configured public roots are conservatively portable even on a case-sensitive
    // filesystem. This is lexical duplicate rejection, not native alias discovery.
    let mut left = left.components();
    let mut right = right.components();
    loop {
        let (left, right) = match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(left), Some(right)) => (left, right),
            _ => return false,
        };
        #[cfg(windows)]
        let same = match (left, right) {
            (Component::Prefix(left), Component::Prefix(right)) => {
                use std::path::Prefix;
                let drive = |prefix| match prefix {
                    Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                        Some(drive.to_ascii_uppercase())
                    }
                    _ => None,
                };
                drive(left.kind()).is_some() && drive(left.kind()) == drive(right.kind())
            }
            _ => left
                .as_os_str()
                .to_str()
                .zip(right.as_os_str().to_str())
                .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right)),
        };
        #[cfg(not(windows))]
        let same = left
            .as_os_str()
            .to_str()
            .zip(right.as_os_str().to_str())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right));
        if !same {
            return false;
        }
    }
}

/// A map-only wire boundary. Derived struct visitors can otherwise deserialize
/// positional arrays; `deny_unknown_fields` alone does not make a TOML table.
struct Table<T>(T);
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Table<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct TableVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for TableVisitor<T> {
            type Value = Table<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a host configuration table")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                T::deserialize(MapAccessDeserializer::new(map)).map(Table)
            }
        }
        deserializer.deserialize_map(TableVisitor(std::marker::PhantomData))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Document {
    schema_version: u16,
    paths: Table<WirePaths>,
    pack: Table<WirePack>,
    pack_access: Table<WirePackAccess>,
    listener: Table<WireListener>,
    process: Option<Table<WireProcess>>,
    java: Option<Table<WireJava>>,
    backups: Option<Table<WireBackups>>,
    metrics: Option<Table<MetricsSettings>>,
    notifications: Option<Table<crate::external_alerts::Settings>>,
    downloads: Option<Table<DownloadSettings>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct WireBackups {
    repository: Option<String>,
    s3: Option<Table<WireS3>>,
    restore_root: Option<String>,
    #[serde(default)]
    profile: Vec<Table<WireBackupProfile>>,
    limits: Option<Table<WireBackupLimits>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct WireBackupLimits {
    max_snapshots: Option<u32>,
    max_repository_size: Option<String>,
    min_free_space: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct WireS3 {
    endpoint: String,
    region: String,
    bucket: String,
    prefix: Option<String>,
    access_key_id: String,
    secret_key_file: String,
    path_style: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct WireBackupProfile {
    name: String,
    mode: BackupMode,
    #[serde(default)]
    keep_total: u32,
    #[serde(default)]
    keep_hourly: u32,
    #[serde(default)]
    keep_last: u32,
    #[serde(default)]
    keep_daily: u32,
    #[serde(default)]
    keep_weekly: u32,
    #[serde(default)]
    keep_monthly: u32,
    #[serde(default)]
    on_mutation: MutationPolicy,
    warn_seconds: Option<u32>,
    max_snapshots: Option<u32>,
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct WireJava {
    #[serde(default)]
    custom_path_roots: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct WireProcess {
    supervision: Supervision,
    restart: Option<Table<WireRestart>>,
    on_agent_start: Option<OnAgentStart>,
    start_on_agent_start: Option<bool>,
}

/// Presence of the table enables crash restart; omitted fields take the
/// recommended preset (three crashes in ten minutes, 10 s to 300 s backoff).
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct WireRestart {
    crash_limit: Option<u32>,
    crash_window_minutes: Option<u32>,
    backoff_seconds: Option<u32>,
    max_backoff_seconds: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct WirePaths {
    state: String,
    game: String,
    scan_roots: ScanRoots,
    launch_definition: String,
}

struct ScanRoots(Vec<String>);
impl<'de> Deserialize<'de> for ScanRoots {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct RootsVisitor;
        impl<'de> Visitor<'de> for RootsVisitor {
            type Value = ScanRoots;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("one to eight scan-root paths")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut roots = Vec::new();
                while let Some(root) = seq.next_element()? {
                    if roots.len() == MAX_SCAN_ROOTS {
                        return Err(de::Error::custom("scan-root limit exceeded"));
                    }
                    roots.push(root);
                }
                if roots.is_empty() {
                    return Err(de::Error::custom("scan-root array is empty"));
                }
                Ok(ScanRoots(roots))
            }
        }
        deserializer.deserialize_seq(RootsVisitor)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct WirePack {
    source: Option<Table<WirePackSource>>,
    name: String,
    version: String,
    revision: u64,
    minimum_revision: u64,
    minecraft_version: String,
    description: Option<String>,
    loader: Option<Table<WireLoader>>,
    #[serde(default)]
    unresolved_disposition: UnresolvedDisposition,
    join: Option<Table<WireJoin>>,
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
enum WirePackSource {
    Scanned {
        root: String,
        directories: Option<Vec<String>>,
    },
    Archive {
        root: String,
        path: String,
    },
    #[serde(rename_all = "kebab-case")]
    Platform {
        provider: String,
        project_id: String,
        version_id: String,
    },
}
impl WirePackSource {
    fn validate(self, base: &Path) -> Result<PackSource> {
        Ok(match self {
            Self::Scanned { root, directories } => PackSource::Scanned {
                root: resolve_value(base, &root)?,
                directories: directories
                    .map(|paths| {
                        SelectedDirectories::new(&paths).map_err(|_| ConfigError::InvalidPath)
                    })
                    .transpose()?,
            },
            Self::Archive { root, path } => {
                let path = RelativePath::parse(&path).map_err(|_| ConfigError::InvalidPath)?;
                if !path
                    .as_str()
                    .rsplit_once('.')
                    .is_some_and(|(_, extension)| {
                        extension.eq_ignore_ascii_case("zip")
                            || extension.eq_ignore_ascii_case("mrpack")
                    })
                {
                    return Err(ConfigError::InvalidPath);
                }
                PackSource::Archive {
                    root: resolve_value(base, &root)?,
                    path,
                }
            }
            Self::Platform {
                provider,
                project_id,
                version_id,
            } => PackSource::Platform(
                PlatformPack::new(
                    match provider.as_str() {
                        "modrinth" => Platform::Modrinth,
                        "curseforge" => Platform::Curseforge,
                        _ => return Err(ConfigError::InvalidDocument),
                    },
                    &project_id,
                    &version_id,
                    None,
                )
                .map_err(|_| ConfigError::InvalidPackMetadata)?,
            ),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireJoin {
    address: String,
    port: u16,
    description: Option<String>,
}

impl WirePack {
    fn validate(self) -> Result<PackMetadata> {
        if self.minimum_revision == 0
            || self.minimum_revision > self.revision
            || self.revision > MAX_REVISION
        {
            return Err(ConfigError::InvalidRevision);
        }
        let join = self
            .join
            .map(|Table(join)| {
                JoinDescriptor::new(
                    &join.address,
                    join.port,
                    &self.minecraft_version,
                    join.description.as_deref(),
                )
                .map_err(|_| ConfigError::InvalidDocument)
            })
            .transpose()?;
        Ok(PackMetadata {
            join,
            unresolved_disposition: self.unresolved_disposition,
            name: validated_text(self.name, MAX_TEXT_BYTES)?,
            version: validated_text(self.version, MAX_TEXT_BYTES)?,
            revision: self.revision,
            minimum_revision: self.minimum_revision,
            minecraft_version: validated_text(self.minecraft_version, MAX_TEXT_BYTES)?,
            description: self
                .description
                .map(|text| validated_text(text, MAX_DESCRIPTION_BYTES))
                .transpose()?,
            loader: self
                .loader
                .map(|Table(loader)| {
                    Ok(LoaderDescription {
                        kind: loader.kind,
                        version: validated_text(loader.version, MAX_TEXT_BYTES)?,
                    })
                })
                .transpose()?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireLoader {
    kind: LoaderKind,
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePackAccess {
    mode: PackAccessMode,
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
enum WireListener {
    Disabled {},
    #[serde(rename_all = "kebab-case")]
    Tls {
        bind: String,
        port: u16,
        certificate: String,
        private_key: String,
        max_connections: u16,
        max_request_bytes: u32,
        request_timeout_seconds: u16,
        #[serde(default)]
        public_endpoint: Option<String>,
        #[serde(default = "default_peer_requests")]
        peer_requests_per_minute: u32,
        #[serde(default = "default_peer_connections")]
        peer_connections_per_minute: u32,
        #[serde(default = "default_session_requests")]
        session_requests_per_minute: u32,
    },
}

#[cfg(test)]
mod tests;

pub(crate) mod properties;
pub(crate) use properties::game_bind_address;
pub(crate) use properties::validate_raw_properties;
pub use properties::{PropertiesPatch, ServerPropertiesEditor};

fn default_peer_requests() -> u32 {
    60
}
fn default_peer_connections() -> u32 {
    120
}
fn default_session_requests() -> u32 {
    300
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnAgentStart {
    #[default]
    StayStopped,
    Sleep,
    Start,
}

/// Omitted concurrency adapts to available CPU capacity and observed transfer rates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DownloadSettings {
    #[serde(default, deserialize_with = "download_concurrency")]
    pub max_concurrent: Option<u8>,
}
fn download_concurrency<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<u8>, D::Error> {
    let value = u8::deserialize(d)?;
    if !(1..=16).contains(&value) { return Err(de::Error::custom("download concurrency must be 1 through 16")); }
    Ok(Some(value))
}
