//! Owner-only browsing of host folders: the filesystem root (drive roots on
//! Windows) plus import roots saved by the privileged local CLI.
//!
//! Remote owners pick a root and slash-separated names below it (or type an
//! absolute path for `check`), and receive opaque folder tokens bound to the
//! owner binding, the root and the opened directory identity. What the service
//! can read is governed by ordinary file permissions of its account. Every use
//! walks the path again one component at a time without following links or
//! reparse points, refuses Vesta Server's own state and pseudo/system trees, and
//! compares the directory identity before and after reading. Ancestors of
//! protected paths can be browsed but not imported. This is check-then-use: it
//! detects swaps between calls, it is not a native sandbox. Copying uses the
//! existing `CopiedExport` primitives, which repeat their own checks.
use super::ImportError;
use crate::files::native;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs::{self, Metadata},
    io,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

pub const PAGE_SIZE: usize = 100;
/// Directories with more entries than this are refused instead of truncated.
pub const MAX_DIRECTORY_ENTRIES: usize = 10_000;
pub const TOKEN_TTL_MS: u64 = 10 * 60 * 1000;
const MAX_TOKENS: usize = 8_192;
const MAX_DEPTH: usize = 64;
const MAX_SEGMENT_BYTES: usize = 255;

pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The authenticated owner a token is bound to. Adapters construct this only
/// after verifying, for this request, that the account is a current primary
/// owner/owner on its active device of this host. It is not itself authority.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OwnerBinding {
    pub host_id: String,
    pub account_id: String,
    pub device_id: String,
}

/// One root added by the privileged local `vesta import-root add <path>` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportRootConfig {
    pub root_id: String,
    pub label: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RootInfo {
    pub root_id: String,
    pub label: String,
    pub display_path: String,
    pub readable: bool,
    /// `filesystem` (`/` or a drive root) or `import_root` (saved locally).
    pub kind: &'static str,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RootsResponse {
    pub roots: Vec<RootInfo>,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveRequest {
    pub root_id: String,
    pub relative_path: String,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FolderTokenResponse {
    pub folder_token: String,
    pub display_path: String,
    /// Milliseconds since the Unix epoch; transport renders RFC 3339.
    pub expires_at: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListRequest {
    pub folder_token: String,
    pub cursor: Option<String>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FolderEntry {
    pub name: String,
    /// Always `"directory"`; files are never listed.
    pub kind: &'static str,
    /// Null only when the platform cannot observe the identity of an unreadable
    /// directory (Windows); Linux tokens unreadable folders so inspection can
    /// report `source_unreadable` with remediation.
    pub folder_token: Option<String>,
    pub readable: bool,
    /// Cheap name-only check: `server.properties`, a server jar or loader marker.
    pub looks_like_server: bool,
    /// For unreadable entries: a command for a local administrator, never run here.
    pub remediation_command: Option<String>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CheckResponse {
    pub path: String,
    /// `ok`, `permission_needed`, `not_found`, `not_directory` or `protected`.
    pub status: &'static str,
    pub folder_token: Option<String>,
    pub remediation_command: Option<String>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ListResponse {
    pub display_path: String,
    pub entries: Vec<FolderEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
pub(crate) struct Root {
    pub(crate) config: ImportRootConfig,
    pub(crate) canonical: PathBuf,
    identity: String,
    filesystem: bool,
}
struct Roots {
    saved: Vec<Root>,
    filesystem: Vec<Root>,
    revision: u64,
}
#[derive(Clone)]
struct TokenEntry {
    binding: OwnerBinding,
    root_id: String,
    root_canonical: PathBuf,
    parts: Vec<String>,
    identity: String,
    expires_at: u64,
}

/// A directory admitted through a token, freshly re-walked for this use.
#[derive(Clone, Debug)]
pub(crate) struct OpenedFolder {
    pub(crate) path: PathBuf,
    pub(crate) root_id: String,
    pub(crate) parts: Vec<String>,
    pub(crate) identity: String,
}

pub struct FolderBrowser {
    roots: RwLock<Roots>,
    protected: Vec<PathBuf>,
    tokens: Mutex<HashMap<String, TokenEntry>>,
    clock: Clock,
}

impl FolderBrowser {
    /// `protected` lists host private state, Vesta Server's install and data
    /// directories, every managed server directory and import destinations.
    /// Saved roots overlapping any of them in either direction, filesystem roots
    /// and linked roots are refused. The filesystem root is always browsable.
    pub fn new(roots: Vec<ImportRootConfig>, protected: Vec<PathBuf>) -> Result<Self, ImportError> {
        Self::with_clock(roots, protected, Arc::new(now_ms))
    }
    /// Like `new`, but saved roots that cannot be used (missing, linked,
    /// overlapping protected paths) are skipped and returned instead of
    /// failing, so one stale saved folder cannot stop the host.
    pub fn new_skipping_unusable(
        roots: Vec<ImportRootConfig>,
        protected: Vec<PathBuf>,
    ) -> Result<(Self, Vec<(ImportRootConfig, ImportError)>), ImportError> {
        let browser = Self::with_clock(Vec::new(), protected, Arc::new(now_ms))?;
        let mut usable = Vec::new();
        let mut skipped = Vec::new();
        for config in roots {
            match browser.open_root(config.clone()) {
                Ok(_) => usable.push(config),
                Err(error) => skipped.push((config, error)),
            }
        }
        browser.replace_roots(usable)?;
        Ok((browser, skipped))
    }
    pub fn with_clock(
        roots: Vec<ImportRootConfig>,
        protected: Vec<PathBuf>,
        clock: Clock,
    ) -> Result<Self, ImportError> {
        let mut checked_protected = Vec::new();
        for path in protected {
            if !path.is_absolute() {
                return Err(ImportError::InvalidRoot);
            }
            if let Ok(real) = fs::canonicalize(&path) {
                checked_protected.push(real);
            }
            checked_protected.push(path);
        }
        let browser = Self {
            roots: RwLock::new(Roots {
                saved: Vec::new(),
                filesystem: Vec::new(),
                revision: 0,
            }),
            protected: checked_protected,
            tokens: Mutex::new(HashMap::new()),
            clock,
        };
        browser.replace_roots(roots)?;
        Ok(browser)
    }
    /// Replace the saved roots (after a local `import-root` change) and rescan
    /// filesystem roots. All tokens are revalidated against current root
    /// membership on each use.
    pub fn replace_roots(&self, roots: Vec<ImportRootConfig>) -> Result<(), ImportError> {
        let mut checked = Vec::new();
        for config in roots {
            if config.root_id.is_empty()
                || config.root_id.len() > 64
                || config.root_id.starts_with(FILESYSTEM_ROOT_PREFIX)
                || checked
                    .iter()
                    .any(|r: &Root| r.config.root_id == config.root_id)
            {
                return Err(ImportError::InvalidRoot);
            }
            checked.push(self.open_root(config)?);
        }
        let filesystem = filesystem_roots()
            .into_iter()
            .filter_map(|config| open_filesystem_root(config).ok())
            .collect();
        let mut state = self.roots.write().map_err(|_| ImportError::Unavailable)?;
        state.saved = checked;
        state.filesystem = filesystem;
        state.revision += 1;
        Ok(())
    }
    fn open_root(&self, config: ImportRootConfig) -> Result<Root, ImportError> {
        let path = &config.path;
        if !path.is_absolute()
            || path.parent().is_none()
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(ImportError::InvalidRoot);
        }
        plain_ancestors(path)?;
        let canonical = fs::canonicalize(path).map_err(read_error)?;
        if canonical.parent().is_none() {
            return Err(ImportError::InvalidRoot);
        }
        for protected in &self.protected {
            if within(&canonical, protected) || within(protected, &canonical) {
                return Err(ImportError::ProtectedPath);
            }
        }
        if system_tree(&canonical) {
            return Err(ImportError::ProtectedPath);
        }
        let metadata = fs::symlink_metadata(&canonical).map_err(read_error)?;
        plain_directory(&metadata)?;
        let identity = native::directory_identity(&canonical).map_err(file_error)?;
        Ok(Root {
            config,
            canonical,
            identity,
            filesystem: false,
        })
    }

    pub fn roots(&self) -> Result<RootsResponse, ImportError> {
        let state = self.roots.read().map_err(|_| ImportError::Unavailable)?;
        Ok(RootsResponse {
            roots: state
                .saved
                .iter()
                .chain(&state.filesystem)
                .map(|root| RootInfo {
                    root_id: root.config.root_id.clone(),
                    label: root.config.label.clone(),
                    display_path: display(&root.canonical),
                    readable: self.check_root(root).is_ok()
                        && fs::read_dir(&root.canonical).is_ok(),
                    kind: if root.filesystem {
                        "filesystem"
                    } else {
                        "import_root"
                    },
                })
                .collect(),
            revision: state.revision,
        })
    }
    /// Saved import roots only; the filesystem root is never scanned as a whole.
    pub(crate) fn root_list(&self) -> Result<Vec<Root>, ImportError> {
        Ok(self
            .roots
            .read()
            .map_err(|_| ImportError::Unavailable)?
            .saved
            .clone())
    }
    /// The filesystem root holding an absolute path, with the path's names below it.
    pub(crate) fn filesystem_root_for(&self, path: &Path) -> Option<(Root, Vec<String>)> {
        let key = volume_key(path)?;
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(_) | Component::RootDir => {}
                Component::Normal(name) => parts.push(name.to_str()?.to_owned()),
                _ => return None,
            }
        }
        let state = self.roots.read().ok()?;
        let root = state
            .filesystem
            .iter()
            .find(|root| volume_key(&root.config.path).as_deref() == Some(key.as_str()))?;
        Some((root.clone(), parts))
    }

    pub fn resolve(
        &self,
        binding: &OwnerBinding,
        request: &ResolveRequest,
    ) -> Result<FolderTokenResponse, ImportError> {
        let parts = parse_relative(&request.relative_path)?;
        let root = self.root(&request.root_id)?;
        let path = self.walk(&root, &parts)?;
        self.mint(binding, &root, parts, &path)
    }

    pub fn list(
        &self,
        binding: &OwnerBinding,
        request: &ListRequest,
    ) -> Result<ListResponse, ImportError> {
        let opened = self.open_for_browsing(binding, &request.folder_token)?;
        let root = self.root(&opened.root_id)?;
        let after = match &request.cursor {
            None => None,
            Some(cursor) => Some(
                B64.decode(cursor)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .filter(|name| segment(name).is_ok())
                    .ok_or(ImportError::InvalidCursor)?,
            ),
        };
        let mut names = Vec::new();
        for entry in fs::read_dir(&opened.path).map_err(read_error)? {
            let entry = entry.map_err(read_error)?;
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            // DirEntry types never follow links; links are omitted, not resolved.
            if !file_type.is_dir() {
                continue;
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if segment(&name).is_err() {
                continue;
            }
            names.push(name);
            if names.len() > MAX_DIRECTORY_ENTRIES {
                return Err(ImportError::TooManyEntries);
            }
        }
        names.sort();
        let start = after
            .as_ref()
            .map(|after| names.partition_point(|name| name <= after))
            .unwrap_or(0);
        let mut entries = Vec::new();
        let mut next_cursor = None;
        for name in &names[start..] {
            if entries.len() == PAGE_SIZE {
                next_cursor = entries
                    .last()
                    .map(|entry: &FolderEntry| B64.encode(entry.name.as_bytes()));
                break;
            }
            let child = opened.path.join(name);
            let Ok(metadata) = fs::symlink_metadata(&child) else {
                continue;
            };
            if plain_directory(&metadata).is_err() || self.hidden(&child) {
                continue;
            }
            let mut parts = opened.parts.clone();
            parts.push(name.clone());
            let server = looks_like_server(&child);
            let folder_token = self
                .mint(binding, &root, parts, &child)
                .ok()
                .map(|t| t.folder_token);
            entries.push(FolderEntry {
                name: name.clone(),
                kind: "directory",
                folder_token,
                readable: server.is_some(),
                looks_like_server: server == Some(true),
                remediation_command: server.is_none().then(|| remediation_command(&child)),
            });
        }
        // Detect a swap of the listed directory while it was being read.
        let again = self.walk(&root, &opened.parts)?;
        if native::directory_identity(&again).map_err(file_error)? != opened.identity {
            return Err(ImportError::Changed);
        }
        Ok(ListResponse {
            display_path: display(&opened.path),
            entries,
            next_cursor,
        })
    }

    /// Revalidate a token for this owner and re-walk its directory, for use as
    /// an import source: folders containing protected paths (for example `/`
    /// or the parent of Vesta Server's data directory) are refused.
    pub(crate) fn open(
        &self,
        binding: &OwnerBinding,
        token: &str,
    ) -> Result<OpenedFolder, ImportError> {
        let opened = self.open_for_browsing(binding, token)?;
        if self.protected_overlap(&opened.path) {
            return Err(ImportError::ProtectedPath);
        }
        Ok(opened)
    }
    fn open_for_browsing(
        &self,
        binding: &OwnerBinding,
        token: &str,
    ) -> Result<OpenedFolder, ImportError> {
        let now = (self.clock)();
        let entry = {
            let mut tokens = self.tokens.lock().map_err(|_| ImportError::Unavailable)?;
            let entry = tokens
                .get(token)
                .cloned()
                .ok_or(ImportError::TokenInvalid)?;
            if entry.expires_at <= now {
                tokens.remove(token);
                return Err(ImportError::TokenExpired);
            }
            entry
        };
        if &entry.binding != binding {
            return Err(ImportError::TokenInvalid);
        }
        let root = self.root(&entry.root_id)?;
        if root.canonical != entry.root_canonical {
            return Err(ImportError::TokenInvalid);
        }
        let path = self.walk(&root, &entry.parts)?;
        let identity = native::directory_identity(&path).map_err(file_error)?;
        if identity != entry.identity {
            return Err(ImportError::Changed);
        }
        Ok(OpenedFolder {
            path,
            root_id: entry.root_id,
            parts: entry.parts,
            identity,
        })
    }

    /// Resolve a typed absolute host path. Only `ok` carries a token, minted
    /// exactly as if the folder had been browsed to from the filesystem root.
    pub fn check(&self, binding: &OwnerBinding, text: &str) -> Result<CheckResponse, ImportError> {
        let typed = normalize_absolute(text)?;
        let reply = |path: &Path, status, token: Option<String>| {
            let remediation_command =
                (status == "permission_needed").then(|| remediation_command(&typed));
            Ok(CheckResponse {
                path: display(path),
                status,
                folder_token: token,
                remediation_command,
            })
        };
        if self.hidden(&typed) {
            return reply(&typed, "protected", None);
        }
        let real = match fs::canonicalize(&typed) {
            Ok(real) => real,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                return reply(&typed, "permission_needed", None);
            }
            Err(_) => return reply(&typed, "not_found", None),
        };
        if self.hidden(&real) {
            return reply(&real, "protected", None);
        }
        match fs::metadata(&real) {
            Ok(metadata) if !metadata.is_dir() => return reply(&real, "not_directory", None),
            Ok(_) => {}
            Err(_) => return reply(&typed, "permission_needed", None),
        }
        if let Err(error) = fs::read_dir(&real) {
            return match error.kind() {
                io::ErrorKind::NotFound => reply(&typed, "not_found", None),
                _ => reply(&typed, "permission_needed", None),
            };
        }
        let Some((root, parts)) = self.filesystem_root_for(&real) else {
            return reply(&typed, "not_found", None);
        };
        let root = self.root(&root.config.root_id)?;
        match self.walk(&root, &parts) {
            Ok(path) => {
                let token = self.mint(binding, &root, parts, &path)?;
                reply(&path, "ok", Some(token.folder_token))
            }
            Err(ImportError::ProtectedPath) => reply(&real, "protected", None),
            Err(ImportError::NotFound) => reply(&typed, "not_found", None),
            Err(ImportError::NotDirectory) => reply(&real, "not_directory", None),
            Err(ImportError::Unreadable) => reply(&typed, "permission_needed", None),
            Err(error) => Err(error),
        }
    }

    /// Mint a token for a directory reached by `walk`/scan below `root`.
    pub(crate) fn mint(
        &self,
        binding: &OwnerBinding,
        root: &Root,
        parts: Vec<String>,
        path: &Path,
    ) -> Result<FolderTokenResponse, ImportError> {
        let identity = native::directory_identity(path).map_err(file_error)?;
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| ImportError::Unavailable)?;
        let token = B64.encode(bytes);
        let now = (self.clock)();
        let expires_at = now.saturating_add(TOKEN_TTL_MS);
        let mut tokens = self.tokens.lock().map_err(|_| ImportError::Unavailable)?;
        tokens.retain(|_, entry| entry.expires_at > now);
        if tokens.len() >= MAX_TOKENS {
            let oldest = tokens
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                tokens.remove(&oldest);
            }
        }
        tokens.insert(
            token.clone(),
            TokenEntry {
                binding: binding.clone(),
                root_id: root.config.root_id.clone(),
                root_canonical: root.canonical.clone(),
                parts,
                identity,
                expires_at,
            },
        );
        Ok(FolderTokenResponse {
            folder_token: token,
            display_path: display(path),
            expires_at,
        })
    }

    pub(crate) fn root(&self, root_id: &str) -> Result<Root, ImportError> {
        let root = {
            let state = self.roots.read().map_err(|_| ImportError::Unavailable)?;
            state
                .saved
                .iter()
                .chain(&state.filesystem)
                .find(|root| root.config.root_id == root_id)
                .cloned()
                .ok_or(ImportError::UnknownRoot)?
        };
        self.check_root(&root)?;
        Ok(root)
    }
    fn check_root(&self, root: &Root) -> Result<(), ImportError> {
        plain_ancestors(&root.config.path)?;
        if fs::canonicalize(&root.config.path).map_err(read_error)? != root.canonical
            || native::directory_identity(&root.canonical).map_err(file_error)? != root.identity
        {
            return Err(ImportError::Changed);
        }
        Ok(())
    }

    /// Walk `parts` below the root one component at a time: no links, reparse
    /// points, special files, protected paths or pseudo/system trees. Mounted
    /// filesystems may be entered (a separate `/home` or `/srv` is common); the
    /// source size walk and copy still refuse mount crossings inside a server.
    pub(crate) fn walk(&self, root: &Root, parts: &[String]) -> Result<PathBuf, ImportError> {
        if parts.len() > MAX_DEPTH {
            return Err(ImportError::InvalidPath);
        }
        let mut path = root.canonical.clone();
        let mut pinned = vec![native::import_open(&path, true).map_err(file_error)?];
        for part in parts {
            segment(part)?;
            let child = path.join(part);
            if self.hidden(&child) {
                return Err(ImportError::ProtectedPath);
            }
            let metadata = fs::symlink_metadata(&child).map_err(read_error)?;
            plain_directory(&metadata)?;
            let actual = fs::canonicalize(&child).map_err(read_error)?;
            if actual.parent() != Some(path.as_path()) {
                return Err(ImportError::LinkedPath);
            }
            let handle = native::import_open(&child, true).map_err(file_error)?;
            if native::file_identity(&handle).map_err(file_error)? != native::directory_identity(&actual).map_err(file_error)? { return Err(ImportError::Changed); }
            pinned.push(handle);
            path = actual;
        }
        if self.hidden(&path) {
            return Err(ImportError::ProtectedPath);
        }
        Ok(path)
    }
    pub(crate) fn protected_overlap(&self, path: &Path) -> bool {
        self.protected
            .iter()
            .any(|protected| within(path, protected) || within(protected, path))
    }
    /// Never listed or entered: protected paths and everything below them, and
    /// pseudo/system trees.
    pub(crate) fn hidden(&self, path: &Path) -> bool {
        self.protected
            .iter()
            .any(|protected| within(path, protected))
            || system_tree(path)
    }
}

/// Root ids of filesystem roots start with this; saved roots may not.
const FILESYSTEM_ROOT_PREFIX: &str = "filesystem";

#[cfg(not(windows))]
fn filesystem_roots() -> Vec<ImportRootConfig> {
    vec![ImportRootConfig {
        root_id: FILESYSTEM_ROOT_PREFIX.into(),
        label: "/".into(),
        path: PathBuf::from("/"),
    }]
}
#[cfg(windows)]
fn filesystem_roots() -> Vec<ImportRootConfig> {
    // Probe letters instead of GetLogicalDrives: this crate denies unsafe_code.
    (b'A'..=b'Z')
        .map(|b| char::from(b))
        .filter(|letter| fs::metadata(format!("{letter}:\\")).is_ok())
        .map(|letter| ImportRootConfig {
            root_id: format!("{FILESYSTEM_ROOT_PREFIX}-{}", letter.to_ascii_lowercase()),
            label: format!("{letter}:\\"),
            path: PathBuf::from(format!("{letter}:\\")),
        })
        .collect()
}
fn open_filesystem_root(config: ImportRootConfig) -> Result<Root, ImportError> {
    let canonical = fs::canonicalize(&config.path).map_err(read_error)?;
    let metadata = fs::symlink_metadata(&canonical).map_err(read_error)?;
    if !metadata.is_dir() {
        return Err(ImportError::NotDirectory);
    }
    let identity = native::directory_identity(&canonical).map_err(file_error)?;
    Ok(Root {
        config,
        canonical,
        identity,
        filesystem: true,
    })
}
/// `/` on Unix; the upper-case drive letter on Windows (plain or verbatim).
fn volume_key(path: &Path) -> Option<String> {
    match path.components().next()? {
        Component::RootDir => Some("/".into()),
        Component::Prefix(prefix) => match prefix.kind() {
            std::path::Prefix::Disk(d) | std::path::Prefix::VerbatimDisk(d) => {
                Some(char::from(d).to_ascii_uppercase().to_string())
            }
            _ => None,
        },
        _ => None,
    }
}

/// Pseudo filesystems, boot files and per-service private trees.
#[cfg(not(windows))]
const SYSTEM_TREES: &[&str] = &[
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/boot",
    "/efi",
    "/lost+found",
    "/snap",
    "/var/run",
    "/var/lock",
];
/// Top-level folders of a drive that are never server folders.
#[cfg(windows)]
const SYSTEM_TREES: &[&str] = &[
    "Windows",
    "System Volume Information",
    "Recovery",
    "Config.Msi",
    "PerfLogs",
];
pub(crate) fn system_tree(path: &Path) -> bool {
    if path
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("lost+found"))
    {
        return true;
    }
    if cfg!(windows) {
        let first = path.components().find_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy()),
            _ => None,
        });
        return first.is_some_and(|first| {
            first.starts_with('$') || SYSTEM_TREES.iter().any(|t| first.eq_ignore_ascii_case(t))
        });
    }
    SYSTEM_TREES.iter().any(|tree| within(path, Path::new(tree)))
}

/// Names-only probe of a directory: `None` when it cannot be read.
pub(crate) fn looks_like_server(path: &Path) -> Option<bool> {
    let read = fs::read_dir(path).ok()?;
    for entry in read.take(512).flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if entry.file_type().is_ok_and(|t| t.is_file()) && server_marker(&name) {
            return Some(true);
        }
    }
    Some(false)
}
fn server_marker(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "server.properties"
            | "server.jar"
            | "minecraft_server.jar"
            | "fabric-server-launch.jar"
            | "fabric-server-launcher.properties"
            | "quilt-server-launch.jar"
            | "user_jvm_args.txt"
            | "bukkit.yml"
            | "spigot.yml"
            | "paper.yml"
            | "purpur.yml"
    ) {
        return true;
    }
    lower.ends_with(".jar")
        && [
            "minecraft_server.",
            "fabric-server-mc.",
            "paper-",
            "purpur-",
            "spigot-",
            "forge-",
            "neoforge-",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        && !lower.contains("installer")
}

/// The local administrator command that grants the service read access.
pub(crate) fn remediation_command(path: &Path) -> String {
    super::amp::remediation(path, super::amp::SERVICE_USER).command
}

/// Absolute, lexically normalised (`.` dropped, `..` applied, no NUL). Links
/// are not resolved here; `check` resolves the result.
pub(crate) fn normalize_absolute(text: &str) -> Result<PathBuf, ImportError> {
    if text.is_empty() || text.len() > 4_096 || text.contains('\0') {
        return Err(ImportError::InvalidPath);
    }
    let path = Path::new(text);
    if !path.is_absolute() {
        return Err(ImportError::InvalidPath);
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if out.parent().is_some() {
                    out.pop();
                }
            }
            Component::Normal(name) => out.push(name),
        }
    }
    Ok(out)
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Slash separated, no empty, `.`/`..`, NUL, backslash or drive segments.
pub(crate) fn parse_relative(text: &str) -> Result<Vec<String>, ImportError> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if text.len() > 4_096 || text.starts_with('/') {
        return Err(ImportError::InvalidPath);
    }
    let parts: Vec<String> = text.split('/').map(str::to_owned).collect();
    if parts.len() > MAX_DEPTH {
        return Err(ImportError::InvalidPath);
    }
    for part in &parts {
        segment(part)?;
    }
    Ok(parts)
}
pub(crate) fn segment(name: &str) -> Result<(), ImportError> {
    if name.is_empty()
        || name.len() > MAX_SEGMENT_BYTES
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\0', ':'])
        || name.chars().any(char::is_control)
        || (cfg!(windows) && name.ends_with(['.', ' ']))
    {
        return Err(ImportError::InvalidPath);
    }
    Ok(())
}

pub(crate) fn plain_directory(metadata: &Metadata) -> Result<(), ImportError> {
    if metadata.file_type().is_symlink() || reparse(metadata) {
        return Err(ImportError::LinkedPath);
    }
    if !metadata.is_dir() {
        return Err(ImportError::NotDirectory);
    }
    Ok(())
}
#[cfg(windows)]
pub(crate) fn reparse(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes()
        & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
        != 0
}
#[cfg(not(windows))]
pub(crate) fn reparse(_metadata: &Metadata) -> bool {
    false
}
fn plain_ancestors(path: &Path) -> Result<(), ImportError> {
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).map_err(read_error)?;
        plain_directory(&metadata)?;
    }
    Ok(())
}

pub(crate) fn read_error(error: io::Error) -> ImportError {
    match error.kind() {
        io::ErrorKind::NotFound => ImportError::NotFound,
        io::ErrorKind::PermissionDenied => ImportError::Unreadable,
        _ => ImportError::Unavailable,
    }
}
fn file_error(error: crate::files::FileError) -> ImportError {
    use crate::files::FileError;
    match error {
        FileError::LinkedPath => ImportError::LinkedPath,
        FileError::NotDirectory => ImportError::NotDirectory,
        FileError::NotFound => ImportError::NotFound,
        _ => ImportError::Unreadable,
    }
}

/// Component-wise prefix (case-insensitive on Windows).
pub(crate) fn within(path: &Path, parent: &Path) -> bool {
    let mut path = path.components();
    for expected in parent.components() {
        let Some(actual) = path.next() else {
            return false;
        };
        let same = if cfg!(windows) {
            actual
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&expected.as_os_str().to_string_lossy())
        } else {
            actual == expected
        };
        if !same {
            return false;
        }
    }
    true
}

/// Informational host path; Windows verbatim prefixes are stripped.
pub(crate) fn display(path: &Path) -> String {
    let text = path.to_string_lossy();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
}
