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

use crate::files::{self, FileError};
use serde::de::{self, MapAccess, SeqAccess, Visitor, value::MapAccessDeserializer};
use serde::{Deserialize, Deserializer};
use std::{
    error::Error,
    fmt, fs,
    net::IpAddr,
    path::{Component, Path, PathBuf},
};
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
}
impl TlsListener {
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

#[derive(Clone, Eq, PartialEq)]
pub struct HostConfiguration {
    base: PathBuf,
    source: PathBuf,
    state: PathBuf,
    game: PathBuf,
    scan_roots: Vec<PathBuf>,
    launch_definition: PathBuf,
    pack: PackMetadata,
    pack_access: PackAccessMode,
    listener: ListenerDescription,
    protected: Vec<PathBuf>,
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
        let pack = document.pack.0.validate()?;
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
            } => {
                let bind = bind
                    .parse::<IpAddr>()
                    .map_err(|_| ConfigError::InvalidListener)?;
                if port == 0
                    || max_connections == 0
                    || max_connections > MAX_CONNECTIONS
                    || max_request_bytes == 0
                    || max_request_bytes > MAX_REQUEST_BYTES
                    || request_timeout_seconds == 0
                    || request_timeout_seconds > MAX_REQUEST_TIMEOUT_SECONDS
                {
                    return Err(ConfigError::InvalidListener);
                }
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
                })
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
            .chain(protected.iter().skip(2).map(PathBuf::as_path))
        {
            path_bytes = path_bytes
                .checked_add(path_text(path)?.len())
                .ok_or(ConfigError::PathBudgetExceeded)?;
            if path_bytes > MAX_TOTAL_PATH_BYTES {
                return Err(ConfigError::PathBudgetExceeded);
            }
        }
        Ok(Self {
            base: base.to_owned(),
            source,
            state,
            game,
            scan_roots,
            launch_definition,
            pack,
            pack_access: document.pack_access.0.mode,
            listener,
            protected,
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
    pub fn pack_access(&self) -> PackAccessMode {
        self.pack_access
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
    },
}

#[cfg(test)]
mod tests;
