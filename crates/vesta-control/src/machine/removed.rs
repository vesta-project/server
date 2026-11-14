//! Recoverable host removal. Registry tombstones preserve server identity.
use super::imports::jobs::{JobError, JobResult};
use super::{
    HostState,
    owners::{Actor, require_owner},
    state::changed,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;
const RETENTION: i64 = 7 * 24 * 60 * 60 * 1000;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveRequest {
    pub confirm_name: String,
    pub idempotency_key: Uuid,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Removed {
    pub server_id: Uuid,
    pub name: String,
    pub removed_at: String,
    pub removed_by: Uuid,
    pub restore_until: String,
    pub removed_ms: i64,
    pub restore_until_ms: i64,
}
impl Removed {
    pub fn reply(&self) -> serde_json::Value {
        serde_json::json!({"serverId":self.server_id,"removedAt":self.removed_at,"restoreUntil":self.restore_until})
    }
    fn path(&self, root: &Path) -> PathBuf {
        root.join(format!("{}-{}", self.server_id, self.removed_ms / 1000))
    }
}
fn schema(c: &rusqlite::Connection) -> crate::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS machine_removal_receipts(account TEXT NOT NULL,key TEXT NOT NULL,server TEXT NOT NULL,name TEXT NOT NULL,record TEXT NOT NULL,complete INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(account,key))")?;
    Ok(())
}
pub fn prepare(
    host: &HostState,
    account: Uuid,
    server: Uuid,
    req: &RemoveRequest,
) -> JobResult<(Removed, bool)> {
    let mut result = None;
    host.write(|c| {
        require_owner(c, Actor::Account(account))?;
        schema(c)?;
        let receipt: Option<(String,String,String,bool)> = c.query_row("SELECT server,name,record,complete FROM machine_removal_receipts WHERE account=? AND key=?", params![account.to_string(), req.idempotency_key.to_string()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some((id,name,record,complete)) = receipt {
            if id != server.to_string() || name != req.confirm_name { result=Some(Err(JobError("idempotency_conflict"))); }
            else { result=Some(serde_json::from_str(&record).map(|r| (r,complete)).map_err(|_| JobError::UNAVAILABLE)); }
            return Ok(());
        }
        let entry: Option<(String,String)> = c.query_row("SELECT name,state FROM machine_servers WHERE server_id=? AND state!='deleted'", [server.to_string()], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((name,_)) = entry else { result=Some(Err(JobError::NOT_FOUND)); return Ok(()); };
        if name != req.confirm_name { result=Some(Err(JobError("confirm_name_mismatch"))); return Ok(()); }
        let imports: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='machine_import_jobs')", [], |r| r.get(0))?;
        if imports && c.query_row("SELECT EXISTS(SELECT 1 FROM machine_import_jobs WHERE server_id=? AND state IN ('planning','applying','starting','interrupted'))", [server.to_string()], |r| r.get::<_,bool>(0))? { result=Some(Err(JobError("server_busy"))); return Ok(()); }
        let now=host.now_ms();
        let record=Removed { server_id: server, name, removed_at: super::sessions::timestamp(now), removed_by: account, restore_until: super::sessions::timestamp(now.saturating_add(RETENTION)), removed_ms: now, restore_until_ms: now.saturating_add(RETENTION) };
        let raw=serde_json::to_string(&record).map_err(|_| super::state::reject(crate::IdentityErrorCode::InvalidRequest))?;
        c.execute("INSERT INTO machine_removal_receipts(account,key,server,name,record) VALUES(?,?,?,?,?)", params![account.to_string(), req.idempotency_key.to_string(), server.to_string(), record.name, raw])?;
        result=Some(Ok((record,false)));
        Ok(())
    })?;
    result.ok_or(JobError::UNAVAILABLE)?
}
pub fn finish(
    host: &HostState,
    servers: &Path,
    record: &Removed,
    req: &RemoveRequest,
) -> JobResult<()> {
    let removed = servers
        .parent()
        .ok_or(JobError::UNAVAILABLE)?
        .join("removed");
    std::fs::create_dir_all(&removed).map_err(|_| JobError::UNAVAILABLE)?;
    let source = servers.join(record.server_id.to_string());
    let target = record.path(&removed);
    if source.exists() {
        let bytes = serde_json::to_vec_pretty(record).map_err(|_| JobError::UNAVAILABLE)?;
        std::fs::write(source.join("removed.json"), bytes).map_err(|_| JobError::UNAVAILABLE)?;
        std::fs::rename(&source, &target).map_err(|_| JobError::UNAVAILABLE)?;
    } else if !target.is_dir() {
        return Err(JobError::NOT_FOUND);
    }
    host.write(|c| {
        require_owner(c, Actor::Account(record.removed_by))?;
        c.execute(
            "UPDATE machine_servers SET state='deleted' WHERE server_id=?",
            [record.server_id.to_string()],
        )?;
        c.execute(
            "DELETE FROM machine_server_grants WHERE server_id=?",
            [record.server_id.to_string()],
        )?;
        for (table, sql) in [
            (
                "machine_invites",
                "UPDATE machine_invites SET revoked=1 WHERE server=?",
            ),
            (
                "machine_import_reservations",
                "UPDATE machine_import_reservations SET released=1 WHERE server_id=?",
            ),
        ] {
            let exists: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?)",
                [table],
                |r| r.get(0),
            )?;
            if exists {
                c.execute(sql, [record.server_id.to_string()])?;
            }
        }
        c.execute(
            "UPDATE machine_removal_receipts SET complete=1 WHERE account=? AND key=?",
            params![
                record.removed_by.to_string(),
                req.idempotency_key.to_string()
            ],
        )?;
        changed(
            c,
            true,
            "server_remove",
            &record.removed_by.to_string(),
            &record.server_id.to_string(),
            host.now_ms(),
        )?;
        Ok(())
    })?;
    Ok(())
}
pub fn list(root: &Path) -> JobResult<Vec<Removed>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(root).map_err(|_| JobError::UNAVAILABLE)? {
        let entry = entry.map_err(|_| JobError::UNAVAILABLE)?;
        if !entry
            .file_type()
            .map_err(|_| JobError::UNAVAILABLE)?
            .is_dir()
        {
            continue;
        }
        if let Ok(bytes) = std::fs::read(entry.path().join("removed.json")) {
            if let Ok(record) = serde_json::from_slice::<Removed>(&bytes) {
                if record.path(root) == entry.path() {
                    out.push(record);
                }
            }
        }
    }
    out.sort_by_key(|r| r.removed_ms);
    Ok(out)
}
pub fn purge(root: &Path, now: i64) -> JobResult<()> {
    for entry in list(root)? {
        if entry.restore_until_ms < now {
            std::fs::remove_dir_all(entry.path(root)).map_err(|_| JobError::UNAVAILABLE)?;
        }
    }
    Ok(())
}
pub fn restore(host: &HostState, servers: &Path, id: Uuid) -> JobResult<Removed> {
    let root = servers
        .parent()
        .ok_or(JobError::UNAVAILABLE)?
        .join("removed");
    let record = list(&root)?
        .into_iter()
        .rev()
        .find(|r| r.server_id == id && r.restore_until_ms >= host.now_ms())
        .ok_or(JobError::NOT_FOUND)?;
    let entry = host.server(id)?.ok_or(JobError::NOT_FOUND)?;
    let conflict=host.read(|c| Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM machine_servers WHERE state!='deleted' AND (game_port=? OR lower(name)=lower(?)))",params![entry.game_port,entry.name],|r|r.get::<_,bool>(0))?))?;
    if conflict
        || super::resources::plan_port(&Default::default(), Some(entry.game_port))
            .blocker
            .is_some()
    {
        return Err(JobError("game_port_in_use"));
    }
    let target = servers.join(id.to_string());
    if target.exists() {
        return Err(JobError("destination_conflict"));
    }
    std::fs::rename(record.path(&root), &target).map_err(|_| JobError::UNAVAILABLE)?;
    let result = host.write(|c| {
        c.execute(
            "UPDATE machine_servers SET state='stopped' WHERE server_id=? AND state='deleted'",
            [id.to_string()],
        )?;
        let reservations: bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='machine_import_reservations')",[],|r|r.get(0))?;
        if reservations { c.execute("UPDATE machine_import_reservations SET released=0 WHERE server_id=?",[id.to_string()])?; }
        changed(
            c,
            true,
            "server_restore",
            "local",
            &id.to_string(),
            host.now_ms(),
        )?;
        Ok(())
    });
    if let Err(error) = result {
        let _ = std::fs::rename(&target, record.path(&root));
        return Err(error.into());
    }
    let _ = std::fs::remove_file(target.join("removed.json"));
    Ok(record)
}
