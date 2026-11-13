use super::{
    HostSnapshot, HostState,
    owners::{Actor, EffectiveRole, HostRole, ServerRole, host_role_in, require_owner},
    state::{changed, expect_revision, reject, snapshot_in, uuid},
};
use crate::{IdentityErrorCode as Code, Result};
use rusqlite::{Connection, OptionalExtension, params};
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Public,
}
impl Visibility {
    fn as_str(self) -> &'static str {
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
    /// One active server in the first slice. Deleted ids remain tombstoned and
    /// can never be reused; increasing capacity later changes no identity/API.
    pub fn register_server(
        &self,
        actor: Actor,
        entry: &ServerEntry,
        revision: u64,
    ) -> Result<HostSnapshot> {
        self.write(|c| {
            require_owner(c,actor)?; expect_revision(c,revision)?; validate(entry)?;
            if entry.server_id==snapshot_in(c)?.host_id || entry.state==ServerState::Deleted { return Err(reject(Code::InvalidRequest)); }
            let count:i64=c.query_row("SELECT count(*) FROM machine_servers WHERE state!='deleted'",[],|r|r.get(0))?;
            let used=server_in(c,entry.server_id)?.is_some();
            if count>=1 || used { return Err(reject(Code::Conflict)); }
            c.execute("INSERT INTO machine_servers(server_id,name,game_port,state,visibility) VALUES(?1,?2,?3,?4,?5)",params![entry.server_id.to_string(),entry.name,entry.game_port,entry.state.as_str(),entry.visibility.as_str()])?;
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
            c.execute("UPDATE machine_servers SET name=?1,game_port=?2,state=?3,visibility=?4 WHERE server_id=?5",params![entry.name,entry.game_port,entry.state.as_str(),entry.visibility.as_str(),entry.server_id.to_string()])?;
            changed(c,true,"server_update",&actor.audit(),&entry.server_id.to_string(),self.now_ms())
        })
    }
    /// Trusted registry lookup, not an authorization check for an HTTP caller.
    pub fn server(&self, id: Uuid) -> Result<Option<ServerEntry>> {
        self.read(|c| server_in(c, id))
    }
    pub fn effective_role(&self, account: Uuid, server: Uuid) -> Result<Option<EffectiveRole>> {
        self.read(|c| effective_role_in(c, account, server))
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
    let role: Option<String> = c
        .query_row(
            "SELECT role FROM machine_server_grants WHERE server_id=?1 AND account_id=?2",
            params![server.to_string(), account.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    role.map(|s| ServerRole::parse(&s).map(EffectiveRole::Server))
        .transpose()
}
fn server_in(c: &Connection, id: Uuid) -> Result<Option<ServerEntry>> {
    let row: Option<(String, u16, String, String)> = c
        .query_row(
            "SELECT name,game_port,state,visibility FROM machine_servers WHERE server_id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    row.map(|(name, port, state, visibility)| {
        Ok(ServerEntry {
            server_id: id,
            name,
            game_port: port,
            state: ServerState::parse(&state)?,
            visibility: match visibility.as_str() {
                "private" => Visibility::Private,
                "public" => Visibility::Public,
                _ => return Err(reject(Code::InvalidRequest)),
            },
        })
    })
    .transpose()
}
