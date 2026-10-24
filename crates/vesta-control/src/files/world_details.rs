use super::*;
use crate::world::{Datapack, LevelDetails, WorldDetails, WorldDimension};
use std::{
    collections::VecDeque,
    time::{Duration, Instant, UNIX_EPOCH},
};
#[derive(Clone)]
pub(super) struct Size {
    bytes: u64,
    complete: bool,
    modified: Option<i64>,
    at: i64,
}
fn millis(time: std::time::SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
}
impl FileBrowser {
    pub(super) fn world_details(
        &self,
        subject: &Subject,
        name: &str,
        level: LevelDetails,
    ) -> Result<WorldDetails> {
        let layouts = [
            ("overworld", vec![name.to_owned()]),
            (
                "nether",
                vec![format!("{name}/DIM-1"), format!("{name}_nether/DIM-1")],
            ),
            (
                "end",
                vec![format!("{name}/DIM1"), format!("{name}_the_end/DIM1")],
            ),
        ];
        let mut dimensions = Vec::new();
        for (id, paths) in layouts {
            let folders: Vec<_> = paths
                .into_iter()
                .filter(|p| {
                    RelativePath::parse(p)
                        .ok()
                        .and_then(|p| self.root.resolve(Some(&p)).ok())
                        .is_some_and(|c| c.metadata.is_dir())
                })
                .collect();
            dimensions.push(WorldDimension {
                id: id.into(),
                present: !folders.is_empty(),
                layout: if folders.iter().any(|p| p.starts_with(&format!("{name}_"))) {
                    "split"
                } else {
                    "nested"
                }
                .into(),
                folders,
            });
        }
        let mut roots = vec![name.to_owned()];
        for suffix in ["_nether", "_the_end"] {
            let path = format!("{name}{suffix}");
            if self
                .root
                .resolve(Some(&RelativePath::parse(&path)?))
                .is_ok_and(|c| c.metadata.is_dir())
            {
                roots.push(path);
            }
        }
        roots.retain(|p| {
            RelativePath::parse(p)
                .ok()
                .and_then(|p| self.root.resolve(Some(&p)).ok())
                .is_some_and(|c| c.metadata.is_dir())
        });
        // Revalidate root read authority even for cached observations.
        for path in &roots {
            self.authorized_parent(
                subject,
                &RelativePath::parse(&format!("{path}/level.dat"))?,
                Capability::FilesRead,
            )?;
        }
        let cache_key = format!(
            "{}:{}:{name}",
            subject.principal.as_str(),
            subject.device.as_str()
        );
        let cached = self
            .world_sizes
            .lock()
            .map_err(|_| FileError::Unavailable)?
            .get(&cache_key)
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(30))
            .map(|(_, s)| s.clone());
        let size = match cached {
            Some(s) => s,
            None => {
                let start = Instant::now();
                let mut queue: VecDeque<_> = roots.into_iter().collect();
                let mut size = Size {
                    bytes: 0,
                    complete: true,
                    modified: None,
                    at: self.db.clock().millis(),
                };
                let mut scanned = 0;
                while let Some(path) = queue.pop_front() {
                    if start.elapsed() > Duration::from_millis(250) || scanned >= 20000 {
                        size.complete = false;
                        break;
                    }
                    if let Ok(checked) = self.root.resolve(Some(&RelativePath::parse(&path)?))
                        && let Some(at) = checked.metadata.modified().ok().and_then(millis)
                    {
                        size.modified = Some(size.modified.map_or(at, |old| old.max(at)));
                    }
                    let listing = match self.list(subject, Some(&RelativePath::parse(&path)?)) {
                        Ok(l) => l,
                        Err(_) => {
                            size.complete = false;
                            continue;
                        }
                    };
                    for entry in listing.entries {
                        scanned += 1;
                        if scanned > 20000 {
                            size.complete = false;
                            break;
                        }
                        if let Some(at) = entry.modified_at.and_then(millis) {
                            size.modified = Some(size.modified.map_or(at, |old| old.max(at)));
                        }
                        match entry.kind {
                            EntryKind::File => size.bytes = size.bytes.saturating_add(entry.size),
                            EntryKind::Directory => {
                                queue.push_back(format!("{path}/{}", entry.name))
                            }
                        }
                    }
                }
                let mut cache = self
                    .world_sizes
                    .lock()
                    .map_err(|_| FileError::Unavailable)?;
                if cache.len() >= 64 {
                    cache.clear();
                }
                cache.insert(cache_key, (Instant::now(), size.clone()));
                size
            }
        };
        let mut datapacks = Vec::new();
        let pack_dir = format!("{name}/datapacks");
        match self.list(subject, Some(&RelativePath::parse(&pack_dir)?)) {
            Ok(list) => {
                for entry in list.entries {
                    if entry.kind == EntryKind::File && !entry.name.ends_with(".zip") {
                        continue;
                    }
                    let id = format!("file/{}", entry.name);
                    let enabled = if level.enabled.as_ref().is_some_and(|v| v.contains(&id)) {
                        Some(true)
                    } else if level.disabled.as_ref().is_some_and(|v| v.contains(&id)) {
                        Some(false)
                    } else {
                        None
                    };
                    datapacks.push(Datapack {
                        id,
                        name: entry.name.clone(),
                        kind: if entry.kind == EntryKind::Directory {
                            "folder"
                        } else {
                            "zip"
                        }
                        .into(),
                        path: Some(format!("{pack_dir}/{}", entry.name)),
                        enabled,
                    });
                }
            }
            Err(FileError::NotFound) => {}
            Err(e) => return Err(e),
        }
        for (ids, enabled) in [(&level.enabled, true), (&level.disabled, false)] {
            if let Some(ids) = ids {
                for id in ids {
                    if !datapacks.iter().any(|p| p.id == *id) {
                        datapacks.push(Datapack {
                            id: id.clone(),
                            name: id.clone(),
                            kind: if id.starts_with("file/") {
                                "missing"
                            } else {
                                "builtin"
                            }
                            .into(),
                            path: None,
                            enabled: Some(enabled),
                        });
                    }
                }
            }
        }
        Ok(WorldDetails {
            size_bytes: Some(size.bytes.to_string()),
            size_complete: size.complete,
            size_observed_at_ms: Some(size.at),
            last_modified_at_ms: size.modified,
            last_played_at_ms: level.last_played_at_ms,
            dimensions,
            datapacks,
            datapack_state_source: "saved_level_dat".into(),
        })
    }
}
