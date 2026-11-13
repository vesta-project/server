//! Bounded discovery of import candidates: saved import roots, a few common
//! server locations, and other server managers' instance layouts (AMP by
//! CubeCoders) as hints. Layouts are evidence, not proof. Manager passwords and
//! tokens are never read: only `instances.json` may be opened, only inside a
//! saved import root, bounded, and only whitelisted name/path fields are kept.
//! Nothing is executed. Only folders that look like servers are reported;
//! unreadable ones only when identified specifically (never a bare `/home`),
//! with an owner-runnable remediation command.
use super::{
    ImportError,
    browse::{FolderBrowser, OwnerBinding, Root, display, remediation_command, within},
    inspection::{amp_instance, minecraft_evidence, top_level},
};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub const MAX_CANDIDATES: usize = 100;
const MAX_VISITS: usize = 2_000;
const MAX_SCAN_DEPTH: usize = 4;
/// Common locations are scanned shallowly with their own small budget, and
/// silently: a location that is absent, unreadable or empty says nothing.
const COMMON_SCAN_DEPTH: usize = 3;
const COMMON_VISITS: usize = 400;
const MAX_INSTANCES_JSON_BYTES: u64 = 1024 * 1024;
/// AMP's documented default instance datastore on Linux.
pub const DEFAULT_AMP_DATASTORE: &str = "/home/amp/.ampdata/instances";
/// The account Vesta Server runs as on Linux installs.
pub const SERVICE_USER: &str = "vesta";
/// Never descended into while scanning.
const SKIPPED_NAMES: &[&str] = &["node_modules", "__pycache__", "target"];

#[derive(Clone, Debug)]
pub struct CandidateScan {
    /// Datastore directories holding `<instance>/Minecraft`. Defaults are hints;
    /// custom datastores are configured locally.
    pub amp_datastores: Vec<PathBuf>,
    /// Places people commonly keep servers, scanned shallowly.
    pub common_locations: Vec<PathBuf>,
    /// The local account the Vesta Server runs as, for remediation commands.
    pub service_user: String,
}
impl Default for CandidateScan {
    fn default() -> Self {
        let (amp_datastores, common_locations) = if cfg!(windows) {
            (Vec::new(), Vec::new())
        } else {
            (
                vec![PathBuf::from(DEFAULT_AMP_DATASTORE)],
                ["/srv", "/opt", "/home", "/var/games", "/var/lib/minecraft"]
                    .iter()
                    .map(PathBuf::from)
                    .collect(),
            )
        };
        Self {
            amp_datastores,
            common_locations,
            service_user: SERVICE_USER.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportCandidate {
    pub candidate_id: String,
    /// `amp` or `minecraft` (machine code; not for display).
    pub kind: &'static str,
    /// `import_root`, `server_manager` or `common_location` (machine code).
    pub source: &'static str,
    /// The folder's own name.
    pub label: String,
    pub display_path: String,
    /// Null when the folder cannot be opened at all (an untraversable parent).
    pub folder_token: Option<String>,
    pub readable: bool,
    /// Present exactly when `readable` is false.
    pub remediation_command: Option<String>,
    /// `high`, `medium` or `low`.
    pub confidence: &'static str,
    pub evidence: Vec<&'static str>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Remediation {
    /// A command for a local administrator on the host; never run by Vesta.
    pub command: String,
    pub alternative: String,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportWarning {
    /// `scan_truncated` (a saved import root held more than the scan visits).
    pub code: &'static str,
    pub display_path: String,
    pub remediation: Option<Remediation>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CandidatesResponse {
    pub candidates: Vec<ImportCandidate>,
    pub warnings: Vec<ImportWarning>,
}

struct Scan<'a> {
    browser: &'a FolderBrowser,
    binding: &'a OwnerBinding,
    seen: BTreeSet<PathBuf>,
    out: CandidatesResponse,
}

pub fn candidates(
    browser: &FolderBrowser,
    binding: &OwnerBinding,
    scan: &CandidateScan,
) -> Result<CandidatesResponse, ImportError> {
    let saved = browser.root_list()?;
    let mut state = Scan {
        browser,
        binding,
        seen: BTreeSet::new(),
        out: CandidatesResponse {
            candidates: Vec::new(),
            warnings: Vec::new(),
        },
    };
    let mut budget = MAX_VISITS;
    for listed in &saved {
        let Ok(root) = browser.root(&listed.config.root_id) else {
            continue;
        };
        if !state.tree(&root, Vec::new(), MAX_SCAN_DEPTH, &mut budget, "import_root")? {
            state.out.warnings.push(ImportWarning {
                code: "scan_truncated",
                display_path: display(&root.canonical),
                remediation: None,
            });
            return Ok(state.out);
        }
    }
    for datastore in &scan.amp_datastores {
        state.manager_hint(&saved, datastore)?;
    }
    for location in &scan.common_locations {
        let Some((root, parts)) = browser.filesystem_root_for(location) else {
            continue;
        };
        let mut budget = COMMON_VISITS;
        if !state.tree(&root, parts, COMMON_SCAN_DEPTH, &mut budget, "common_location")?
            && state.out.candidates.len() >= MAX_CANDIDATES
        {
            break;
        }
    }
    Ok(state.out)
}

impl Scan<'_> {
    /// Breadth-bounded scan below `start`. Returns false when a bound was hit.
    fn tree(
        &mut self,
        root: &Root,
        start: Vec<String>,
        depth: usize,
        budget: &mut usize,
        source: &'static str,
    ) -> Result<bool, ImportError> {
        let base = start.len();
        let mut queue = vec![start];
        while let Some(parts) = queue.pop() {
            if self.out.candidates.len() >= MAX_CANDIDATES || *budget == 0 {
                return Ok(false);
            }
            *budget -= 1;
            let Ok(path) = self.browser.walk(root, &parts) else {
                continue;
            };
            let listing = match top_level(&path) {
                Ok(listing) => listing,
                Err(ImportError::Unreadable) => {
                    // Below the starting folder, a name that says "minecraft"
                    // identifies a likely server folder worth asking about.
                    if parts.len() > base && (likely_by_name(&path) || amp_instance(&path).is_some())
                    {
                        self.push(root, Some(parts), &path, None, source)?;
                    }
                    continue;
                }
                Err(_) => continue,
            };
            let evidence = minecraft_evidence(&path, &listing);
            if evidence.contains(&"server_properties")
                || (evidence.contains(&"world_directory") && evidence.contains(&"server_jar"))
            {
                self.push(root, Some(parts), &path, Some(evidence), source)?;
                continue;
            }
            if parts.len() - base >= depth {
                continue;
            }
            for dir in listing.dirs.iter().rev() {
                if (dir.starts_with('.') && dir != ".ampdata") || SKIPPED_NAMES.contains(&dir.as_str())
                {
                    continue;
                }
                if self.browser.hidden(&path.join(dir)) {
                    continue;
                }
                let mut next = parts.clone();
                next.push(dir.clone());
                queue.push(next);
            }
        }
        Ok(true)
    }

    /// Instances of another server manager found through its datastore. A
    /// datastore that is absent or unreadable is silent; an instance folder
    /// identified inside a readable datastore is reported even if unreadable.
    fn manager_hint(&mut self, saved: &[Root], datastore: &Path) -> Result<(), ImportError> {
        if !datastore.is_absolute() {
            return Ok(());
        }
        let mut instances: Vec<PathBuf> = Vec::new();
        if let Ok(read) = fs::read_dir(datastore) {
            for entry in read.take(256).flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    instances.push(entry.path().join("Minecraft"));
                }
            }
        }
        if let Some(ampdata) = datastore.parent() {
            for path in instances_json(self.browser, saved, &ampdata.join("instances.json"), datastore)
            {
                if !instances.contains(&path) {
                    instances.push(path);
                }
            }
        }
        for instance in instances {
            if self.out.candidates.len() >= MAX_CANDIDATES {
                break;
            }
            let Some((root, parts)) = self.browser.filesystem_root_for(&instance) else {
                continue;
            };
            let Ok(root) = self.browser.root(&root.config.root_id) else {
                continue;
            };
            match self.browser.walk(&root, &parts) {
                Ok(path) => match top_level(&path) {
                    Ok(listing) => {
                        let evidence = minecraft_evidence(&path, &listing);
                        if evidence.iter().any(|e| {
                            matches!(
                                *e,
                                "server_properties" | "world_directory" | "server_jar" | "loader_markers"
                            )
                        }) {
                            self.push(&root, Some(parts), &path, Some(evidence), "server_manager")?;
                        }
                    }
                    Err(ImportError::Unreadable) => {
                        self.push(&root, Some(parts), &path, None, "server_manager")?;
                    }
                    Err(_) => {}
                },
                // The instance folder exists for root but its parent cannot be
                // traversed by the service: no token, still worth asking about.
                Err(ImportError::Unreadable) => {
                    self.push(&root, None, &instance, None, "server_manager")?;
                }
                Err(_) => {}
            }
        }
        Ok(())
    }

    /// `evidence` is None for an unreadable folder; `parts` None when no token
    /// can be minted.
    fn push(
        &mut self,
        root: &Root,
        parts: Option<Vec<String>>,
        path: &Path,
        evidence: Option<Vec<&'static str>>,
        source: &'static str,
    ) -> Result<(), ImportError> {
        if !self.seen.insert(path.to_path_buf()) {
            return Ok(());
        }
        let readable = evidence.is_some();
        let mut evidence = evidence.unwrap_or_else(|| vec!["folder_name"]);
        let amp = amp_instance(path).is_some();
        if amp {
            evidence.push("amp_instance_layout");
        }
        let confidence =
            if evidence.contains(&"server_properties") && evidence.contains(&"world_directory") {
                "high"
            } else if evidence.contains(&"server_properties") {
                "medium"
            } else {
                "low"
            };
        let token = match parts {
            Some(parts) => self
                .browser
                .mint(self.binding, root, parts, path)
                .ok()
                .map(|t| t.folder_token),
            None => None,
        };
        use sha2::Digest;
        self.out.candidates.push(ImportCandidate {
            candidate_id: hex::encode(&sha2::Sha256::digest(display(path).as_bytes())[..16]),
            kind: if amp { "amp" } else { "minecraft" },
            source,
            label: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| display(path)),
            display_path: display(path),
            folder_token: token,
            readable,
            remediation_command: (!readable).then(|| remediation_command(path)),
            confidence,
            evidence,
        });
        Ok(())
    }
}

fn likely_by_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().contains("minecraft"))
}

/// The manager's instance list, read only inside a saved import root. Only
/// instance names, modules and data paths are used; everything else is
/// discarded unread.
fn instances_json(
    browser: &FolderBrowser,
    roots: &[Root],
    file: &Path,
    datastore: &Path,
) -> Vec<PathBuf> {
    let Ok(canonical) = fs::canonicalize(file) else {
        return Vec::new();
    };
    if !roots.iter().any(|r| within(&canonical, &r.canonical))
        || browser.protected_overlap(&canonical)
    {
        return Vec::new();
    }
    let Ok(metadata) = fs::symlink_metadata(file) else {
        return Vec::new();
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || super::browse::reparse(&metadata)
    {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if fs::File::open(&canonical)
        .and_then(|f| f.take(MAX_INSTANCES_JSON_BYTES).read_to_end(&mut bytes))
        .is_err()
    {
        return Vec::new();
    }
    parse_instances(&bytes, datastore)
}

pub(crate) fn parse_instances(bytes: &[u8], datastore: &Path) -> Vec<PathBuf> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Vec::new();
    };
    let list = match &value {
        serde_json::Value::Array(list) => list.as_slice(),
        serde_json::Value::Object(map) => map
            .get("Instances")
            .and_then(|v| v.as_array())
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        _ => &[],
    };
    let mut out = Vec::new();
    for item in list.iter().take(256) {
        let text = |key: &str| item.get(key).and_then(|v| v.as_str());
        let module = text("Module").or(text("ModuleName")).unwrap_or("");
        if !module.to_ascii_lowercase().contains("minecraft") {
            continue;
        }
        if let Some(path) = text("DataPath").or(text("Path")).or(text("InstancePath")) {
            let path = PathBuf::from(path);
            if path.is_absolute()
                && !path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                out.push(path.join("Minecraft"));
                continue;
            }
        }
        if let Some(name) = text("InstanceName")
            && super::browse::segment(name).is_ok()
        {
            out.push(datastore.join(name).join("Minecraft"));
        }
    }
    out
}

/// Narrow read access for one folder, granted by a local administrator.
/// Never ownership changes or access to a whole datastore.
pub fn remediation(path: &Path, user: &str) -> Remediation {
    let user: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    let alternative = "Or stop the other server manager, copy this folder somewhere Vesta Server can already read, then import the copy.".to_owned();
    if cfg!(windows) {
        return Remediation {
            command: format!("icacls {} /grant {user}:(OI)(CI)RX /T", quote(path)),
            alternative,
        };
    }
    // The privileged local CLI saves the folder as an import root and grants
    // the service account the narrow read/traverse access itself.
    Remediation {
        command: format!("sudo vesta import-root add {}", quote(path)),
        alternative,
    }
}

fn quote(path: &Path) -> String {
    let text = display(path);
    if cfg!(windows) {
        format!("\"{}\"", text.replace('"', ""))
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}
