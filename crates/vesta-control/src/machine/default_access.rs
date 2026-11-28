//! Host-owned policy for linked accounts without a server membership.
use super::{
    HostState,
    owners::{Actor, ServerRole},
    registry::{Visibility, explicit_role_in, require_server, server_in},
    roles::{CustomRole, role_definition, role_ids_in},
    state::{changed, expect_revision, reject, snapshot_in},
};
use crate::{IdentityErrorCode as Code, Result, identity::Capability};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use uuid::Uuid;

pub(super) fn inherited_capability(cap: Capability) -> bool {
    matches!(
        cap,
        Capability::PublicInfo
            | Capability::PackDownload
            | Capability::PackRealtime
            | Capability::PackAccess
    )
}

pub(super) fn eligible(id: &str, role: &CustomRole) -> bool {
    role.capabilities.iter().all(|grant| {
        grant.scope == "*"
            && Capability::from_id(&grant.capability).is_some_and(|cap| {
                // Pack User also ships with Status View for explicit members.
                inherited_capability(cap) || (id == "pack-user" && cap == Capability::StatusView)
            })
    })
}

pub(super) fn configured_role(c: &Connection, server: Uuid) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT default_role_id FROM machine_servers WHERE server_id=?",
        [server.to_string()],
        |row| row.get(0),
    )?)
}

pub(super) fn excluded_in(c: &Connection, server: Uuid, account: Uuid) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM machine_server_access_exclusions WHERE server_id=? AND account_id=?)",
        params![server.to_string(), account.to_string()],
        |row| row.get(0),
    )?)
}

pub(super) fn exclude_in(c: &Connection, server: Uuid, account: Uuid) -> Result<()> {
    c.execute(
        "DELETE FROM machine_server_grants WHERE server_id=? AND account_id=?",
        params![server.to_string(), account.to_string()],
    )?;
    c.execute(
        "INSERT OR IGNORE INTO machine_server_access_exclusions VALUES(?,?)",
        params![server.to_string(), account.to_string()],
    )?;
    Ok(())
}

pub(super) fn clear_exclusion(c: &Connection, server: Uuid, account: Uuid) -> Result<()> {
    c.execute(
        "DELETE FROM machine_server_access_exclusions WHERE server_id=? AND account_id=?",
        params![server.to_string(), account.to_string()],
    )?;
    Ok(())
}

fn authorize(c: &Connection, actor: Actor, server: Uuid) -> Result<bool> {
    require_server(c, server)?;
    if super::access::is_owner(c, actor)? {
        return Ok(true);
    }
    if let Actor::Account(account) = actor
        && super::access::host_access_sources_in(c, account)?
        && explicit_role_in(c, account, server)?.is_some()
    {
        return Ok(false);
    }
    Err(reject(Code::Forbidden))
}

fn projection(
    c: &Connection,
    server: Uuid,
    role: Option<String>,
    visibility: Visibility,
    owner: bool,
    revision: u64,
) -> Result<Value> {
    let mut eligible_roles = Vec::new();
    for id in role_ids_in(c, server)? {
        let definition = role_definition(c, server, &id)?;
        if eligible(&id, &definition) {
            eligible_roles.push(json!({"id":id,"name":definition.name}));
        }
    }
    Ok(json!({
        "version":revision.to_string(),
        "default_role_id":role,
        "eligible_roles":eligible_roles,
        "visibility":visibility,
        "can_manage_visibility":owner,
    }))
}

impl HostState {
    pub fn server_default_access(&self, actor: Actor, server: Uuid) -> Result<Value> {
        self.read(|c| {
            let owner = authorize(c, actor, server)?;
            let entry = server_in(c, server)?.ok_or_else(|| reject(Code::NotFound))?;
            projection(
                c,
                server,
                configured_role(c, server)?,
                entry.visibility,
                owner,
                snapshot_in(c)?.revision,
            )
        })
    }

    pub fn set_server_default_access(
        &self,
        actor: Actor,
        server: Uuid,
        revision: u64,
        default_role_id: Option<&str>,
        visibility: Visibility,
    ) -> Result<Value> {
        self.write(|c| {
            let owner = authorize(c, actor, server)?;
            expect_revision(c, revision)?;
            let entry = server_in(c, server)?.ok_or_else(|| reject(Code::NotFound))?;
            if entry.visibility != visibility && !owner {
                return Err(reject(Code::Forbidden));
            }
            if visibility == Visibility::Public && default_role_id.is_none() {
                return Err(reject(Code::InvalidRequest));
            }
            let role = if let Some(id) = default_role_id {
                let id = ServerRole::parse(id)
                    .map(|role| role.id().to_owned())
                    .unwrap_or_else(|_| {
                        Uuid::parse_str(id).map_or_else(|_| id.to_owned(), |uuid| uuid.to_string())
                    });
                if !eligible(&id, &role_definition(c, server, &id)?) {
                    return Err(reject(Code::Forbidden));
                }
                Some(id)
            } else {
                None
            };
            let role_changed = configured_role(c, server)? != role;
            let revision = if role_changed || entry.visibility != visibility {
                c.execute(
                    "UPDATE machine_servers SET default_role_id=?,visibility=? WHERE server_id=?",
                    params![role, visibility.as_str(), server.to_string()],
                )?;
                changed(
                    c,
                    role_changed,
                    "server_default_access",
                    &actor.audit(),
                    &server.to_string(),
                    self.now_ms(),
                )?
                .revision
            } else {
                revision
            };
            projection(c, server, role, visibility, owner, revision)
        })
    }
}
