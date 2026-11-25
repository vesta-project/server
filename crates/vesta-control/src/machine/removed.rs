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
pub(super) mod lifecycle;
#[cfg(target_os = "linux")]
pub(crate) use lifecycle::refuse_nested_mounts;
pub use lifecycle::{
    PurgeRequest, RestoreRequest, list_page, purge_expired, queue_purge, reconcile, restore_remote,
};

pub(crate) fn retained_root(path: &Path) -> Option<std::fs::ReadDir> {
    lifecycle::plain_path(path).ok()?;
    std::fs::read_dir(path).ok()
}

pub(super) fn plain_storage_file(metadata: &std::fs::Metadata) -> bool {
    lifecycle::plain_file(metadata)
}

pub(crate) fn plain_repository(path: &Path) -> bool {
    lifecycle::plain_path(path).is_ok()
}

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveRequest {
    pub confirm_name: String,
    pub idempotency_key: Uuid,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub permanently: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_backups: Option<bool>,
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
    #[serde(default)]
    pub permanently: bool,
    #[serde(default = "keep_backups_by_default")]
    pub keep_backups: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge_bytes: Option<u64>,
}
fn keep_backups_by_default() -> bool {
    true
}
impl Removed {
    pub fn reply(&self) -> serde_json::Value {
        serde_json::json!({"serverId":self.server_id,"removedAt":self.removed_at,"restoreUntil":self.restore_until})
    }
    pub fn purge_reply(&self) -> serde_json::Value {
        serde_json::json!({"serverId":self.server_id,"purgeStartedAt":self.removed_at,"bytes":self.purge_bytes})
    }
    pub(super) fn path(&self, root: &Path) -> PathBuf {
        root.join(format!("{}-{}", self.server_id, self.removed_ms / 1000))
    }
}
fn schema(c: &rusqlite::Connection) -> crate::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS machine_removal_receipts(account TEXT NOT NULL,key TEXT NOT NULL,server TEXT NOT NULL,name TEXT NOT NULL,record TEXT NOT NULL,complete INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(account,key))")?;
    Ok(())
}
/// Look for an interrupted request without creating or modifying a receipt.
pub fn has_pending(host: &HostState, server: Uuid) -> JobResult<bool> {
    Ok(host.read(|c| {
        let exists: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='machine_removal_receipts')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(false);
        }
        Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM machine_removal_receipts WHERE server=? AND complete=0)",
            [server.to_string()],
            |row| row.get(0),
        )?)
    })?)
}

pub fn prepare(
    host: &HostState,
    account: Uuid,
    server: Uuid,
    req: &RemoveRequest,
    measured_bytes: Option<u64>,
) -> JobResult<(Removed, bool)> {
    if !req.permanently && req.keep_backups.is_some() {
        return Err(JobError::INVALID);
    }
    let mut result = None;
    host.write(|c| {
        super::access::require(c, Actor::Account(account), crate::identity::Capability::HostServersRemove)?;
        schema(c)?;
        let receipt: Option<(String,String,String,bool)> = c.query_row("SELECT server,name,record,complete FROM machine_removal_receipts WHERE account=? AND key=?", params![account.to_string(), req.idempotency_key.to_string()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let pending = if let Some((id,name,record,complete)) = receipt {
            if id != server.to_string() || name != req.confirm_name {
                result=Some(Err(JobError("idempotency_conflict"))); return Ok(());
            }
            let record: Removed=serde_json::from_str(&record).map_err(|_| super::state::reject(crate::IdentityErrorCode::InvalidRequest))?;
            if record.permanently != req.permanently
                || record.keep_backups != req.keep_backups.unwrap_or(true)
            {
                result = Some(Err(JobError::IDEMPOTENCY));
                return Ok(());
            }
            if complete { result=Some(Ok((record,true))); return Ok(()); }
            Some(record)
        } else { None };
        if !super::access::is_owner(c, Actor::Account(account))?
            && (req.permanently || super::registry::explicit_role_in(c, account, server)?.is_none())
        {
            return Err(super::state::reject(crate::IdentityErrorCode::Forbidden));
        }
        let entry: Option<(String,String)> = c.query_row("SELECT name,state FROM machine_servers WHERE server_id=? AND state!='deleted'", [server.to_string()], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((name,_)) = entry else { result=Some(Err(JobError("not_found"))); return Ok(()); };
        if name != req.confirm_name { result=Some(Err(JobError("confirm_name_mismatch"))); return Ok(()); }
        let imports: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='machine_import_jobs')", [], |r| r.get(0))?;
        if imports && c.query_row("SELECT EXISTS(SELECT 1 FROM machine_import_jobs WHERE server_id=? AND state IN ('planning','applying','starting','interrupted'))", [server.to_string()], |r| r.get::<_,bool>(0))? { result=Some(Err(JobError("server_busy"))); return Ok(()); }
        if let Some(record)=pending { result=Some(Ok((record,false))); return Ok(()); }
        let now=host.now_ms();
        let retention = i64::from(super::settings::removal_retention_in(c)?) * DAY_MS;
        let record=Removed { server_id: server, name, removed_at: super::sessions::timestamp(now), removed_by: account, restore_until: super::sessions::timestamp(now.saturating_add(retention)), removed_ms: now, restore_until_ms: now.saturating_add(retention), permanently: req.permanently, keep_backups: req.keep_backups.unwrap_or(true), purge_bytes: measured_bytes.filter(|_| req.permanently) };
        let raw=serde_json::to_string(&record).map_err(|_| super::state::reject(crate::IdentityErrorCode::InvalidRequest))?;
        c.execute("INSERT INTO machine_removal_receipts(account,key,server,name,record) VALUES(?,?,?,?,?)", params![account.to_string(), req.idempotency_key.to_string(), server.to_string(), record.name, raw])?;
        result=Some(Ok((record,false)));
        Ok(())
    })?;
    result.ok_or(JobError::UNAVAILABLE)?
}
/// Reject an unmanaged or missing server directory before stopping its worker.
pub fn check_removable_path(servers: &Path, record: &Removed) -> JobResult<()> {
    let source = servers.join(record.server_id.to_string());
    if source.exists() {
        return lifecycle::plain_path(&source);
    }
    let root = servers
        .parent()
        .ok_or(JobError::UNAVAILABLE)?
        .join("removed");
    let target = record.path(&root);
    if target.exists() {
        return lifecycle::plain_path(&target);
    }
    Err(JobError("not_found"))
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
    lifecycle::ensure_directory(&removed)?;
    let source = servers.join(record.server_id.to_string());
    let target = record.path(&removed);
    if source.exists() {
        lifecycle::plain_path(&source)?;
        if target.exists() {
            return Err(JobError("destination_conflict"));
        }
        let bytes = serde_json::to_vec_pretty(record).map_err(|_| JobError::UNAVAILABLE)?;
        lifecycle::write_atomic(&source.join("removed.json"), &bytes)?;
        std::fs::rename(&source, &target).map_err(|_| JobError::UNAVAILABLE)?;
    } else if !target.is_dir() {
        return Err(JobError("not_found"));
    }
    let result = host.write(|c| {
        super::access::require(c, Actor::Account(record.removed_by), crate::identity::Capability::HostServersRemove)?;
        let updated = c.execute(
            "UPDATE machine_servers SET state='deleted',visibility='private',default_role_id=NULL
             WHERE server_id=? AND name=? AND state!='deleted'",
            params![record.server_id.to_string(), record.name],
        )?;
        if updated != 1 {
            return Err(super::state::reject(crate::IdentityErrorCode::Conflict));
        }
        if record.permanently {
            lifecycle::insert_purge(c, record, &record.removed_at, record.keep_backups)?;
        }
        c.execute("DELETE FROM machine_roles WHERE server_id=?", [record.server_id.to_string()])?;
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
            "DELETE FROM machine_removal_receipts WHERE server=? AND complete=0 AND NOT (account=? AND key=?)",
            params![record.server_id.to_string(), record.removed_by.to_string(), req.idempotency_key.to_string()],
        )?;
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
    });
    if let Err(error) = result {
        let _ = std::fs::rename(&target, &source);
        return Err(error.into());
    }
    Ok(())
}
pub fn list(root: &Path) -> JobResult<Vec<Removed>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    lifecycle::plain_path(root)?;
    for entry in std::fs::read_dir(root).map_err(|_| JobError::UNAVAILABLE)? {
        let entry = entry.map_err(|_| JobError::UNAVAILABLE)?;
        if !entry
            .file_type()
            .map_err(|_| JobError::UNAVAILABLE)?
            .is_dir()
        {
            continue;
        }
        let manifest = entry.path().join("removed.json");
        if lifecycle::plain_path(&entry.path()).is_err()
            || !std::fs::symlink_metadata(&manifest)
                .is_ok_and(|m| lifecycle::plain_file(&m) && m.len() <= 64 * 1024)
        {
            continue;
        }
        if let Ok(bytes) = std::fs::read(manifest) {
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
pub fn restore(host: &HostState, servers: &Path, id: Uuid) -> JobResult<Removed> {
    lifecycle::restore_local(host, servers, id)
}
