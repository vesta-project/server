use super::{
    HostState,
    imports::jobs::{JobError, JobResult},
    owners::{Actor, require_owner},
    removed::{self, lifecycle},
    state::changed,
};
use crate::{config::BackupRepository, files::CleanupFiles, host::NetworkHost};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    targets: Vec<Target>,
    pub dry_run: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Target {
    Cache {
        cache: Cache,
    },
    Logs {
        server_id: String,
        older_than_days: u32,
    },
    Trash {
        server_id: String,
    },
    Removed {
        server_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        confirm_name: Option<String>,
    },
}

#[derive(Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cache {
    Java,
    SoftwareDownloads,
    Catalogue,
    PackStaging,
    Temp,
}

struct Item {
    target: Target,
    id: Option<Uuid>,
}

fn validate(request: &Request) -> JobResult<()> {
    if request.targets.is_empty() || request.targets.len() > 100 {
        return Err(JobError::INVALID);
    }
    let mut seen = HashMap::<_, Vec<&str>>::new();
    for target in &request.targets {
        let (kind, selector) = match target {
            Target::Cache { cache } => (
                "cache",
                match cache {
                    Cache::Java => "java",
                    Cache::SoftwareDownloads => "software_downloads",
                    Cache::Catalogue => "catalogue",
                    Cache::PackStaging => "pack_staging",
                    Cache::Temp => "temp",
                },
            ),
            Target::Logs { server_id, .. } => ("logs", server_id.as_str()),
            Target::Trash { server_id } => ("trash", server_id.as_str()),
            Target::Removed {
                server_id,
                confirm_name,
            } if (server_id == "all") == confirm_name.is_none() => ("removed", server_id.as_str()),
            _ => return Err(JobError::INVALID),
        };
        if kind != "cache"
            && selector != "all"
            && !Uuid::parse_str(selector).is_ok_and(|id| id.to_string() == selector)
        {
            return Err(JobError::INVALID);
        }
        let current = seen.entry(kind).or_default();
        if current.contains(&selector)
            || current.contains(&"all")
            || (selector == "all" && !current.is_empty())
        {
            return Err(JobError::INVALID);
        }
        current.push(selector);
    }
    Ok(())
}

fn expand(request: &Request, active: &[Uuid], removed: &[Uuid]) -> Vec<Item> {
    let mut items = Vec::new();
    for target in &request.targets {
        let (selector, candidates) = match target {
            Target::Cache { .. } => {
                items.push(Item {
                    target: target.clone(),
                    id: None,
                });
                continue;
            }
            Target::Logs { server_id, .. } | Target::Trash { server_id } => (server_id, active),
            Target::Removed { server_id, .. } => (server_id, removed),
        };
        if selector != "all" || candidates.is_empty() {
            items.push(Item {
                target: target.clone(),
                id: selector.parse().ok(),
            });
            continue;
        }
        for &id in candidates {
            let server_id = id.to_string();
            let target = match target {
                Target::Logs {
                    older_than_days, ..
                } => Target::Logs {
                    server_id,
                    older_than_days: *older_than_days,
                },
                Target::Trash { .. } => Target::Trash { server_id },
                Target::Removed { .. } => Target::Removed {
                    server_id,
                    confirm_name: None,
                },
                Target::Cache { .. } => unreachable!(),
            };
            items.push(Item {
                target,
                id: Some(id),
            });
        }
    }
    items
}

/// The host writer serializes removed-server lifecycle transitions and import
/// admission. The worker callback must only touch the separate server database.
pub fn run(
    host: &HostState,
    servers: &Path,
    java: Option<&Path>,
    account: Uuid,
    key: Option<Uuid>,
    request: &Request,
    auth_generation: u64,
    workers: &HashMap<Uuid, NetworkHost>,
) -> JobResult<Value> {
    validate(request)?;
    let input = serde_json::to_string(request).map_err(|_| JobError::INVALID)?;
    let root = servers
        .parent()
        .ok_or(JobError::UNAVAILABLE)?
        .join("removed");
    host.write(|c| {
        require_owner(c, Actor::Account(account))?;
        if super::state::snapshot_in(c)?.auth_generation != auth_generation {
            return Ok(Err(JobError::FORBIDDEN));
        }
        lifecycle::schema(c)?;
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS machine_cleanup_receipts(
            account TEXT NOT NULL, key TEXT NOT NULL, input TEXT NOT NULL, reply TEXT NOT NULL,
            PRIMARY KEY(account,key)
        )",
        )?;
        if let Some(key) = key {
            let previous: Option<(String, String)> = c
                .query_row(
                    "SELECT input,reply FROM machine_cleanup_receipts WHERE account=? AND key=?",
                    params![account.to_string(), key.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((previous, reply)) = previous {
                return Ok(if previous == input {
                    serde_json::from_str(&reply).map_err(|_| JobError::UNAVAILABLE)
                } else {
                    Err(JobError::IDEMPOTENCY)
                });
            }
        }
        let mut stmt =
            c.prepare("SELECT server_id,state FROM machine_servers ORDER BY server_id")?;
        let registry = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let active: Vec<_> = registry
            .iter()
            .filter(|(_, state)| state != "deleted")
            .filter_map(|(id, _)| id.parse().ok())
            .collect();
        let backup_roots = active_backups(servers, &active, workers);
        let records = match removed::list(&root) {
            Ok(records) => records,
            Err(error) => return Ok(Err(error)),
        };
        let mut removed = HashMap::new();
        for record in records {
            if !record.permanently
                && registry
                    .iter()
                    .any(|(id, state)| *id == record.server_id.to_string() && state == "deleted")
            {
                removed.insert(record.server_id, record);
            }
        }
        let mut removed_ids: Vec<_> = removed.keys().copied().collect();
        removed_ids.sort_unstable();
        let items = expand(request, &active, &removed_ids);
        // Validate typed names before any cleanup effects.
        for item in &items {
            if let Target::Removed {
                confirm_name: Some(name),
                ..
            } = &item.target
                && item
                    .id
                    .and_then(|id| removed.get(&id))
                    .is_some_and(|record| record.name != *name)
            {
                return Ok(Err(JobError("confirm_name_mismatch")));
            }
        }
        let now = host.now_ms();
        let mut results = Vec::new();
        let mut total = 0_u64;
        for item in items {
            let outcome = match (&item.target, item.id) {
                (Target::Cache { cache }, _) => {
                    let path = match cache {
                        Cache::Java => java.map(Path::to_path_buf),
                        Cache::PackStaging => Some(servers.join(".create-uploads")),
                        Cache::Temp => Some(servers.join(".staging")),
                        Cache::Catalogue | Cache::SoftwareDownloads => None,
                    };
                    Err(
                        if path.is_some_and(|p| std::fs::symlink_metadata(p).is_ok()) {
                            "in_use"
                        } else {
                            "not_found"
                        },
                    )
                }
                (Target::Removed { .. }, Some(id)) if removed.contains_key(&id) => {
                    let record = &removed[&id];
                    if lifecycle::purge_state(c, id)?.is_some() || host_job_busy(c, id)? {
                        Err("busy")
                    } else {
                        let path = record.path(&root);
                        let repository = if backup_roots
                            .as_ref()
                            .is_ok_and(|paths| !overlaps_any(&path, paths))
                        {
                            lifecycle::local_repository(&path, &servers.join(id.to_string()))
                        } else {
                            Err(JobError::UNAVAILABLE)
                        };
                        let bytes = repository.and_then(|repo| {
                            #[cfg(target_os = "linux")]
                            lifecycle::refuse_nested_mounts(&path)?;
                            let exclude: Vec<_> = repo.as_deref().into_iter().collect();
                            super::storage::tree_bytes(&path, &exclude)
                        });
                        match bytes {
                            Ok(bytes) => {
                                if !request.dry_run {
                                    let timestamp = super::sessions::timestamp(now);
                                    lifecycle::insert_purge(c, record, &timestamp, true)?;
                                    changed(
                                        c,
                                        false,
                                        "server_purging",
                                        &account.to_string(),
                                        &id.to_string(),
                                        now,
                                    )?;
                                }
                                Ok(bytes)
                            }
                            Err(_) => Err("in_use"),
                        }
                    }
                }
                (Target::Logs { .. } | Target::Trash { .. }, Some(id)) if active.contains(&id) => {
                    if host_job_busy(c, id)? {
                        Err("busy")
                    } else {
                        let selection = match item.target {
                            Target::Logs {
                                older_than_days, ..
                            } => {
                                let ms = now
                                    .saturating_sub(i64::from(older_than_days) * 86_400_000)
                                    .max(0);
                                CleanupFiles::Logs {
                                    cutoff: UNIX_EPOCH + Duration::from_millis(ms as u64),
                                }
                            }
                            _ => CleanupFiles::Trash,
                        };
                        workers
                            .get(&id)
                            .and_then(|worker| {
                                clean_worker(
                                    worker,
                                    selection,
                                    request.dry_run,
                                    backup_roots.as_deref().ok(),
                                )
                            })
                            .ok_or("busy")
                    }
                }
                _ => Err("not_found"),
            };
            let mut row = json!({"target":item.target,"bytes":outcome.unwrap_or(0)});
            match outcome {
                Ok(bytes) => total = total.saturating_add(bytes),
                Err(reason) => row["skipped"] = json!(reason),
            }
            results.push(row);
        }
        let reply = json!({"dryRun":request.dry_run,"items":results,"bytes":total});
        if let Some(key) = key {
            c.execute(
                "INSERT INTO machine_cleanup_receipts VALUES(?,?,?,?)",
                params![
                    account.to_string(),
                    key.to_string(),
                    input,
                    reply.to_string()
                ],
            )?;
        }
        Ok(Ok(reply))
    })?
}

fn host_job_busy(c: &Connection, server: Uuid) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM machine_import_jobs WHERE server_id=? AND state IN ('planning','applying','starting','interrupted'))",
        [server.to_string()], |row| row.get(0),
    )
}

fn active_backups(
    servers: &Path,
    active: &[Uuid],
    workers: &HashMap<Uuid, NetworkHost>,
) -> JobResult<Vec<std::path::PathBuf>> {
    use crate::config::{BackupSettings, HostConfiguration};
    active
        .iter()
        .map(|id| {
            let dir = servers.join(id.to_string());
            let settings = if let Some(host) = workers.get(id) {
                host.configuration()
                    .backups()
                    .cloned()
                    .unwrap_or_else(|| BackupSettings::for_instance(&dir))
            } else {
                match crate::files::read_local_document(&dir.join("host.toml"), 1024 * 1024) {
                    Ok(bytes) => {
                        HostConfiguration::parse_toml(&bytes, &dir, Path::new("host.toml"))
                            .map_err(|_| JobError::UNAVAILABLE)?
                            .backups()
                            .cloned()
                            .unwrap_or_else(|| BackupSettings::for_instance(&dir))
                    }
                    Err(crate::files::FileError::NotFound) => BackupSettings::for_instance(&dir),
                    Err(_) => return Err(JobError::UNAVAILABLE),
                }
            };
            Ok(match settings.repository {
                BackupRepository::Local(path) => Some(path),
                _ => None,
            })
        })
        .collect::<JobResult<Vec<_>>>()
        .map(|paths| paths.into_iter().flatten().collect())
}

fn overlaps_any(target: &Path, paths: &[std::path::PathBuf]) -> bool {
    fn overlaps(left: &Path, right: &Path) -> bool {
        #[cfg(windows)]
        {
            let left = left.to_string_lossy().to_lowercase();
            let right = right.to_string_lossy().to_lowercase();
            let (left, right) = (Path::new(&left), Path::new(&right));
            left.starts_with(right) || right.starts_with(left)
        }
        #[cfg(not(windows))]
        {
            left.starts_with(right) || right.starts_with(left)
        }
    }
    let canonical = std::fs::canonicalize(target).ok();
    paths.iter().any(|path| {
        overlaps(target, path)
            || canonical.as_ref().is_some_and(|target| {
                std::fs::canonicalize(path).is_ok_and(|path| overlaps(target, &path))
            })
    })
}

fn clean_worker(
    host: &NetworkHost,
    selection: CleanupFiles,
    dry_run: bool,
    backup_roots: Option<&[std::path::PathBuf]>,
) -> Option<u64> {
    let logs = matches!(selection, CleanupFiles::Logs { .. });
    if logs && !backup_roots.is_some_and(|paths| logs_safe(host, paths)) {
        return None;
    }
    host.files()
        .cleanup_files(selection, dry_run, || {
            !logs
                || host
                    .runtime()
                    .game_snapshot()
                    .is_ok_and(|game| game.phase == crate::operations::ProcessPhase::Stopped)
                    && host.runtime().supervision().is_ok_and(|health| {
                        health.is_none_or(|health| {
                            !health.orphan_running
                                && !health.launch_unresolved
                                && !health.observation_failed
                        })
                    })
        })
        .ok()
        .flatten()
}

fn logs_safe(host: &NetworkHost, backup_roots: &[std::path::PathBuf]) -> bool {
    let game = host.configuration().game();
    if overlaps_any(&game.join("logs"), backup_roots) {
        return false;
    }
    match crate::files::read_local_document(&game.join("server.properties"), 1024 * 1024) {
        Ok(data) => String::from_utf8(data).is_ok_and(|text| {
            !super::imports::properties::parse(&text)
                .get("level-name")
                .and_then(|name| name.split('/').next())
                .is_some_and(|name| name.eq_ignore_ascii_case("logs"))
        }),
        Err(crate::files::FileError::NotFound) => true,
        Err(_) => false,
    }
}
