use super::*;
use crate::{
    config::{BackupRepository, BackupSettings, HostConfiguration},
    machine::{
        imports::{browse::plain_directory, jobs},
        registry::ServerState,
        resources::{self, Reservations},
        state::reject,
    },
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, io::Write};

const PAGE_SIZE: usize = 100;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoreRequest {
    pub name: Option<String>,
    pub game_port: Option<u16>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PurgeRequest {
    pub confirm_name: String,
    pub keep_backups: Option<bool>,
}

fn root(servers: &Path) -> JobResult<PathBuf> {
    Ok(servers
        .parent()
        .ok_or(JobError::UNAVAILABLE)?
        .join("removed"))
}

fn schema(c: &Connection) -> crate::Result<()> {
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS machine_server_purges(
            server TEXT PRIMARY KEY, record TEXT NOT NULL, started_at TEXT NOT NULL,
            keep_backups INTEGER NOT NULL, complete INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS machine_removed_actions(
            account TEXT NOT NULL, action TEXT NOT NULL, key TEXT NOT NULL,
            server TEXT NOT NULL, input TEXT NOT NULL, reply TEXT NOT NULL,
            PRIMARY KEY(account,action,key)
        );",
    )?;
    Ok(())
}

pub(super) fn insert_purge(
    c: &Connection,
    record: &Removed,
    started_at: &str,
    keep_backups: bool,
) -> crate::Result<()> {
    schema(c)?;
    let raw = serde_json::to_string(record)
        .map_err(|_| reject(crate::IdentityErrorCode::InvalidRequest))?;
    c.execute(
        "INSERT INTO machine_server_purges(server,record,started_at,keep_backups) VALUES(?,?,?,?)",
        params![record.server_id.to_string(), raw, started_at, keep_backups],
    )?;
    Ok(())
}

fn table_exists(c: &Connection, name: &str) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?)",
        [name],
        |r| r.get(0),
    )
}

fn purge_state(c: &Connection, id: Uuid) -> crate::Result<Option<bool>> {
    Ok(c.query_row(
        "SELECT complete FROM machine_server_purges WHERE server=?",
        [id.to_string()],
        |r| r.get(0),
    )
    .optional()?)
}

fn encode(value: &impl Serialize) -> crate::Result<String> {
    serde_json::to_string(value).map_err(|_| reject(crate::IdentityErrorCode::InvalidRequest))
}

fn replay(
    c: &Connection,
    account: Uuid,
    operation: &str,
    key: Uuid,
    id: Uuid,
    input: &str,
) -> crate::Result<Option<JobResult<Value>>> {
    let existing: Option<(String, String, String)> = c.query_row(
        "SELECT server,input,reply FROM machine_removed_actions WHERE account=? AND action=? AND key=?",
        params![account.to_string(), operation, key.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional()?;
    Ok(existing.map(|(server, previous, reply)| {
        if server != id.to_string() || previous != input {
            return Err(JobError::IDEMPOTENCY);
        }
        serde_json::from_str(&reply).map_err(|_| JobError::UNAVAILABLE)
    }))
}

fn save_reply(
    c: &Connection,
    account: Uuid,
    operation: &str,
    key: Uuid,
    id: Uuid,
    input: &str,
    reply: &Value,
) -> crate::Result<()> {
    c.execute(
        "INSERT INTO machine_removed_actions(account, action, key, server, input, reply) VALUES(?,?,?,?,?,?)",
        params![account.to_string(), operation, key.to_string(), id.to_string(), input, encode(reply)?],
    )?;
    Ok(())
}

fn removed_record(root: &Path, id: Uuid) -> JobResult<Removed> {
    list(root)?
        .into_iter()
        .rev()
        .find(|r| r.server_id == id && !r.permanently)
        .ok_or(JobError("not_found"))
}

pub fn list_page(
    host: &HostState,
    servers: &Path,
    account: Uuid,
    cursor: Option<&str>,
) -> JobResult<Value> {
    let before = cursor
        .map(|cursor| {
            let (time, id) = cursor.split_once('.').ok_or(JobError::INVALID)?;
            Ok::<_, JobError>((
                time.parse::<i64>().map_err(|_| JobError::INVALID)?,
                Uuid::parse_str(id).map_err(|_| JobError::INVALID)?,
            ))
        })
        .transpose()?;
    let root = root(servers)?;
    if host.host_role(account)?.is_none() {
        return Err(JobError::FORBIDDEN);
    }
    let mut records = list(&root)?;
    host.write(|c| {
        require_owner(c, Actor::Account(account))?;
        schema(c)?;
        let mut statement = c.prepare("SELECT record FROM machine_server_purges WHERE complete=0")?;
        let pending = statement.query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for raw in pending {
            let record: Removed = serde_json::from_str(&raw)
                .map_err(|_| reject(crate::IdentityErrorCode::InvalidRequest))?;
            records.push(record);
        }
        records.sort_unstable_by_key(|r| std::cmp::Reverse((r.removed_ms, r.server_id)));
        records.dedup_by_key(|r| (r.removed_ms, r.server_id));
        let now = host.now_ms();
        let mut page = Vec::new();
        let mut last = None;
        let mut more = false;
        for record in records {
            let key = (record.removed_ms, record.server_id);
            if record.permanently || before.is_some_and(|before| key >= before) {
                continue;
            }
            if !c.query_row("SELECT EXISTS(SELECT 1 FROM machine_servers WHERE server_id=? AND state='deleted')",
                [record.server_id.to_string()], |r| r.get::<_, bool>(0))? {
                continue;
            }
            let purge = purge_state(c, record.server_id)?;
            if purge == Some(true) {
                continue;
            }
            if page.len() == PAGE_SIZE {
                more = true;
                break;
            }
            last = Some(format!("{}.{}", key.0, key.1));
            let state = if purge.is_some() || record.restore_until_ms <= now { "purging" } else { "restorable" };
            page.push(json!({"serverId":record.server_id,"name":record.name,"removedAt":record.removed_at,
                "restoreUntil":record.restore_until,"removedBy":record.removed_by,
                "state":state,"bytes":null,"minecraftVersion":null,"software":null}));
        }
        Ok(json!({"servers":page,"nextCursor":last.filter(|_| more)}))
    }).map_err(Into::into)
}

pub fn queue_purge(
    host: &HostState,
    servers: &Path,
    account: Uuid,
    id: Uuid,
    key: Uuid,
    request: &PurgeRequest,
) -> JobResult<Value> {
    let root = root(servers)?;
    let input = encode(request)?;
    host.write(|c| {
        require_owner(c, Actor::Account(account))?;
        schema(c)?;
        if let Some(reply) = replay(c, account, "purge", key, id, &input)? {
            return Ok(reply);
        }
        if let Some(complete) = purge_state(c, id)? {
            return Ok(Err(JobError(if complete { "gone" } else { "purging" })));
        }
        let record = match removed_record(&root, id) {
            Ok(record) => record,
            Err(error) => return Ok(Err(error)),
        };
        let deleted = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM machine_servers WHERE server_id=? AND state='deleted')",
            [id.to_string()],
            |r| r.get::<_, bool>(0),
        )?;
        if !deleted {
            return Ok(Err(JobError("not_found")));
        }
        if record.name != request.confirm_name {
            return Ok(Err(JobError("confirm_name_mismatch")));
        }
        let now = host.now_ms();
        let started_at = super::super::sessions::timestamp(now);
        insert_purge(
            c,
            &record,
            &started_at,
            request.keep_backups.unwrap_or(true),
        )?;
        changed(
            c,
            false,
            "server_purging",
            &account.to_string(),
            &id.to_string(),
            now,
        )?;
        let reply = json!({"serverId":id,"purgeStartedAt":started_at,"bytes":null});
        save_reply(c, account, "purge", key, id, &input, &reply)?;
        Ok(Ok(reply))
    })?
}

pub fn restore_remote(
    host: &HostState,
    servers: &Path,
    account: Uuid,
    id: Uuid,
    key: Uuid,
    request: &RestoreRequest,
) -> JobResult<Value> {
    restore_inner(host, servers, id, Some((account, key)), request).map(|(_, value)| value)
}

pub(super) fn restore_local(host: &HostState, servers: &Path, id: Uuid) -> JobResult<Removed> {
    restore_inner(
        host,
        servers,
        id,
        None,
        &RestoreRequest {
            name: None,
            game_port: None,
        },
    )?
    .0
    .ok_or(JobError::UNAVAILABLE)
}

fn restore_inner(
    host: &HostState,
    servers: &Path,
    id: Uuid,
    receipt: Option<(Uuid, Uuid)>,
    request: &RestoreRequest,
) -> JobResult<(Option<Removed>, Value)> {
    let root = root(servers)?;
    let input = encode(request)?;
    let target = servers.join(id.to_string());
    let mut moved_from = None;
    let result = host.write(|c| {
        schema(c)?;
        if let Some((account, key)) = receipt {
            require_owner(c, Actor::Account(account))?;
            if let Some(reply) = replay(c, account, "restore", key, id, &input)? {
                return Ok(reply.map(|reply| (None, reply)));
            }
        }
        if let Some(complete) = purge_state(c, id)? {
            return Ok(Err(JobError(if complete { "gone" } else { "purging" })));
        }
        let Some(entry) = super::super::registry::server_in(c, id)? else {
            return Ok(Err(JobError("not_found")));
        };
        if entry.state != ServerState::Deleted {
            return Ok(Err(JobError("not_found")));
        }
        let mut record = match removed_record(&root, id) {
            Ok(record) => record,
            Err(JobError("not_found")) => return Ok(Err(JobError("gone"))),
            Err(error) => return Ok(Err(error)),
        };
        if record.restore_until_ms <= host.now_ms() {
            return Ok(Err(JobError("purging")));
        }
        let base = request.name.as_deref().map(str::trim).unwrap_or(&record.name);
        if base.is_empty() || base.len() > 128 || request.game_port == Some(0) {
            return Ok(Err(JobError::INVALID));
        }
        let reservations_table = table_exists(c, "machine_import_reservations")?;
        let mut name = base.to_owned();
        for suffix in 1.. {
            let used = if reservations_table {
                jobs::name_in_use(c, &name).map_err(|_| reject(crate::IdentityErrorCode::InvalidRequest))?
            } else {
                c.query_row("SELECT EXISTS(SELECT 1 FROM machine_servers WHERE state!='deleted' AND lower(name)=lower(?))",
                    [&name], |r| r.get::<_, bool>(0))?
            };
            if !used {
                break;
            }
            let extra = if suffix == 1 { " (restored)".into() } else { format!(" (restored {suffix})") };
            let limit = base.floor_char_boundary(128 - extra.len());
            name = format!("{}{extra}", base[..limit].trim_end());
        }
        let reservations = if reservations_table {
            jobs::reservations_in(c, None).map_err(|_| reject(crate::IdentityErrorCode::InvalidRequest))?
        } else {
            let mut stmt = c.prepare("SELECT game_port FROM machine_servers WHERE state!='deleted'")?;
            let ports = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            Reservations { ports, ..Default::default() }
        };
        let mut plan = resources::plan_port(&reservations, request.game_port.or(Some(entry.game_port)));
        if request.game_port.is_none() && plan.blocker.is_some() {
            plan = resources::plan_port(&reservations, None);
        }
        if let Some(blocker) = plan.blocker {
            return Ok(Err(JobError(blocker)));
        }
        let port = plan.game_port.expect("resolved port");
        let source = record.path(&root);
        if target.exists() {
            return Ok(Err(JobError("destination_conflict")));
        }
        if plain_path(&source).is_err() || fs::rename(&source, &target).is_err() {
            return Ok(Err(JobError::UNAVAILABLE));
        }
        moved_from = Some(source);
        if let Err(error) = set_game_port(&target, port) {
            return Ok(Err(error));
        }
        c.execute("UPDATE machine_servers SET state='stopped',name=?,game_port=? WHERE server_id=?",
            params![name, port, id.to_string()])?;
        if reservations_table {
            c.execute("UPDATE machine_import_reservations SET released=0,game_port=? WHERE server_id=?",
                params![port, id.to_string()])?;
            c.execute("UPDATE machine_import_jobs SET request=json_set(request,'$.name',?) WHERE server_id=? AND json_valid(request)",
                params![name, id.to_string()])?;
        }
        let now = host.now_ms();
        let actor = receipt.map_or_else(|| "local".to_owned(), |(a, _)| a.to_string());
        changed(c, true, "server_restore", &actor, &id.to_string(), now)?;
        let reply = json!({"serverId":id,"name":name,"gamePort":port,"restoredAt":super::super::sessions::timestamp(now)});
        if let Some((account, key)) = receipt {
            save_reply(c, account, "restore", key, id, &input, &reply)?;
        }
        record.name = name;
        Ok(Ok((Some(record), reply)))
    });
    match (result, moved_from) {
        (Ok(Ok(value)), Some(_)) => {
            let _ = fs::remove_file(target.join("removed.json"));
            Ok(value)
        }
        (result, source) => {
            if let Some(source) = source {
                let _ = fs::rename(&target, source);
            }
            result?
        }
    }
}

fn set_game_port(dir: &Path, port: u16) -> JobResult<()> {
    let game = dir.join("game");
    if !game.exists() && !dir.join("host.toml").exists() {
        return Ok(());
    }
    plain_path(&game)?;
    let properties = game.join("server.properties");
    let mut contents = match fs::symlink_metadata(&properties) {
        Ok(metadata) if plain_file(&metadata) && metadata.len() <= 1024 * 1024 => {
            fs::read(&properties).map_err(|_| JobError::UNAVAILABLE)?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        _ => return Err(JobError::UNAVAILABLE),
    };
    // Java Properties uses the last occurrence. The blank line also ends a
    // continued logical line. Keep the other settings and their original bytes.
    contents.extend_from_slice(format!("\n\nserver-port={port}\n").as_bytes());
    write_atomic(&properties, &contents)
}

pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> JobResult<()> {
    let temp = path.with_file_name(format!(".vesta-{}", Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::File::create_new(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|_| JobError::UNAVAILABLE)
}

pub(super) fn plain_path(path: &Path) -> JobResult<()> {
    for component in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
        let metadata = fs::symlink_metadata(component).map_err(|_| JobError::UNAVAILABLE)?;
        plain_directory(&metadata).map_err(|_| JobError::UNAVAILABLE)?;
    }
    Ok(())
}

pub(super) fn ensure_directory(path: &Path) -> JobResult<()> {
    plain_path(path.parent().ok_or(JobError::UNAVAILABLE)?)?;
    if let Err(error) = fs::create_dir(path) {
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(JobError::UNAVAILABLE);
        }
    }
    plain_path(path)
}

pub(super) fn plain_file(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    metadata.is_file()
}

/// Recover the filesystem side of a transition interrupted before SQLite commit.
pub fn reconcile(host: &HostState, servers: &Path) -> JobResult<()> {
    let removed = root(servers)?;
    for record in list(&removed)? {
        if host
            .server(record.server_id)?
            .is_some_and(|entry| entry.state != ServerState::Deleted)
        {
            let target = servers.join(record.server_id.to_string());
            if !target.exists() {
                plain_path(servers)?;
                fs::rename(record.path(&removed), &target).map_err(|_| JobError::UNAVAILABLE)?;
                let _ = fs::remove_file(target.join("removed.json"));
            }
        }
    }
    let Ok(entries) = fs::read_dir(servers) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry.map_err(|_| JobError::UNAVAILABLE)?;
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| Uuid::parse_str(name).ok())
        else {
            continue;
        };
        let dir = entry.path();
        let manifest = dir.join("removed.json");
        if plain_path(&dir).is_err()
            || !fs::symlink_metadata(&manifest)
                .is_ok_and(|m| plain_file(&m) && m.len() <= 64 * 1024)
        {
            continue;
        }
        let record: Removed =
            serde_json::from_slice(&fs::read(&manifest).map_err(|_| JobError::UNAVAILABLE)?)
                .map_err(|_| JobError::UNAVAILABLE)?;
        if record.server_id != id {
            continue;
        }
        if host
            .server(id)?
            .is_some_and(|entry| entry.state == ServerState::Deleted)
        {
            ensure_directory(&removed)?;
            let target = record.path(&removed);
            if target.exists() {
                return Err(JobError("destination_conflict"));
            }
            fs::rename(dir, target).map_err(|_| JobError::UNAVAILABLE)?;
        } else {
            fs::remove_file(manifest).map_err(|_| JobError::UNAVAILABLE)?;
        }
    }
    Ok(())
}

/// The host transport calls this from its background janitor. Queue admission
/// and restore use the same database writer. Files are deleted outside it so a
/// large world cannot hold the host write transaction open.
pub fn purge_expired(host: &HostState, servers: &Path, now: i64) -> JobResult<()> {
    let root = root(servers)?;
    let expired = list(&root)?
        .into_iter()
        .filter(|r| r.restore_until_ms <= now);
    let pending = host.write(|c| {
        schema(c)?;
        for record in expired {
            let deleted = super::super::registry::server_in(c, record.server_id)?
                .is_some_and(|e| e.state == ServerState::Deleted);
            if deleted && purge_state(c, record.server_id)?.is_none() {
                insert_purge(c, &record, &super::super::sessions::timestamp(now), true)?;
                changed(c, false, "server_purging", "host", &record.server_id.to_string(), now)?;
            }
        }
        let mut stmt = c.prepare("SELECT record, keep_backups FROM machine_server_purges WHERE complete=0 ORDER BY started_at")?;
        let result = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(result)
    })?;
    let mut outcome = Ok(());
    for (raw, keep_backups) in pending {
        let record: Removed = serde_json::from_str(&raw).map_err(|_| JobError::UNAVAILABLE)?;
        if let Err(error) =
            purge_files(servers, &record.path(&root), record.server_id, keep_backups)
        {
            outcome = Err(error);
            continue;
        }
        host.write(|c| {
            c.execute(
                "UPDATE machine_server_purges SET complete=1 WHERE server=?",
                [record.server_id.to_string()],
            )?;
            changed(
                c,
                false,
                "server_purged",
                "host",
                &record.server_id.to_string(),
                now,
            )?;
            Ok(())
        })?;
    }
    outcome
}

fn purge_files(servers: &Path, source: &Path, id: Uuid, keep_backups: bool) -> JobResult<()> {
    match fs::symlink_metadata(source) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(JobError::UNAVAILABLE),
        Ok(_) => plain_path(source)?,
    }
    #[cfg(target_os = "linux")]
    refuse_nested_mounts(source)?;
    if keep_backups {
        let retained = servers
            .parent()
            .ok_or(JobError::UNAVAILABLE)?
            .join("retained-backups");
        let destination = retained.join(id.to_string());
        if destination.exists() {
            // A previous pass preserved the repository before deleting the tree.
            plain_path(&destination)?;
        } else if let Some(repository) = local_repository(source, &servers.join(id.to_string()))? {
            ensure_directory(&retained)?;
            fs::rename(repository, destination).map_err(|_| JobError::UNAVAILABLE)?;
        }
    }
    fs::remove_dir_all(source).map_err(|_| JobError::UNAVAILABLE)
}

fn local_repository(source: &Path, original: &Path) -> JobResult<Option<PathBuf>> {
    let path = source.join("host.toml");
    let settings = match fs::symlink_metadata(&path) {
        Ok(metadata) if plain_file(&metadata) && metadata.len() <= 1024 * 1024 => {
            let bytes = fs::read(path).map_err(|_| JobError::UNAVAILABLE)?;
            let config = HostConfiguration::parse_toml(&bytes, source, Path::new("host.toml"))
                .map_err(|_| JobError::UNAVAILABLE)?;
            config
                .backups()
                .cloned()
                .unwrap_or_else(|| BackupSettings::for_instance(source))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            BackupSettings::for_instance(source)
        }
        _ => return Err(JobError::UNAVAILABLE),
    };
    let BackupRepository::Local(path) = settings.repository else {
        return Ok(None);
    };
    let plain_original = jobs::plain_managed_path(original);
    let Ok(relative) = path
        .strip_prefix(source)
        .or_else(|_| path.strip_prefix(original))
        .or_else(|_| path.strip_prefix(plain_original))
    else {
        return Ok(None);
    };
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(JobError("unsafe_path"));
    }
    let path = source.join(relative);
    match fs::symlink_metadata(&path) {
        Ok(_) => plain_path(&path).map(|()| Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(JobError::UNAVAILABLE),
    }
}

#[cfg(target_os = "linux")]
fn refuse_nested_mounts(root: &Path) -> JobResult<()> {
    use std::io::{BufRead, BufReader};
    use std::os::unix::ffi::OsStrExt;
    let root = fs::canonicalize(root).map_err(|_| JobError::UNAVAILABLE)?;
    let file = fs::File::open("/proc/self/mountinfo").map_err(|_| JobError::UNAVAILABLE)?;
    for line in BufReader::new(file).split(b'\n') {
        let line = line.map_err(|_| JobError::UNAVAILABLE)?;
        let field = line
            .split(|b| *b == b' ')
            .nth(4)
            .ok_or(JobError::UNAVAILABLE)?;
        let mut bytes = Vec::with_capacity(field.len());
        let mut rest = field;
        while let Some((&first, tail)) = rest.split_first() {
            rest = tail;
            if first == b'\\' {
                let [a @ b'0'..=b'3', b @ b'0'..=b'7', c @ b'0'..=b'7', tail @ ..] = rest else {
                    return Err(JobError::UNAVAILABLE);
                };
                bytes.push((a - b'0') * 64 + (b - b'0') * 8 + (c - b'0'));
                rest = tail;
            } else {
                bytes.push(first);
            }
        }
        if Path::new(std::ffi::OsStr::from_bytes(&bytes)).starts_with(&root) {
            return Err(JobError("unsafe_path"));
        }
    }
    Ok(())
}
