use super::{
    HostState,
    imports::{
        inspection::SPACE_MARGIN_BYTES,
        jobs::{JobError, JobResult},
    },
    removed,
};
use crate::files::native::{same_volume, volume_space};
use serde::Serialize;
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStorage {
    measured_at: Option<String>,
    volumes: Vec<Volume>,
    servers: Vec<Server>,
    removed: RemovedStorage,
    backups: Backups,
    caches: Vec<Cache>,
    other: u64,
    #[serde(skip)]
    pub removed_sizes: HashMap<Uuid, u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Volume {
    volume_id: String,
    label: String,
    display_path: String,
    total_bytes: u64,
    free_bytes: u64,
    holds: BTreeSet<&'static str>,
    low_at: u64,
    #[serde(skip)]
    root: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Server {
    server_id: Uuid,
    name: String,
    volume_id: String,
    bytes: u64,
    parts: Parts,
    #[serde(skip)]
    largest: Vec<LargestFolder>,
}

#[derive(Serialize)]
struct LargestFolder {
    path: String,
    bytes: u64,
}

#[derive(Default, Serialize)]
struct Parts {
    worlds: u64,
    backups: u64,
    content: u64,
    logs: u64,
    software: u64,
    trash: u64,
    other: u64,
}
impl Parts {
    fn total(&self) -> u64 {
        [
            self.worlds,
            self.backups,
            self.content,
            self.logs,
            self.software,
            self.trash,
            self.other,
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }
}

#[derive(Default, Serialize)]
struct RemovedStorage {
    count: usize,
    bytes: u64,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Backups {
    bytes: u64,
    outside_servers: u64,
}

#[derive(Serialize)]
struct Cache {
    kind: &'static str,
    bytes: u64,
    removable: bool,
}

impl HostStorage {
    pub fn server_details(&self, id: Uuid) -> serde_json::Value {
        let Some(server) = self.servers.iter().find(|server| server.server_id == id) else {
            return serde_json::json!({
                "measuredAt": null, "bytes": 0, "parts": Parts::default(),
                "volume": {"label": null, "freeBytes": null, "totalBytes": null}, "largest": []
            });
        };
        let volume = self
            .volumes
            .iter()
            .find(|volume| volume.volume_id == server.volume_id)
            .expect("measured server has a volume");
        serde_json::json!({
            "measuredAt": self.measured_at, "bytes": server.bytes, "parts": server.parts,
            "volume": {"label": volume.label, "freeBytes": volume.free_bytes, "totalBytes": volume.total_bytes},
            "largest": server.largest
        })
    }

    pub fn active_size(&self, id: Uuid) -> Option<u64> {
        self.servers
            .iter()
            .find(|server| server.server_id == id)
            .map(|server| server.bytes)
    }

    fn volume(&mut self, root: &Path, holds: &'static str) -> JobResult<String> {
        if let Some(existing) = self
            .volumes
            .iter_mut()
            .find(|existing| same_volume(root, &existing.root).unwrap_or(false))
        {
            existing.holds.insert(holds);
            return Ok(existing.volume_id.clone());
        }
        let (free_bytes, total_bytes) = volume_space(root).map_err(|_| JobError::UNAVAILABLE)?;
        let index = self.volumes.len() + 1;
        let volume_id = format!("volume-{index}");
        self.volumes.push(Volume {
            volume_id: volume_id.clone(),
            label: if index == 1 {
                "Data drive".to_owned()
            } else {
                format!("Data drive {index}")
            },
            display_path: super::imports::browse::display(root),
            total_bytes,
            free_bytes,
            holds: BTreeSet::from([holds]),
            low_at: SPACE_MARGIN_BYTES,
            root: root.to_owned(),
        });
        Ok(volume_id)
    }
}

/// Walk configured Vesta data roots. Callers run this on a background thread.
pub fn measure(
    host: &HostState,
    servers: &Path,
    java_cache: Option<&Path>,
) -> JobResult<HostStorage> {
    let parent = servers.parent().ok_or(JobError::UNAVAILABLE)?;
    let state_dir = host.database().directory().path();
    let removed_dir = parent.join("removed");
    let retained_dir = parent.join("retained-backups");
    let mut result = HostStorage::default();
    let servers_volume = result.volume(servers, "servers")?;
    let state_root_bytes = if state_dir.starts_with(servers) {
        0
    } else {
        result.volume(state_dir, "state")?;
        tree_bytes(state_dir, &[servers])?
    };
    result.other = state_root_bytes;
    let mut entries = if let Some(account) = host.snapshot()?.primary_account_id {
        host.visible_servers(account)?
    } else {
        Vec::new()
    };
    entries.sort_unstable_by_key(|(entry, _)| entry.server_id);
    let mut active_dirs = Vec::new();
    for (entry, _) in entries {
        let dir = servers.join(entry.server_id.to_string());
        if !removed::plain_repository(&dir) {
            continue;
        }
        let (parts, largest) = server_parts(Some(&dir), &dir.join("game"), &[])?;
        if parts.backups > 0 {
            result.volume(&dir, "backups")?;
        }
        result.backups.bytes = result.backups.bytes.saturating_add(parts.backups);
        result.servers.push(Server {
            server_id: entry.server_id,
            name: entry.name,
            volume_id: servers_volume.clone(),
            bytes: parts.total(),
            parts,
            largest,
        });
        active_dirs.push(dir);
    }
    for record in removed::list(&removed_dir)? {
        let dir = record.path(&removed_dir);
        let bytes = tree_bytes(&dir, &[])?;
        if record.permanently {
            result.other = result.other.saturating_add(bytes);
        } else {
            result.removed.count += 1;
            result.removed_sizes.insert(record.server_id, bytes);
            result.removed.bytes = result.removed.bytes.saturating_add(bytes);
        }
    }
    if result.removed.bytes > 0 {
        result.volume(&removed_dir, "servers")?;
    }
    let retained = tree_bytes(&retained_dir, &[])?;
    if retained > 0 {
        result.volume(&retained_dir, "backups")?;
    }
    result.backups.outside_servers = retained;
    result.backups.bytes = result.backups.bytes.saturating_add(retained);
    let mut caches = vec![
        ("pack_staging", servers.join(".create-uploads")),
        ("temp", servers.join(".staging")),
    ];
    if let Some(java) = java_cache {
        caches.push(("java", java.to_owned()));
    }
    let mut exclude: Vec<&Path> = active_dirs.iter().map(PathBuf::as_path).collect();
    exclude.push(state_dir);
    for (kind, cache) in &caches {
        if removed::plain_repository(cache) {
            result.volume(cache, "caches")?;
            let bytes = tree_bytes(cache, &[])?;
            result.caches.push(Cache {
                kind,
                bytes,
                removable: false,
            });
            exclude.push(cache);
        }
    }
    result.other = result.other.saturating_add(tree_bytes(servers, &exclude)?);
    result
        .servers
        .sort_unstable_by_key(|server| (std::cmp::Reverse(server.bytes), server.server_id));
    result.measured_at = Some(super::sessions::timestamp(host.now_ms()));
    Ok(result)
}

pub fn measure_instance(
    config: &crate::config::HostConfiguration,
    measured_ms: i64,
) -> JobResult<serde_json::Value> {
    use crate::config::{BackupRepository, BackupSettings};
    let game = config.game();
    if !removed::plain_repository(game) {
        return Err(JobError::UNAVAILABLE);
    }
    let settings = config
        .backups()
        .cloned()
        .unwrap_or_else(|| BackupSettings::for_instance(config.base()));
    let repository = match settings.repository {
        BackupRepository::Local(path)
            if path.starts_with(config.base())
                && !game.starts_with(&path)
                && removed::plain_repository(&path) =>
        {
            Some(path)
        }
        _ => None,
    };
    let mut exclude = vec![config.state()];
    exclude.extend(repository.as_deref());
    let (mut parts, largest) = server_parts(None, game, &exclude)?;
    let trash = config.state().join("trash");
    if removed::plain_repository(&trash) {
        parts.trash = tree_bytes(&trash, &[])?;
    }
    if let Some(path) = repository {
        parts.backups = tree_bytes(&path, &[])?;
    }
    let (free_bytes, total_bytes) = volume_space(game).map_err(|_| JobError::UNAVAILABLE)?;
    Ok(serde_json::json!({
        "measuredAt": super::sessions::timestamp(measured_ms), "bytes": parts.total(),
        "parts": parts, "volume": {"label":"Data drive","freeBytes":free_bytes,"totalBytes":total_bytes},
        "largest": largest
    }))
}

fn server_parts(
    dir: Option<&Path>,
    game: &Path,
    exclude: &[&Path],
) -> JobResult<(Parts, Vec<LargestFolder>)> {
    let mut largest = Vec::new();
    let mut parts = Parts::default();
    let props = game.join("server.properties");
    let world = if removed::plain_repository(game)
        && fs::symlink_metadata(&props)
            .is_ok_and(|m| removed::plain_storage_file(&m) && m.len() <= 1024 * 1024)
    {
        fs::read_to_string(&props)
            .ok()
            .and_then(|text| super::imports::properties::parse(&text).remove("level-name"))
            .filter(|name| super::imports::properties::safe_world(name))
    } else {
        None
    }
    .unwrap_or_else(|| "world".to_owned());
    let world = world.split('/').next().unwrap_or("world");
    for (root, is_game) in dir
        .map(|dir| (dir, false))
        .into_iter()
        .chain([(game, true)])
    {
        if !removed::plain_repository(root) {
            continue;
        }
        for entry in fs::read_dir(root).map_err(|_| JobError::UNAVAILABLE)? {
            let entry = entry.map_err(|_| JobError::UNAVAILABLE)?;
            let name = entry.file_name();
            let name = name.to_str().unwrap_or_default();
            let bytes = match (is_game, name) {
                (false, "game") => continue,
                (false, "private") => {
                    let private = entry.path();
                    if removed::plain_repository(&private.join("state")) {
                        let trash = private.join("state/trash");
                        parts.trash = tree_bytes(&trash, &[])?;
                        parts.other = parts.other.saturating_add(tree_bytes(&private, &[&trash])?);
                    } else {
                        parts.other = parts.other.saturating_add(tree_bytes(&private, &[])?);
                    }
                    continue;
                }
                (false, "backups") => &mut parts.backups,
                (false, "install") | (true, "libraries" | "versions") => &mut parts.software,
                (true, "logs" | "crash-reports") => &mut parts.logs,
                (true, "mods" | "plugins" | "datapacks" | "resourcepacks") => &mut parts.content,
                (true, name)
                    if name == world
                        || name
                            .strip_prefix(world)
                            .is_some_and(|suffix| matches!(suffix, "_nether" | "_the_end")) =>
                {
                    &mut parts.worlds
                }
                (true, name) if name.ends_with(".jar") => &mut parts.software,
                _ => &mut parts.other,
            };
            let path = entry.path();
            let size = if is_game {
                walk_bytes(&path, exclude, |folder, bytes| {
                    let Some(path) = folder.strip_prefix(game).ok().and_then(Path::to_str) else {
                        return;
                    };
                    let path = path.replace(std::path::MAIN_SEPARATOR, "/");
                    if crate::files::RelativePath::parse(&path).is_ok() {
                        largest.push(LargestFolder { path, bytes });
                        largest.sort_unstable_by(|a, b| {
                            b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path))
                        });
                        largest.truncate(10);
                    }
                })?
            } else {
                tree_bytes(&path, &[])?
            };
            *bytes = bytes.saturating_add(size);
        }
    }
    Ok((parts, largest))
}

pub(super) fn tree_bytes(root: &Path, exclude: &[&Path]) -> JobResult<u64> {
    walk_bytes(root, exclude, |_, _| {})
}

/// Sum folders after their children. Open directory iterators are bounded by depth.
fn walk_bytes(
    root: &Path,
    exclude: &[&Path],
    mut folder: impl FnMut(&Path, u64),
) -> JobResult<u64> {
    struct Directory {
        path: PathBuf,
        entries: fs::ReadDir,
        bytes: u64,
    }
    let mut stack: Vec<Directory> = Vec::new();
    let mut next = Some(root.to_owned());
    let mut total = 0_u64;
    loop {
        if let Some(path) = next
            .take()
            .filter(|path| !exclude.contains(&path.as_path()))
        {
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(_) => return Err(JobError::UNAVAILABLE),
            };
            if removed::plain_storage_file(&metadata) {
                total = total.saturating_add(metadata.len());
                if let Some(parent) = stack.last_mut() {
                    parent.bytes = parent.bytes.saturating_add(metadata.len());
                }
            } else if super::imports::browse::plain_directory(&metadata).is_ok() {
                #[cfg(unix)]
                if path != root && !same_volume(root, &path).unwrap_or(false) {
                    continue;
                }
                match fs::read_dir(&path) {
                    Ok(entries) => stack.push(Directory {
                        path,
                        entries,
                        bytes: 0,
                    }),
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(_) => return Err(JobError::UNAVAILABLE),
                }
            }
        }
        let Some(directory) = stack.last_mut() else {
            return Ok(total);
        };
        match directory.entries.next() {
            Some(entry) => next = Some(entry.map_err(|_| JobError::UNAVAILABLE)?.path()),
            None => {
                let directory = stack.pop().expect("current directory");
                folder(&directory.path, directory.bytes);
                if let Some(parent) = stack.last_mut() {
                    parent.bytes = parent.bytes.saturating_add(directory.bytes);
                }
            }
        }
    }
}
