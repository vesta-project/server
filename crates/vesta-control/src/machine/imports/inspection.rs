//! Read-only inspection of an admitted host folder. Software, version and launch
//! choice come from file and directory names plus bounded `server.properties`
//! keys only. Nothing found in the folder is executed; jars are never opened.
//! Process and socket evidence is best effort: anything not observable is
//! reported `unknown`, never "no conflict".
use super::{
    ImportError,
    browse::{FolderBrowser, OwnerBinding, display, read_error},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
};

const MAX_TOP_LEVEL: usize = 4_096;
/// Matches the import copy bound (`CopyLimits::IMPORT`).
const MAX_WALK_ENTRIES: u64 = 2_000_000;
const MAX_PROPERTIES_BYTES: u64 = 64 * 1024;
/// Free space required beyond the source size before a copy may start.
pub const SPACE_MARGIN_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Loader {
    Vanilla,
    Paper,
    Purpur,
    Fabric,
    Forge,
    NeoForge,
    Quilt,
}

#[derive(Clone, Debug, Default)]
pub struct InspectRequest {
    pub folder_token: String,
    /// Trusted, host-chosen managed destination parent used for the space check.
    pub destination: Option<PathBuf>,
    /// For a stopped exported copy: the original instance path, used only to
    /// correlate running processes. Supplied locally, never from a request body.
    pub original_path: Option<PathBuf>,
    /// Game port override from the plan request; null means server.properties.
    pub game_port: Option<u16>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    /// `jar` or `java_argfile`.
    pub kind: &'static str,
    pub relative_path: String,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Detected {
    /// `amp` or `minecraft`.
    pub kind: &'static str,
    pub instance_name: Option<String>,
    pub minecraft_version: Option<String>,
    pub loader: Option<Loader>,
    pub launch: Option<Launch>,
}
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServerPorts {
    pub game_port: Option<u16>,
    pub query_port: Option<u16>,
    pub rcon_port: Option<u16>,
}
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceSize {
    pub files: u64,
    pub bytes: u64,
    pub complete: bool,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProcessEvidence {
    /// `java`, `amp` or `other`.
    pub kind: &'static str,
    pub pid: u32,
    pub name: String,
    pub start_identity: Option<String>,
    /// `source`, `amp_controller`, `unrelated` or `unknown`.
    pub relation: &'static str,
    /// `running`, `stopped` or `unknown`.
    pub state: &'static str,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PortEvidence {
    pub port: u16,
    pub protocol: &'static str,
    /// `free`, `occupied` or `unknown`.
    pub state: &'static str,
    /// `source`, `other`, `unknown`, or null when free.
    pub owner: Option<&'static str>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Completeness {
    /// `complete` or `unknown`.
    pub processes: &'static str,
    pub ports: &'static str,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InspectionReport {
    pub display_path: String,
    pub minecraft: bool,
    pub evidence: Vec<&'static str>,
    pub detected: Detected,
    pub server: ServerPorts,
    pub size: SourceSize,
    pub processes: Vec<ProcessEvidence>,
    pub ports: Vec<PortEvidence>,
    pub inspection: Completeness,
    pub destination_free_bytes: Option<u64>,
    pub blockers: Vec<&'static str>,
    pub warnings: Vec<&'static str>,
    /// Server files recognised: `server_properties`, `server_jar` (a jar or
    /// loader argfile to launch), `world`, `loader_files`.
    pub found: Vec<&'static str>,
    /// Of `server_jar`, `server_properties`, `world`: what is absent. Explains
    /// `not_minecraft_server`.
    pub missing: Vec<&'static str>,
    pub nearby: Vec<Nearby>,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Nearby {
    pub folder_token: String,
    pub path: String,
    pub looks_like_server: bool,
}

pub(crate) fn nearby(browser: &FolderBrowser, binding: &OwnerBinding, token: &str) -> Vec<Nearby> {
    let mut nearby = Vec::new();
    let mut cursor = None;
    loop {
        let Ok(page) = browser.list(
            binding,
            &super::browse::ListRequest {
                folder_token: token.into(),
                cursor,
            },
        ) else {
            break;
        };
        for entry in page.entries {
            if entry.readable && entry.looks_like_server {
                if let Some(folder_token) = entry.folder_token {
                    nearby.push(Nearby {
                        folder_token,
                        path: format!("{}/{}", page.display_path.trim_end_matches('/'), entry.name),
                        looks_like_server: true,
                    });
                }
            }
        }
        cursor = page.next_cursor;
        if cursor.is_none() || nearby.len() >= 100 {
            break;
        }
    }
    nearby.truncate(100);
    nearby
}

/// Inspect the folder a token admits for this owner.
pub fn inspect(
    browser: &FolderBrowser,
    binding: &OwnerBinding,
    request: &InspectRequest,
) -> Result<InspectionReport, ImportError> {
    let opened = browser.open(binding, &request.folder_token)?;
    let mut blockers = BTreeSet::new();
    let mut warnings = BTreeSet::new();
    let mut report = inspect_path(&opened.path, &mut blockers, &mut warnings);

    if blockers.contains("source_unreadable") || !report.minecraft {
        let cause = if blockers.contains("source_unreadable") {
            "source_unreadable"
        } else {
            "not_minecraft_server"
        };
        if cause == "not_minecraft_server" {
            report.nearby = nearby(browser, binding, &request.folder_token);
        }
        report.blockers = vec![cause];
        report.warnings.clear();
        return Ok(report);
    }

    let mut sources = vec![opened.path.clone()];
    if let Some(original) = &request.original_path
        && original.is_absolute()
    {
        sources.push(fs::canonicalize(original).unwrap_or_else(|_| original.clone()));
    }
    let (processes, process_state) = processes(&sources);
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
    if let Some(code) = stopped_evidence(&opened.path, report.server.game_port) {
        blockers.insert(code);
    }
    let source_pids: BTreeSet<u32> = processes
        .iter()
        .filter(|p| p.relation == "source")
        .map(|p| p.pid)
        .collect();

    let game = request
        .game_port
        .or(report.server.game_port)
        .or(Some(25_565));
    let mut ports = Vec::new();
    if let Some(port) = game {
        let evidence = port_state(port, "tcp", &source_pids);
        match evidence.state {
            "occupied" => {
                blockers.insert("game_port_in_use");
            }
            "unknown" => {
                blockers.insert("port_state_unknown");
            }
            _ => {}
        }
        ports.push(evidence);
    }
    for (port, protocol) in [
        (report.server.query_port, "udp"),
        (report.server.rcon_port, "tcp"),
    ] {
        if let Some(port) = port
            && Some(port) != game.filter(|_| protocol == "tcp")
        {
            let evidence = port_state(port, protocol, &source_pids);
            if evidence.state != "free" {
                warnings.insert("auxiliary_port_in_use");
            }
            ports.push(evidence);
        }
    }
    let port_state = if ports.iter().any(|p| p.state == "unknown") {
        "unknown"
    } else {
        "complete"
    };

    if let Some(destination) = &request.destination {
        match crate::files::native::volume_space(destination) {
            Ok((available, _)) => {
                report.destination_free_bytes = Some(available);
                if report.size.complete
                    && available < report.size.bytes.saturating_add(SPACE_MARGIN_BYTES)
                {
                    blockers.insert("insufficient_space");
                }
            }
            Err(_) => {}
        }
    }
    if report.detected.kind == "amp" {
        warnings.insert("disable_amp_auto_restart");
    }
    report.processes = processes;
    report.ports = ports;
    report.inspection = Completeness {
        processes: process_state,
        ports: port_state,
    };
    report.blockers = blockers.into_iter().collect();
    report.warnings = warnings.into_iter().collect();
    Ok(report)
}

/// File-only part of the inspection, shared with candidate discovery.
pub(crate) fn inspect_path(
    path: &Path,
    blockers: &mut BTreeSet<&'static str>,
    warnings: &mut BTreeSet<&'static str>,
) -> InspectionReport {
    let mut report = InspectionReport {
        display_path: display(path),
        minecraft: false,
        evidence: Vec::new(),
        detected: Detected {
            kind: "minecraft",
            instance_name: None,
            minecraft_version: None,
            loader: None,
            launch: None,
        },
        server: ServerPorts::default(),
        size: SourceSize::default(),
        processes: Vec::new(),
        ports: Vec::new(),
        inspection: Completeness {
            processes: "unknown",
            ports: "unknown",
        },
        destination_free_bytes: None,
        blockers: Vec::new(),
        warnings: Vec::new(),
        found: Vec::new(),
        missing: Vec::new(),
        nearby: Vec::new(),
    };
    let listing = match top_level(path) {
        Ok(listing) => listing,
        Err(_) => {
            blockers.insert("source_unreadable");
            return report;
        }
    };
    if let Some(instance) = amp_instance(path) {
        report.detected.kind = "amp";
        report.detected.instance_name = Some(instance);
        report.evidence.push("amp_instance_layout");
    }
    let evidence = minecraft_evidence(path, &listing);
    report.minecraft = evidence.iter().any(|e| {
        matches!(
            *e,
            "server_properties" | "world_directory" | "server_jar" | "loader_markers"
        )
    }) && (evidence.contains(&"server_properties") || evidence.len() >= 2);
    report.evidence.extend(evidence);
    if listing.files.contains("server.properties") {
        match read_properties(&path.join("server.properties")) {
            Ok(properties) => {
                report.server = ports_from(&properties);
                if properties.get("server-ip").is_some_and(|v| !v.is_empty()) {
                    warnings.insert("bind_address_cleared");
                }
                if properties
                    .get("level-name")
                    .is_some_and(|v| !super::properties::safe_world(v))
                {
                    blockers.insert("world_outside_source");
                }
            }
            Err(ImportError::Unreadable) => {
                blockers.insert("source_unreadable");
            }
            Err(_) => {
                blockers.insert("unsafe_path");
            }
        }
    }
    let (loader, version) = detect_software(path, &listing);
    report.detected.loader = loader;
    report.detected.minecraft_version = version;
    report.detected.launch = detect_launch(path, &listing, loader);
    let has = |e: &str| report.evidence.iter().any(|x| *x == e);
    for (name, present) in [
        ("server_properties", has("server_properties")),
        (
            "server_jar",
            has("server_jar") || report.detected.launch.is_some(),
        ),
        ("world", has("world_directory")),
    ] {
        if present {
            report.found.push(name);
        } else {
            report.missing.push(name);
        }
    }
    if has("loader_markers") {
        report.found.push("loader_files");
    }

    report.size = size(path, blockers);
    if !report.minecraft {
        blockers.insert("not_minecraft_server");
    }
    if report.detected.loader.is_none() || report.detected.minecraft_version.is_none() {
        blockers.insert("software_unresolved");
    }
    if report.detected.launch.is_none() {
        blockers.insert("launch_unresolved");
    }
    if listing.files.contains("eula.txt") {
        warnings.insert("eula_not_inherited");
    }
    report
}

pub(crate) struct Listing {
    pub(crate) files: BTreeSet<String>,
    pub(crate) dirs: BTreeSet<String>,
}
/// Regular files and plain directories directly inside `path`; links and special
/// files are left out (the size walk reports them as `unsafe_path`).
pub(crate) fn top_level(path: &Path) -> Result<Listing, ImportError> {
    let mut listing = Listing {
        files: BTreeSet::new(),
        dirs: BTreeSet::new(),
    };
    for (count, entry) in fs::read_dir(path).map_err(read_error)?.enumerate() {
        if count >= MAX_TOP_LEVEL {
            return Err(ImportError::TooManyEntries);
        }
        let entry = entry.map_err(read_error)?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if metadata.file_type().is_symlink() || super::browse::reparse(&metadata) {
            continue;
        }
        if metadata.is_dir() {
            listing.dirs.insert(name);
        } else if metadata.is_file() {
            listing.files.insert(name);
        }
    }
    Ok(listing)
}

/// `.../instances/<instance>/Minecraft`, or an instance directory carrying AMP's
/// config file (existence only; AMP config files are never opened).
pub(crate) fn amp_instance(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    if !name.eq_ignore_ascii_case("Minecraft") {
        return None;
    }
    let instance_dir = path.parent()?;
    let instance = instance_dir.file_name()?.to_str()?.to_owned();
    let in_datastore = instance_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("instances"));
    let marker = ["AMPConfig.conf", "GenericModule.kvp", "MinecraftModule.kvp"]
        .iter()
        .any(|m| fs::symlink_metadata(instance_dir.join(m)).is_ok_and(|md| md.is_file()));
    (in_datastore || marker).then_some(instance)
}

pub(crate) fn minecraft_evidence(path: &Path, listing: &Listing) -> Vec<&'static str> {
    let mut evidence = Vec::new();
    if listing.files.contains("server.properties") {
        evidence.push("server_properties");
    }
    if listing
        .dirs
        .iter()
        .take(64)
        .any(|d| fs::symlink_metadata(path.join(d).join("level.dat")).is_ok_and(|m| m.is_file()))
    {
        evidence.push("world_directory");
    }
    if listing
        .files
        .iter()
        .any(|f| f.to_ascii_lowercase().ends_with(".jar"))
    {
        evidence.push("server_jar");
    }
    if [
        ".fabric",
        ".quilt",
        "mods",
        "plugins",
        "libraries",
        "versions",
    ]
    .iter()
    .any(|d| listing.dirs.contains(*d))
        || [
            "bukkit.yml",
            "spigot.yml",
            "purpur.yml",
            "paper.yml",
            "user_jvm_args.txt",
        ]
        .iter()
        .any(|f| listing.files.contains(*f))
    {
        evidence.push("loader_markers");
    }
    if listing.files.contains("eula.txt") {
        evidence.push("eula_file");
    }
    evidence
}

fn version_like(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    (2..=3).contains(&parts.len())
        && text.len() <= 16
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 4 && p.bytes().all(|b| b.is_ascii_digit()))
}
fn subdirs(path: &Path) -> Vec<String> {
    let Ok(read) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .take(256)
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}
fn plain_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && !super::browse::reparse(&m))
}
/// The single value of `prefix<version><suffix>` among `names`, if exactly one.
fn unique_version<'a>(
    names: impl Iterator<Item = &'a String>,
    prefix: &str,
    split: &[char],
) -> Option<String> {
    let found: BTreeSet<String> = names
        .filter_map(|name| {
            let rest = name.strip_prefix(prefix)?;
            let version = rest.split(split).next()?;
            version_like(version).then(|| version.to_owned())
        })
        .collect();
    (found.len() == 1).then(|| found.into_iter().next())?
}

fn detect_software(path: &Path, listing: &Listing) -> (Option<Loader>, Option<String>) {
    let jars: Vec<&String> = listing
        .files
        .iter()
        .filter(|f| f.to_ascii_lowercase().ends_with(".jar"))
        .collect();
    let libraries = path.join("libraries");
    // Minecraft version evidence independent of loader.
    let versions_dir = unique_version(subdirs(&path.join("versions")).iter(), "", &[]);
    let server_library = unique_version(
        subdirs(&libraries.join("net/minecraft/server")).iter(),
        "",
        &['-'],
    );
    // Paper/Purpur keep `cache/mojang_<version>.jar`.
    let cache_names: Vec<String> = if listing.dirs.contains("cache") {
        fs::read_dir(path.join("cache"))
            .into_iter()
            .flatten()
            .take(256)
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter_map(|n| n.strip_suffix(".jar").map(str::to_owned))
            .collect()
    } else {
        Vec::new()
    };
    let mojang_cache = unique_version(cache_names.iter(), "mojang_", &[]);
    let general = versions_dir
        .clone()
        .or(server_library.clone())
        .or(mojang_cache.clone());

    // NeoForge: libraries/net/neoforged/neoforge/<21.1.77> or neoforged/forge/<1.20.1-47.x>.
    let neo = subdirs(&libraries.join("net/neoforged/neoforge"));
    let neo_legacy = subdirs(&libraries.join("net/neoforged/forge"));
    if !neo.is_empty() || !neo_legacy.is_empty() {
        let from_neo = (neo.len() == 1)
            .then(|| {
                let mut parts = neo[0].split('.');
                let major: u32 = parts.next()?.parse().ok()?;
                let minor: u32 = parts.next()?.parse().ok()?;
                (20..25).contains(&major).then(|| {
                    if minor == 0 {
                        format!("1.{major}")
                    } else {
                        format!("1.{major}.{minor}")
                    }
                })
            })
            .flatten();
        let version = server_library
            .clone()
            .or(from_neo)
            .or_else(|| unique_version(neo_legacy.iter(), "", &['-']));
        return (Some(Loader::NeoForge), version);
    }
    let forge = subdirs(&libraries.join("net/minecraftforge/forge"));
    let forge_jar = unique_version(jars.iter().copied(), "forge-", &['-']);
    if !forge.is_empty() || forge_jar.is_some() {
        let version = unique_version(forge.iter(), "", &['-'])
            .or(forge_jar)
            .or(server_library.clone());
        return (Some(Loader::Forge), version);
    }
    let fabric_jar = jars.iter().find_map(|j| {
        let rest = j.strip_prefix("fabric-server-mc.")?;
        let version = rest.split("-loader").next()?;
        version_like(version).then(|| version.to_owned())
    });
    if listing.dirs.contains(".fabric")
        || listing.files.contains("fabric-server-launch.jar")
        || listing.files.contains("fabric-server-launcher.properties")
        || fabric_jar.is_some()
    {
        let remapped = fs::read_dir(path.join(".fabric/remappedJars"))
            .into_iter()
            .flatten()
            .take(64)
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .collect::<Vec<_>>();
        let version = fabric_jar
            .or_else(|| unique_version(remapped.iter(), "minecraft-", &['-']))
            .or(general);
        return (Some(Loader::Fabric), version);
    }
    if listing.dirs.contains(".quilt") || listing.files.contains("quilt-server-launch.jar") {
        return (Some(Loader::Quilt), general);
    }
    let stems: Vec<String> = jars
        .iter()
        .filter_map(|j| j.strip_suffix(".jar").map(str::to_owned))
        .collect();
    let purpur_jar = unique_version(stems.iter(), "purpur-", &['-']);
    if listing.files.contains("purpur.yml") || purpur_jar.is_some() {
        return (Some(Loader::Purpur), purpur_jar.or(general));
    }
    let paper_jar = unique_version(stems.iter(), "paper-", &['-']);
    if listing.files.contains("paper.yml")
        || plain_file(&path.join("config/paper-global.yml"))
        || paper_jar.is_some()
    {
        return (Some(Loader::Paper), paper_jar.or(general));
    }
    let vanilla_jar = unique_version(stems.iter(), "minecraft_server.", &[]);
    let plugin_platform = listing.files.contains("bukkit.yml") || listing.dirs.contains("plugins");
    if !plugin_platform
        && !listing.dirs.contains("mods")
        && (vanilla_jar.is_some()
            || listing.files.contains("server.jar")
            || listing.files.contains("minecraft_server.jar"))
    {
        let embedded = || {
            ["server.jar", "minecraft_server.jar"]
                .into_iter()
                .filter(|name| listing.files.contains(*name))
                .find_map(|name| jar_minecraft_version(&path.join(name)))
        };
        return (
            Some(Loader::Vanilla),
            vanilla_jar.or(general).or_else(embedded),
        );
    }
    (None, general)
}

/// Largest jar inspected for its embedded version (Mojang's are ~60 MiB).
const MAX_VERSION_JAR_BYTES: u64 = 512 * 1024 * 1024;
/// Largest `version.json` / `META-INF/versions.list` entry read.
const MAX_VERSION_ENTRY_BYTES: u64 = 64 * 1024;

/// The Minecraft version a Mojang server jar declares about itself: its
/// `version.json` `id` (every jar since 1.14, including 1.18+ bundlers), else a
/// bundler's single `META-INF/versions.list` entry. Reads one small zip entry
/// in memory; nothing is executed or extracted.
fn jar_minecraft_version(jar: &Path) -> Option<String> {
    use std::io::Read;
    if !plain_file(jar) || fs::metadata(jar).ok()?.len() > MAX_VERSION_JAR_BYTES {
        return None;
    }
    let mut zip = zip::ZipArchive::new(fs::File::open(jar).ok()?).ok()?;
    let mut read = |name: &str| -> Option<String> {
        let entry = zip.by_name(name).ok()?;
        if entry.size() > MAX_VERSION_ENTRY_BYTES {
            return None;
        }
        let mut text = String::new();
        entry
            .take(MAX_VERSION_ENTRY_BYTES)
            .read_to_string(&mut text)
            .ok()?;
        Some(text)
    };
    let valid = |id: &str| {
        (!id.is_empty()
            && id.len() <= 64
            && id.bytes().any(|b| b.is_ascii_digit())
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b)))
        .then(|| id.to_owned())
    };
    if let Some(text) = read("version.json") {
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        return value["id"].as_str().and_then(valid);
    }
    // Bundler: `<sha256>\t<id>\t<path>` per bundled version; exactly one.
    let list = read("META-INF/versions.list")?;
    let mut ids = list
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('\t').nth(1));
    let id = ids.next()??;
    ids.next().is_none().then(|| valid(id))?
}

fn detect_launch(path: &Path, listing: &Listing, loader: Option<Loader>) -> Option<Launch> {
    let jar = |name: &str| {
        listing.files.contains(name).then(|| Launch {
            kind: "jar",
            relative_path: name.to_owned(),
        })
    };
    let jars: Vec<&String> = listing
        .files
        .iter()
        .filter(|f| {
            let lower = f.to_ascii_lowercase();
            lower.ends_with(".jar") && !lower.contains("installer")
        })
        .collect();
    let prefixed = |prefix: &str| {
        let matching: Vec<&&String> = jars.iter().filter(|j| j.starts_with(prefix)).collect();
        (matching.len() == 1).then(|| Launch {
            kind: "jar",
            relative_path: (*matching[0]).clone(),
        })
    };
    let argfile = if cfg!(windows) {
        "win_args.txt"
    } else {
        "unix_args.txt"
    };
    let argfile_in = |base: &str| {
        let dirs = subdirs(&path.join(base));
        let found: Vec<String> = dirs
            .iter()
            .filter(|d| plain_file(&path.join(base).join(d).join(argfile)))
            .map(|d| format!("{base}/{d}/{argfile}"))
            .collect();
        (found.len() == 1).then(|| Launch {
            kind: "java_argfile",
            relative_path: found[0].clone(),
        })
    };
    let chosen = match loader {
        Some(Loader::NeoForge) => argfile_in("libraries/net/neoforged/neoforge")
            .or_else(|| argfile_in("libraries/net/neoforged/forge")),
        Some(Loader::Forge) => {
            argfile_in("libraries/net/minecraftforge/forge").or_else(|| prefixed("forge-"))
        }
        Some(Loader::Fabric) => {
            jar("fabric-server-launch.jar").or_else(|| prefixed("fabric-server-mc."))
        }
        Some(Loader::Quilt) => jar("quilt-server-launch.jar"),
        Some(Loader::Purpur) => prefixed("purpur-"),
        Some(Loader::Paper) => prefixed("paper-"),
        Some(Loader::Vanilla) => prefixed("minecraft_server").or_else(|| jar("server.jar")),
        None => None,
    };
    chosen.or_else(|| {
        (jars.len() == 1).then(|| Launch {
            kind: "jar",
            relative_path: jars[0].clone(),
        })
    })
}

/// Bounded, no-follow read of `server.properties` keys. Values of other keys
/// (for example `rcon.password`) are never returned.
pub(crate) fn read_properties(path: &Path) -> Result<BTreeMap<String, String>, ImportError> {
    let metadata = fs::symlink_metadata(path).map_err(read_error)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || super::browse::reparse(&metadata)
    {
        return Err(ImportError::LinkedPath);
    }
    let file =
        crate::files::native::import_open(path, false).map_err(|_| ImportError::LinkedPath)?;
    let opened = file.metadata().map_err(read_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            return Err(ImportError::Changed);
        }
    }
    if opened.len() > MAX_PROPERTIES_BYTES {
        return Err(ImportError::TooManyEntries);
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROPERTIES_BYTES)
        .read_to_end(&mut bytes)
        .map_err(read_error)?;
    let text = String::from_utf8_lossy(&bytes);
    let out = super::properties::parse(&text)
        .into_iter()
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                "server-port"
                    | "server-ip"
                    | "query.port"
                    | "enable-query"
                    | "rcon.port"
                    | "enable-rcon"
                    | "level-name"
            )
        })
        .collect();
    Ok(out)
}
fn ports_from(properties: &BTreeMap<String, String>) -> ServerPorts {
    let port = |key: &str| {
        properties
            .get(key)
            .and_then(|v| v.parse::<u16>().ok())
            .filter(|p| *p > 0)
    };
    let enabled = |key: &str| properties.get(key).is_some_and(|v| v == "true");
    let game = port("server-port").or(Some(25_565));
    ServerPorts {
        game_port: game,
        query_port: enabled("enable-query")
            .then(|| port("query.port").or(game))
            .flatten(),
        rcon_port: enabled("enable-rcon")
            .then(|| port("rcon.port").or(Some(25_575)))
            .flatten(),
    }
}

/// Bounded no-follow walk. Links, special files and mount crossings are counted
/// as `unsafe_path`; unreadable subdirectories as `source_unreadable`.
fn size(root: &Path, blockers: &mut BTreeSet<&'static str>) -> SourceSize {
    let mut size = SourceSize {
        complete: true,
        ..SourceSize::default()
    };
    #[cfg(unix)]
    let device = fs::symlink_metadata(root)
        .map(|m| std::os::unix::fs::MetadataExt::dev(&m))
        .ok();
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0_u64;
    while let Some(dir) = stack.pop() {
        let read = match fs::read_dir(&dir) {
            Ok(read) => read,
            Err(_) => {
                blockers.insert("source_unreadable");
                size.complete = false;
                continue;
            }
        };
        for entry in read {
            seen += 1;
            if seen > MAX_WALK_ENTRIES {
                size.complete = false;
                blockers.insert("source_too_large");
                return size;
            }
            let Ok(entry) = entry else {
                blockers.insert("source_unreadable");
                size.complete = false;
                continue;
            };
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                blockers.insert("source_unreadable");
                size.complete = false;
                continue;
            };
            if metadata.file_type().is_symlink() || super::browse::reparse(&metadata) {
                blockers.insert("unsafe_path");
                continue;
            }
            #[cfg(unix)]
            if device.is_some_and(|d| std::os::unix::fs::MetadataExt::dev(&metadata) != d) {
                blockers.insert("unsafe_path");
                continue;
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() {
                size.files += 1;
                size.bytes = size.bytes.saturating_add(metadata.len());
            } else {
                blockers.insert("unsafe_path");
            }
        }
    }
    size
}

/// Probe whether `port` is free for `protocol`, naming its owner when visible.
pub fn port_state(port: u16, protocol: &'static str, source_pids: &BTreeSet<u32>) -> PortEvidence {
    #[cfg(target_os = "linux")]
    if let Some(evidence) = linux::port(port, protocol, source_pids) {
        return evidence;
    }
    let _ = source_pids;
    let state = bind_probe(port, protocol);
    PortEvidence {
        port,
        protocol,
        state,
        owner: (state == "occupied").then_some("unknown"),
    }
}
/// Advisory bind probe on the wildcard and loopback addresses.
pub(crate) fn bind_probe(port: u16, protocol: &str) -> &'static str {
    let mut verdict = "free";
    for address in [
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
    ] {
        let result = if protocol == "udp" {
            std::net::UdpSocket::bind((address, port)).map(drop)
        } else {
            std::net::TcpListener::bind((address, port)).map(drop)
        };
        match result {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => return "occupied",
            Err(error) if cfg!(windows) && error.kind() == std::io::ErrorKind::PermissionDenied => {
                // WSAEACCES: another socket holds the port exclusively.
                return "occupied";
            }
            Err(_) => verdict = "unknown",
        }
    }
    verdict
}

/// Processes relevant to the source folders: those whose working directory,
/// executable or an absolute argument lies inside a source, their AMP parent,
/// and Java processes whose correlation cannot be observed.
pub fn processes(sources: &[PathBuf]) -> (Vec<ProcessEvidence>, &'static str) {
    #[cfg(target_os = "linux")]
    {
        linux::processes(Path::new("/proc"), sources)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = sources;
        (Vec::new(), "unknown")
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::machine::imports::browse::within;
    const MAX_PROCESSES: usize = 100_000;

    struct Stat {
        name: String,
        ppid: u32,
        start: String,
    }
    fn stat(proc: &Path, pid: u32) -> Option<Stat> {
        let text = fs::read_to_string(proc.join(pid.to_string()).join("stat")).ok()?;
        let open = text.find('(')?;
        let close = text.rfind(')')?;
        let name: String = text[open + 1..close]
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(32)
            .collect();
        let fields: Vec<&str> = text[close + 1..].split_whitespace().collect();
        Some(Stat {
            name,
            ppid: fields.get(1)?.parse().ok()?,
            start: fields.get(19)?.to_string(),
        })
    }
    fn pids(proc: &Path) -> Option<Vec<u32>> {
        Some(
            fs::read_dir(proc)
                .ok()?
                .take(MAX_PROCESSES)
                .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                .collect(),
        )
    }
    fn inside(path: &Path, sources: &[PathBuf]) -> bool {
        sources.iter().any(|s| within(path, s))
    }
    fn is_java(name: &str) -> bool {
        name == "java" || name.starts_with("java")
    }
    fn is_amp(name: &str) -> bool {
        name.starts_with("AMP") || name.starts_with("ampinstmgr")
    }

    pub(super) fn processes(
        proc: &Path,
        sources: &[PathBuf],
    ) -> (Vec<ProcessEvidence>, &'static str) {
        let Some(pids) = pids(proc) else {
            return (Vec::new(), "unknown");
        };
        let me = std::process::id();
        let mut out = Vec::new();
        let mut complete = true;
        let mut parents = BTreeSet::new();
        for pid in pids {
            if pid == me {
                continue;
            }
            let Some(stat) = stat(proc, pid) else {
                continue;
            };
            let base = proc.join(pid.to_string());
            let mut related = false;
            let mut observed = 0;
            for link in ["cwd", "exe"] {
                // Unreadable links (other users' processes) leave `observed` false.
                if let Ok(target) = fs::read_link(base.join(link)) {
                    observed += 1;
                    related |= inside(&target, sources);
                }
            }
            if !related && let Ok(cmdline) = fs::read(base.join("cmdline")) {
                // Matched only; argv is never returned.
                related = cmdline
                    .split(|b| *b == 0)
                    .take(512)
                    .filter_map(|arg| std::str::from_utf8(arg).ok())
                    .filter(|arg| arg.starts_with('/'))
                    .any(|arg| inside(Path::new(arg), sources));
            }
            let kind = if is_java(&stat.name) {
                "java"
            } else if is_amp(&stat.name) {
                "amp"
            } else {
                "other"
            };
            if related {
                parents.insert(stat.ppid);
                out.push(ProcessEvidence {
                    kind,
                    pid,
                    name: stat.name,
                    start_identity: Some(format!("linux:{}:{}", pid, stat.start)),
                    relation: "source",
                    state: "running",
                });
            } else if observed < 2 && kind == "java" {
                complete = false;
                out.push(ProcessEvidence {
                    kind,
                    pid,
                    name: stat.name,
                    start_identity: Some(format!("linux:{}:{}", pid, stat.start)),
                    relation: "unknown",
                    state: "unknown",
                });
            }
        }
        for ppid in parents {
            if out.iter().any(|p| p.pid == ppid) {
                continue;
            }
            if let Some(stat) = stat(proc, ppid)
                && is_amp(&stat.name)
            {
                out.push(ProcessEvidence {
                    kind: "amp",
                    pid: ppid,
                    name: stat.name,
                    start_identity: Some(format!("linux:{}:{}", ppid, stat.start)),
                    relation: "amp_controller",
                    state: "running",
                });
            }
        }
        (out, if complete { "complete" } else { "unknown" })
    }

    /// Listening sockets from `/proc/net`; owners via `/proc/<pid>/fd` when visible.
    pub(super) fn port(
        port: u16,
        protocol: &'static str,
        source_pids: &BTreeSet<u32>,
    ) -> Option<PortEvidence> {
        let (files, listen) = if protocol == "udp" {
            (["/proc/net/udp", "/proc/net/udp6"], "07")
        } else {
            (["/proc/net/tcp", "/proc/net/tcp6"], "0A")
        };
        let mut inodes = BTreeSet::new();
        for file in files {
            let text = match fs::read_to_string(file) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return None,
            };
            for line in text.lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                let (Some(local), Some(state), Some(inode)) =
                    (fields.get(1), fields.get(3), fields.get(9))
                else {
                    continue;
                };
                let Some((_, hex)) = local.rsplit_once(':') else {
                    continue;
                };
                if u16::from_str_radix(hex, 16).ok() == Some(port) && *state == listen {
                    inodes.insert(format!("socket:[{inode}]"));
                }
            }
        }
        if inodes.is_empty() {
            return Some(PortEvidence {
                port,
                protocol,
                state: "free",
                owner: None,
            });
        }
        let mut owner = "unknown";
        for pid in pids(Path::new("/proc")).unwrap_or_default() {
            let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
                continue;
            };
            if fds
                .take(65_536)
                .filter_map(|e| e.ok())
                .filter_map(|e| fs::read_link(e.path()).ok())
                .any(|target| inodes.contains(&*target.to_string_lossy()))
            {
                owner = if source_pids.contains(&pid) {
                    "source"
                } else {
                    "other"
                };
                break;
            }
        }
        Some(PortEvidence {
            port,
            protocol,
            state: "occupied",
            owner: Some(owner),
        })
    }
}

/// Independent, unprivileged source-port probe, even when importing to another port.
pub(crate) fn stopped_evidence(source: &Path, port: Option<u16>) -> Option<&'static str> {
    let properties = read_properties(&source.join("server.properties")).ok();
    let mut addresses = vec![
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
    ];
    if let Some(ip) = properties
        .as_ref()
        .and_then(|p| p.get("server-ip"))
        .and_then(|v| v.parse::<IpAddr>().ok())
    {
        // Probe only this host's interfaces, never a source-supplied remote address.
        if !ip.is_unspecified() && std::net::TcpListener::bind((ip, 0)).is_ok() {
            addresses.push(ip);
        }
    }
    if addresses.into_iter().any(|ip| {
        std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::new(ip, port.unwrap_or(25565)),
            std::time::Duration::from_millis(100),
        )
        .is_ok()
    }) {
        return Some("source_port_listening");
    }
    let world = properties
        .and_then(|p| p.get("level-name").cloned())
        .unwrap_or_else(|| "world".into());
    if !super::properties::safe_world(&world) {
        return Some("world_outside_source");
    }
    let world = source.join(world);
    if let Ok(file) = crate::files::native::import_open(&world.join("session.lock"), false) {
        if crate::files::native::import_locked(&file) {
            return Some("source_recently_active");
        }
    }
    for dir in [
        source.join("logs"),
        world.join("region"),
        world.join("DIM-1/region"),
        world.join("DIM1/region"),
    ] {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.take(4096).flatten() {
                if fs::symlink_metadata(entry.path())
                    .ok()
                    .filter(|m| m.is_file() && !m.file_type().is_symlink())
                    .and_then(|m| m.modified().ok())
                    .is_some_and(|t| {
                        t.elapsed().unwrap_or_default() < std::time::Duration::from_secs(60)
                    })
                {
                    return Some("source_recently_active");
                }
            }
        }
    }
    None
}
#[cfg(test)]
mod stopped_tests {
    use super::*;
    #[test]
    fn listening_clears_after_stop() {
        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_eq!(
            stopped_evidence(dir.path(), Some(port)),
            Some("source_port_listening")
        );
        drop(listener);
        assert_eq!(stopped_evidence(dir.path(), Some(port)), None);
        fs::create_dir_all(dir.path().join("world/region")).unwrap();
        fs::write(dir.path().join("world/region/r.0.0.mca"), b"world").unwrap();
        assert_eq!(
            stopped_evidence(dir.path(), None),
            Some("source_recently_active")
        );
    }
}

#[cfg(all(test, unix))]
mod swap_tests {
    use super::*;
    #[test]
    fn properties_swap_to_link_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let source = t.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("server.properties"), "server-port=25565").unwrap();
        let held =
            crate::files::native::import_open(&source.join("server.properties"), false).unwrap();
        fs::rename(source.join("server.properties"), source.join("old")).unwrap();
        fs::write(t.path().join("outside"), "server-port=1").unwrap();
        std::os::unix::fs::symlink(t.path().join("outside"), source.join("server.properties"))
            .unwrap();
        assert!(read_properties(&source.join("server.properties")).is_err());
        let mut text = String::new();
        held.take(1024).read_to_string(&mut text).unwrap();
        assert_eq!(text, "server-port=25565");
    }
}
