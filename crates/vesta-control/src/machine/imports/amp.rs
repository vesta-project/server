//! Bounded discovery of import candidates inside configured roots, with AMP
//! (CubeCoders) instance layouts as hints. AMP paths are evidence, not proof.
//! AMP passwords/tokens are never read: only `instances.json` may be opened, only
//! inside an import root, bounded, and only whitelisted name/path fields are kept.
//! Nothing is executed and no AMP command is run. Unreadable locations become
//! warnings with an owner-runnable remediation, never escalation.
use super::{
    ImportError,
    browse::{FolderBrowser, OwnerBinding, Root, display, plain_directory, within},
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
const MAX_INSTANCES_JSON_BYTES: u64 = 1024 * 1024;
/// AMP's documented default instance datastore on Linux.
pub const DEFAULT_AMP_DATASTORE: &str = "/home/amp/.ampdata/instances";

#[derive(Clone, Debug)]
pub struct CandidateScan {
    /// Datastore directories holding `<instance>/Minecraft`. Defaults are hints;
    /// custom datastores are configured locally.
    pub amp_datastores: Vec<PathBuf>,
    /// The local account the Vesta Server runs as, for remediation commands.
    pub service_user: String,
}
impl Default for CandidateScan {
    fn default() -> Self {
        Self {
            amp_datastores: if cfg!(windows) {
                Vec::new()
            } else {
                vec![PathBuf::from(DEFAULT_AMP_DATASTORE)]
            },
            service_user: "vesta".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportCandidate {
    pub candidate_id: String,
    /// `amp` or `minecraft`.
    pub kind: &'static str,
    pub label: String,
    pub display_path: String,
    pub folder_token: String,
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
    /// `source_unreadable`, `outside_import_roots` or `scan_truncated`.
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

pub fn candidates(
    browser: &FolderBrowser,
    binding: &OwnerBinding,
    scan: &CandidateScan,
) -> Result<CandidatesResponse, ImportError> {
    let roots = browser.root_list()?;
    let mut out = CandidatesResponse {
        candidates: Vec::new(),
        warnings: Vec::new(),
    };
    let mut seen = BTreeSet::new();
    let mut visits = 0_usize;
    for listed in &roots {
        let Ok(root) = browser.root(&listed.config.root_id) else {
            continue;
        };
        let mut queue = vec![Vec::<String>::new()];
        while let Some(parts) = queue.pop() {
            if out.candidates.len() >= MAX_CANDIDATES || visits >= MAX_VISITS {
                out.warnings.push(ImportWarning {
                    code: "scan_truncated",
                    display_path: display(&root.canonical),
                    remediation: None,
                });
                return Ok(out);
            }
            visits += 1;
            let path = match browser.walk(&root, &parts) {
                Ok(path) => path,
                Err(_) => continue,
            };
            let listing = match top_level(&path) {
                Ok(listing) => listing,
                Err(ImportError::Unreadable) => {
                    out.warnings.push(unreadable(&path, scan));
                    continue;
                }
                Err(_) => continue,
            };
            let evidence = minecraft_evidence(&path, &listing);
            if evidence.contains(&"server_properties")
                || (evidence.contains(&"world_directory") && evidence.contains(&"server_jar"))
            {
                if seen.insert(path.clone()) {
                    push_candidate(browser, binding, &root, parts, &path, evidence, &mut out)?;
                }
                continue;
            }
            if parts.len() >= MAX_SCAN_DEPTH {
                continue;
            }
            for dir in listing.dirs.iter().rev() {
                if dir.starts_with('.') && dir != ".ampdata" {
                    continue;
                }
                let child = path.join(dir);
                if browser.protected_overlap(&child) {
                    continue;
                }
                let mut next = parts.clone();
                next.push(dir.clone());
                queue.push(next);
            }
        }
    }
    for datastore in &scan.amp_datastores {
        hint(
            browser, binding, &roots, datastore, scan, &mut seen, &mut out,
        )?;
    }
    Ok(out)
}

fn push_candidate(
    browser: &FolderBrowser,
    binding: &OwnerBinding,
    root: &Root,
    parts: Vec<String>,
    path: &Path,
    mut evidence: Vec<&'static str>,
    out: &mut CandidatesResponse,
) -> Result<(), ImportError> {
    let amp = amp_instance(path);
    if amp.is_some() {
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
    let label = amp.clone().unwrap_or_else(|| {
        parts
            .last()
            .cloned()
            .unwrap_or_else(|| root.config.label.clone())
    });
    let id_source = format!("{}/{}", root.config.root_id, parts.join("/"));
    let token = browser.mint(binding, root, parts, path)?;
    use sha2::Digest;
    out.candidates.push(ImportCandidate {
        candidate_id: hex::encode(&sha2::Sha256::digest(id_source.as_bytes())[..16]),
        kind: if amp.is_some() { "amp" } else { "minecraft" },
        label,
        display_path: token.display_path,
        folder_token: token.folder_token,
        confidence,
        evidence,
    });
    Ok(())
}

/// Report AMP instances found through a datastore hint that the scan did not
/// already cover: outside every import root, or unreadable.
fn hint(
    browser: &FolderBrowser,
    binding: &OwnerBinding,
    roots: &[Root],
    datastore: &Path,
    scan: &CandidateScan,
    seen: &mut BTreeSet<PathBuf>,
    out: &mut CandidatesResponse,
) -> Result<(), ImportError> {
    if !datastore.is_absolute() {
        return Ok(());
    }
    let mut instances: Vec<PathBuf> = Vec::new();
    match fs::read_dir(datastore) {
        Ok(read) => {
            for entry in read.take(256).flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    instances.push(entry.path().join("Minecraft"));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            out.warnings.push(unreadable(datastore, scan));
            return Ok(());
        }
        Err(_) => {
            // Absent or blocked parent: check whether the parent is the obstacle.
            if let Some(parent) = first_unreadable_ancestor(datastore) {
                out.warnings.push(unreadable(&parent, scan));
            }
            return Ok(());
        }
    }
    if let Some(ampdata) = datastore.parent() {
        for path in instances_json(browser, roots, &ampdata.join("instances.json"), datastore) {
            if !instances.contains(&path) {
                instances.push(path);
            }
        }
    }
    for instance in instances {
        let Ok(metadata) = fs::symlink_metadata(&instance) else {
            if instance.parent().is_some_and(|p| fs::read_dir(p).is_err()) {
                out.warnings.push(unreadable(&instance, scan));
            }
            continue;
        };
        if plain_directory(&metadata).is_err() {
            continue;
        }
        let canonical = fs::canonicalize(&instance).unwrap_or(instance.clone());
        if seen.contains(&canonical) {
            continue;
        }
        let Some(root) = roots.iter().find(|r| within(&canonical, &r.canonical)) else {
            out.warnings.push(ImportWarning {
                code: "outside_import_roots",
                display_path: display(&canonical),
                remediation: Some(Remediation {
                    command: format!("sudo vesta import-root add {}", quote(&canonical)),
                    alternative: "Or stop the AMP instance and copy its Minecraft folder into an existing import root.".into(),
                }),
            });
            continue;
        };
        if fs::read_dir(&canonical).is_err() {
            out.warnings.push(unreadable(&canonical, scan));
            continue;
        }
        // Inside a root but beyond scan depth: admit through the root walk.
        let Ok(relative) = canonical.strip_prefix(&root.canonical) else {
            continue;
        };
        let parts: Vec<String> = relative
            .components()
            .filter_map(|c| c.as_os_str().to_str().map(str::to_owned))
            .collect();
        let Ok(root) = browser.root(&root.config.root_id) else {
            continue;
        };
        let Ok(path) = browser.walk(&root, &parts) else {
            continue;
        };
        let Ok(listing) = top_level(&path) else {
            continue;
        };
        if out.candidates.len() < MAX_CANDIDATES && seen.insert(path.clone()) {
            let evidence = minecraft_evidence(&path, &listing);
            push_candidate(browser, binding, &root, parts, &path, evidence, out)?;
        }
    }
    Ok(())
}

/// AMP's instance list, read only inside an import root. Only instance names,
/// modules and data paths are used; everything else is discarded unread.
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

fn first_unreadable_ancestor(path: &Path) -> Option<PathBuf> {
    let mut ancestors: Vec<&Path> = path.ancestors().collect();
    ancestors.reverse();
    for ancestor in ancestors {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => {
                if fs::read_dir(ancestor).is_err() {
                    return Some(ancestor.to_path_buf());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                return Some(ancestor.to_path_buf());
            }
            Err(_) => return None,
        }
    }
    None
}

fn unreadable(path: &Path, scan: &CandidateScan) -> ImportWarning {
    ImportWarning {
        code: "source_unreadable",
        display_path: display(path),
        remediation: Some(remediation(path, &scan.service_user)),
    }
}

/// Narrow read access for one instance, granted by a local administrator.
/// Never ownership changes or whole-datastore access.
pub fn remediation(path: &Path, user: &str) -> Remediation {
    let user: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    let alternative = "Or stop the AMP instance and copy its Minecraft folder into an import root as a read-only export.".to_owned();
    if cfg!(windows) {
        return Remediation {
            command: format!("icacls {} /grant {user}:(OI)(CI)RX /T", quote(path)),
            alternative,
        };
    }
    // The privileged local CLI registers the folder as an import root and
    // grants the service account the narrow read/traverse access itself.
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
