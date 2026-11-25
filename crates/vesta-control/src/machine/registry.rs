use super::{
    HostSnapshot, HostState,
    owners::{Actor, EffectiveRole, HostRole, host_role_in, require_owner},
    state::{changed, expect_revision, reject, snapshot_in, uuid},
};
use crate::{IdentityErrorCode as Code, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerState {
    Registered,
    Starting,
    Online,
    Stopped,
    Sleeping,
    Failed,
    Deleted,
    Disabled,
}
impl ServerState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Starting => "starting",
            Self::Online => "online",
            Self::Stopped => "stopped",
            Self::Sleeping => "sleeping",
            Self::Failed => "failed",
            Self::Deleted => "deleted",
            Self::Disabled => "disabled",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        match s {
            "registered" => Ok(Self::Registered),
            "starting" => Ok(Self::Starting),
            "online" => Ok(Self::Online),
            "stopped" => Ok(Self::Stopped),
            "sleeping" => Ok(Self::Sleeping),
            "failed" => Ok(Self::Failed),
            "deleted" => Ok(Self::Deleted),
            _ => Err(reject(Code::InvalidRequest)),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Private,
    Public,
}
impl Visibility {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Public => "public",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerEntry {
    pub server_id: Uuid,
    pub name: String,
    pub game_port: u16,
    pub state: ServerState,
    pub visibility: Visibility,
}

impl HostState {
    /// Deleted ids remain tombstoned and can never be reused. Active names
    /// and ports are unique across all servers.
    pub fn register_server(
        &self,
        actor: Actor,
        entry: &ServerEntry,
        revision: u64,
    ) -> Result<HostSnapshot> {
        self.write(|c| {
            super::access::require(c,actor,crate::identity::Capability::HostServersCreate)?; expect_revision(c,revision)?; validate(entry)?;
            if entry.server_id==snapshot_in(c)?.host_id || entry.state==ServerState::Deleted || entry.visibility==Visibility::Public { return Err(reject(Code::InvalidRequest)); }
            let conflict: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM machine_servers WHERE state!='deleted' AND (lower(name)=lower(?1) OR game_port=?2))", params![entry.name, entry.game_port], |r|r.get(0))?;
            let used=server_in(c,entry.server_id)?.is_some();
            if conflict || used { return Err(reject(Code::Conflict)); }
            c.execute("INSERT INTO machine_servers(server_id,name,game_port,state,visibility) VALUES(?1,?2,?3,?4,?5)",params![entry.server_id.to_string(),entry.name,entry.game_port,entry.state.as_str(),entry.visibility.as_str()])?;
            super::startup::register_in(c, entry.server_id)?;
            super::presets::register_in(c, entry.server_id)?;
            changed(c,true,"server_register",&actor.audit(),&entry.server_id.to_string(),self.now_ms())
        })
    }
    pub fn update_server(
        &self,
        actor: Actor,
        entry: &ServerEntry,
        revision: u64,
    ) -> Result<HostSnapshot> {
        self.write(|c| {
            require_owner(c,actor)?; expect_revision(c,revision)?; validate(entry)?; require_server(c,entry.server_id)?;
            if entry.visibility == Visibility::Public && super::default_access::configured_role(c, entry.server_id)?.is_none() {
                return Err(reject(Code::InvalidRequest));
            }
            let conflict: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM machine_servers WHERE state!='deleted' AND server_id!=?1 AND (lower(name)=lower(?2) OR game_port=?3))", params![entry.server_id.to_string(),entry.name,entry.game_port], |r|r.get(0))?;
            if entry.state != ServerState::Deleted && conflict { return Err(reject(Code::Conflict)); }
            c.execute("UPDATE machine_servers SET name=?1,game_port=?2,state=?3,visibility=?4 WHERE server_id=?5",params![entry.name,entry.game_port,entry.state.as_str(),entry.visibility.as_str(),entry.server_id.to_string()])?;
            changed(c,true,"server_update",&actor.audit(),&entry.server_id.to_string(),self.now_ms())
        })
    }
    /// Host-observed runtime state (starting/online/stopped/failed) from the
    /// server's own worker. Trusted host input only, never request data. This is
    /// not an authority change: it advances the revision but not the
    /// authorization generation, so live sessions and worker subjects stay
    /// valid. Returns whether the stored state changed. Deleted entries and the
    /// first-slice `registered` state before any start are left alone.
    pub fn observe_server_state(&self, id: Uuid, observed: ServerState) -> Result<bool> {
        if matches!(observed, ServerState::Deleted | ServerState::Registered | ServerState::Disabled) {
            return Err(reject(Code::InvalidRequest));
        }
        let mut updated = false;
        self.write(|c| {
            let Some(entry) = server_in(c, id)? else {
                return Ok(());
            };
            if entry.state == observed
                || matches!(entry.state, ServerState::Deleted | ServerState::Disabled)
                || (entry.state == ServerState::Registered && observed == ServerState::Stopped)
            {
                return Ok(());
            }
            c.execute(
                "UPDATE machine_servers SET state=?1 WHERE server_id=?2",
                params![observed.as_str(), id.to_string()],
            )?;
            changed(
                c,
                false,
                "server_state",
                "host",
                &id.to_string(),
                self.now_ms(),
            )?;
            updated = true;
            Ok(())
        })?;
        Ok(updated)
    }
    /// Whether another active server on this machine already has `port` as its game port.
    pub fn game_port_taken(&self, id: Uuid, port: u16) -> Result<bool> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM machine_servers WHERE state!='deleted' AND server_id!=?1 AND game_port=?2)",
                params![id.to_string(), port],
                |r| r.get(0),
            )?)
        })
    }
    /// The server's own settings changed its game port (trusted host input, after the worker
    /// accepted the edit). Keeps the registry, which allocates ports for new servers, in step.
    /// Refuses a port another active server holds; not an authority change.
    pub fn observe_game_port(&self, id: Uuid, port: u16) -> Result<()> {
        if port == 0 {
            return Err(reject(Code::InvalidRequest));
        }
        self.write(|c| {
            let Some(entry) = server_in(c, id)? else {
                return Ok(());
            };
            if entry.game_port == port || entry.state == ServerState::Deleted {
                return Ok(());
            }
            let taken: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM machine_servers WHERE state!='deleted' AND server_id!=?1 AND game_port=?2)",
                params![id.to_string(), port],
                |r| r.get(0),
            )?;
            if taken {
                return Err(reject(Code::Conflict));
            }
            c.execute(
                "UPDATE machine_servers SET game_port=?1 WHERE server_id=?2",
                params![port, id.to_string()],
            )?;
            changed(c, false, "server_port", "host", &id.to_string(), self.now_ms())?;
            Ok(())
        })
    }
    pub fn authorize_enablement(&self, actor: Actor, server: Uuid, revision: u64) -> Result<()> {
        self.read(|c| {
            super::roles::require_capability(c, actor, server, crate::identity::Capability::LifecycleEnable)?;
            expect_revision(c, revision)
        })
    }

    pub fn set_server_enabled(
        &self, actor: Actor, server: Uuid, enabled: bool, revision: Option<u64>,
    ) -> Result<HostSnapshot> {
        self.write(|c| {
            super::roles::require_capability(c, actor, server, crate::identity::Capability::LifecycleEnable)?;
            if let Some(revision) = revision {
                expect_revision(c, revision)?;
            }
            let entry = server_in(c, server)?.ok_or_else(|| reject(Code::NotFound))?;
            if enabled == (entry.state != ServerState::Disabled) {
                return snapshot_in(c);
            }
            c.execute("UPDATE machine_servers SET enabled=?,state=CASE WHEN ? THEN 'stopped' ELSE state END WHERE server_id=?",
                params![enabled, enabled, server.to_string()])?;
            changed(c, false, "server_enablement", &actor.audit(), &server.to_string(), self.now_ms())
        })
    }

    /// Trusted registry lookup, not an authorization check for an HTTP caller.
    pub fn server(&self, id: Uuid) -> Result<Option<ServerEntry>> {
        self.read(|c| server_in(c, id))
    }
    pub fn effective_role(&self, account: Uuid, server: Uuid) -> Result<Option<EffectiveRole>> {
        self.read(|c| effective_role_in(c, account, server))
    }
    pub fn public_server_at_port(&self, port: u16) -> Result<Option<(Uuid, Uuid)>> {
        self.read(|c| {
            let entry = c.query_row(
                "SELECT h.host_id,s.server_id FROM machine_servers s CROSS JOIN machine_host h
                 WHERE s.game_port=? AND s.state!='deleted' AND s.enabled=1
                   AND s.visibility='public' AND s.default_role_id IS NOT NULL",
                [port],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            ).optional()?;
            entry.map(|(host, server)| Ok((uuid(host)?, uuid(server)?))).transpose()
        })
    }
    pub fn visible_servers(&self, account: Uuid) -> Result<Vec<(ServerEntry, EffectiveRole)>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT server_id FROM machine_servers WHERE state!='deleted' ORDER BY server_id",
            )?;
            let ids = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut entries = Vec::new();
            for id in ids {
                let id = uuid(id)?;
                if let Some(role) = effective_role_in(c, account, id)? {
                    if let Some(entry) = server_in(c, id)? {
                        entries.push((entry, role));
                    }
                }
            }
            Ok(entries)
        })
    }
}
fn validate(entry: &ServerEntry) -> Result<()> {
    if entry.server_id.is_nil()
        || entry.state == ServerState::Disabled
        || entry.game_port == 0
        || entry.name.trim().is_empty()
        || entry.name.len() > 128
    {
        return Err(reject(Code::InvalidRequest));
    }
    Ok(())
}
pub(crate) fn require_server(c: &Connection, id: Uuid) -> Result<()> {
    if server_in(c, id)?.is_none_or(|s| s.state == ServerState::Deleted) {
        return Err(reject(Code::NotFound));
    }
    Ok(())
}
pub fn effective_role_in(
    c: &Connection,
    account: Uuid,
    server: Uuid,
) -> Result<Option<EffectiveRole>> {
    resolve_access_in(c, account, server, true)
}

pub(super) fn explicit_role_in(
    c: &Connection,
    account: Uuid,
    server: Uuid,
) -> Result<Option<EffectiveRole>> {
    resolve_access_in(c, account, server, false)
}

fn resolve_access_in(
    c: &Connection,
    account: Uuid,
    server: Uuid,
    inherit_default: bool,
) -> Result<Option<EffectiveRole>> {
    if server_in(c, server)?.is_none_or(|s| s.state == ServerState::Deleted)
        || !super::state::account_exists(c, account)?
    {
        return Ok(None);
    }
    match host_role_in(c, account)? {
        Some(HostRole::PrimaryOwner) => return Ok(Some(EffectiveRole::PrimaryOwner)),
        Some(HostRole::Owner) => return Ok(Some(EffectiveRole::Owner)),
        None => {}
    }
    if let Some(role) = server_grant_in(c, server, account)? {
        return Ok(Some(super::roles::resolve_role(c, server, &role)?));
    }
    if inherit_default
        && let Some(role) = super::default_access::configured_role(c, server)?
        && !super::default_access::excluded_in(c, server, account)?
    {
        return Ok(Some(super::roles::resolve_role(c, server, &role)?));
    }
    Ok(None)
}

pub(super) fn server_grant_in(c: &Connection, server: Uuid, account: Uuid) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT role FROM machine_server_grants WHERE server_id=? AND account_id=?",
        params![server.to_string(), account.to_string()],
        |row| row.get(0),
    ).optional()?)
}

pub(super) fn server_in(c: &Connection, id: Uuid) -> Result<Option<ServerEntry>> {
    let row: Option<(String, u16, String, String, bool)> = c
        .query_row(
            "SELECT name,game_port,state,visibility,enabled FROM machine_servers WHERE server_id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    row.map(|(name, port, state, visibility, enabled)| {
        Ok(ServerEntry {
            server_id: id,
            name,
            game_port: port,
            state: if enabled || state == "deleted" {
                ServerState::parse(&state)?
            } else {
                ServerState::Disabled
            },
            visibility: match visibility.as_str() {
                "private" => Visibility::Private,
                "public" => Visibility::Public,
                _ => return Err(reject(Code::InvalidRequest)),
            },
        })
    })
    .transpose()
}
