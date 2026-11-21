//! Host-owned custom roles and effective capability overrides.
use super::{
    HostSnapshot, HostState,
    owners::{Actor, EffectiveRole, ServerRole},
    registry::{effective_role_in, require_server},
    state::{account_exists, changed, expect_revision, reject},
};
use crate::{IdentityErrorCode as Code, Result, identity::Capability};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub capability: String,
    pub scope: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Allow,
    Deny,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Override {
    pub capability: String,
    pub effect: Effect,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleFields {
    pub name: Option<String>,
    pub rank: Option<i32>,
    pub capabilities: Option<Vec<Grant>>,
}

#[derive(Serialize, Deserialize)]
struct CustomRole {
    name: String,
    rank: i32,
    capabilities: Vec<Grant>,
}

impl CustomRole {
    fn summary(&self, id: &str) -> Value {
        json!({"id":id,"name":self.name,"rank":self.rank,"builtin":false,"capabilities":self.capabilities})
    }

    fn capabilities(&self) -> BTreeSet<Capability> {
        self.capabilities
            .iter()
            .filter_map(|grant| Capability::from_id(&grant.capability))
            .collect()
    }
}

fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text)
        .map_err(|_| vesta_state::StateError::new("state_host_record_invalid").into())
}

fn encode(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|_| vesta_state::StateError::new("state_host_record_invalid").into())
}

fn custom_role(c: &Connection, server: Uuid, id: &str) -> Result<CustomRole> {
    let document: Option<String> = c
        .query_row(
            "SELECT document FROM machine_roles WHERE server_id=? AND id=?",
            params![server.to_string(), id],
            |row| row.get(0),
        )
        .optional()?;
    decode(&document.ok_or_else(|| reject(Code::NotFound))?)
}

pub(super) fn resolve_role(c: &Connection, server: Uuid, id: &str) -> Result<EffectiveRole> {
    if let Ok(role) = ServerRole::parse(id) {
        return Ok(EffectiveRole::Server(role));
    }
    let uuid = Uuid::parse_str(id).map_err(|_| reject(Code::NotFound))?;
    Ok(EffectiveRole::Custom {
        id: uuid,
        rank: custom_role(c, server, id)?.rank,
    })
}

fn overrides_in(c: &Connection, server: Uuid, account: Uuid) -> Result<Vec<Override>> {
    let document: Option<String> = c
        .query_row(
            "SELECT document FROM machine_capability_overrides WHERE server_id=? AND account_id=?",
            params![server.to_string(), account.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    document
        .map(|s| decode(&s))
        .transpose()
        .map(Option::unwrap_or_default)
}

pub(super) fn clear_overrides(c: &Connection, server: Uuid, account: Uuid) -> Result<()> {
    c.execute(
        "DELETE FROM machine_capability_overrides WHERE server_id=? AND account_id=?",
        params![server.to_string(), account.to_string()],
    )?;
    Ok(())
}

fn base(c: &Connection, server: Uuid, role: EffectiveRole) -> Result<BTreeSet<Capability>> {
    Ok(match role {
        EffectiveRole::Custom { id, .. } => custom_role(c, server, &id.to_string())?.capabilities(),
        EffectiveRole::Server(role) => role.capabilities().iter().copied().collect(),
        _ => crate::identity::BuiltinRole::Owner.capabilities().iter().copied().collect(),
    })
}

fn effective(
    c: &Connection,
    server: Uuid,
    account: Uuid,
    role: EffectiveRole,
) -> Result<BTreeSet<Capability>> {
    let mut caps = base(c, server, role)?;
    for item in overrides_in(c, server, account)? {
        let cap = Capability::from_id(&item.capability).ok_or_else(|| reject(Code::RoleInvalid))?;
        match item.effect {
            Effect::Allow => {
                caps.insert(cap);
            }
            Effect::Deny => {
                caps.remove(&cap);
            }
        }
    }
    Ok(caps)
}

struct Authority {
    owner: bool,
    rank: i32,
    caps: BTreeSet<Capability>,
}

impl Authority {
    fn require(&self, cap: Capability) -> Result<()> {
        if self.owner || self.caps.contains(&cap) {
            Ok(())
        } else {
            Err(reject(Code::Forbidden))
        }
    }

    fn require_role_edit(&self) -> Result<()> {
        self.require(Capability::RolesManage)
            .or_else(|_| self.require(Capability::RolesCreate))
    }

    fn delegate(&self, rank: i32, caps: impl IntoIterator<Item = Capability>) -> Result<()> {
        if rank >= self.rank
            || (!self.owner
                && caps
                    .into_iter()
                    .any(|cap| !cap.delegable() || !self.caps.contains(&cap)))
        {
            return Err(reject(Code::Forbidden));
        }
        Ok(())
    }
}

fn authority(c: &Connection, actor: Actor, server: Uuid) -> Result<Authority> {
    require_server(c, server)?;
    let Actor::Account(account) = actor else {
        return Ok(Authority {
            owner: true,
            rank: 100,
            caps: BTreeSet::new(),
        });
    };
    let role = effective_role_in(c, account, server)?.ok_or_else(|| reject(Code::Forbidden))?;
    Ok(Authority {
        owner: matches!(role, EffectiveRole::PrimaryOwner | EffectiveRole::Owner),
        rank: role.rank(),
        caps: effective(c, server, account, role)?,
    })
}

pub(super) fn require_capability(
    c: &Connection,
    actor: Actor,
    server: Uuid,
    cap: Capability,
) -> Result<()> {
    authority(c, actor, server)?.require(cap)
}

pub(super) fn can_view(c: &Connection, account: Uuid, server: Uuid) -> Result<bool> {
    if effective_role_in(c, account, server)?.is_none() {
        return Ok(false);
    }
    let auth = authority(c, Actor::Account(account), server)?;
    Ok(auth.owner
        || [
            Capability::PrincipalsView,
            Capability::RolesCreate,
            Capability::RolesManage,
            Capability::RolesAssign,
            Capability::RolesRevoke,
        ]
        .iter()
        .any(|cap| auth.caps.contains(cap)))
}

pub(super) fn can_invite_builtin(
    c: &Connection,
    actor: Actor,
    server: Uuid,
    role: Option<ServerRole>,
) -> Result<bool> {
    let auth = authority(c, actor, server)?;
    Ok(auth.require(Capability::InvitationsCreate).is_ok()
        && role.is_none_or(|role| {
            role != ServerRole::Admin
                && auth
                    .delegate(role.rank(), role.capabilities().iter().copied())
                    .is_ok()
        }))
}

fn target(
    c: &Connection,
    server: Uuid,
    auth: &Authority,
    account: Uuid,
) -> Result<Option<EffectiveRole>> {
    if !account_exists(c, account)? {
        return Err(reject(Code::NotFound));
    }
    let role = effective_role_in(c, account, server)?;
    if let Some(role) = role {
        // Include latent base authority and overrides even if a deny is active.
        auth.delegate(
            role.rank(),
            base(c, server, role)?
                .into_iter()
                .chain(effective(c, server, account, role)?),
        )?;
    }
    Ok(role)
}

/// Hard capabilities are never carried by custom grants or principal overrides.
pub fn host_delegable(cap: Capability) -> bool {
    cap.delegable()
        && !matches!(
            cap,
            Capability::AccessSourcesManage | Capability::DevicesRevoke
        )
}

fn grant_cap(id: &str) -> Result<Capability> {
    let cap = Capability::from_id(id).ok_or_else(|| reject(Code::InvalidRequest))?;
    if !host_delegable(cap) {
        return Err(reject(Code::Forbidden));
    }
    Ok(cap)
}

impl HostState {
    pub fn server_capabilities(
        &self,
        server: Uuid,
        account: Uuid,
        role: EffectiveRole,
    ) -> Result<BTreeSet<Capability>> {
        self.read(|c| {
            if effective_role_in(c, account, server)? != Some(role) {
                return Err(reject(Code::Forbidden));
            }
            effective(c, server, account, role)
        })
    }

    pub fn custom_roles(&self, server: Uuid) -> Result<Vec<Value>> {
        self.read(|c| {
            let mut q =
                c.prepare("SELECT id,document FROM machine_roles WHERE server_id=? ORDER BY id")?;
            let mut rows = q.query([server.to_string()])?;
            let mut roles = Vec::new();
            while let Some(row) = rows.next()? {
                let role: CustomRole = decode(&row.get::<_, String>(1)?)?;
                roles.push(role.summary(&row.get::<_, String>(0)?));
            }
            Ok(roles)
        })
    }

    pub fn capability_overrides(&self, server: Uuid, account: Uuid) -> Result<Vec<Override>> {
        self.read(|c| overrides_in(c, server, account))
    }

    pub fn write_custom_role(
        &self,
        actor: Actor,
        server: Uuid,
        id: Option<Uuid>,
        revision: u64,
        fields: RoleFields,
    ) -> Result<(HostSnapshot, Value)> {
        self.write(|c| {
            let auth = authority(c, actor, server)?;
            if id.is_some() { auth.require_role_edit()?; } else { auth.require(Capability::RolesCreate)?; }
            expect_revision(c, revision)?;
            if fields.name.is_none() && fields.rank.is_none() && fields.capabilities.is_none() {
                return Err(reject(Code::InvalidRequest));
            }
            let editing = id.is_some();
            let id = id.unwrap_or_else(Uuid::new_v4).to_string();
            let prior = editing.then(|| custom_role(c, server, &id)).transpose()?;
            if let Some(role) = &prior {
                auth.delegate(role.rank, role.capabilities())?;
                let mut q = c.prepare("SELECT account_id FROM machine_server_grants WHERE server_id=? AND role=?")?;
                let mut rows = q.query(params![server.to_string(), id])?;
                while let Some(row) = rows.next()? {
                    target(c, server, &auth, super::state::uuid(row.get(0)?)?)?;
                }
            }
            let invalid = || reject(Code::InvalidRequest);
            let name = fields.name.or_else(|| prior.as_ref().map(|r| r.name.clone())).ok_or_else(invalid)?;
            let name = name.trim();
            if name.is_empty() || name.len() > 96 || name.chars().any(char::is_control) {
                return Err(invalid());
            }
            let rank = fields.rank.or_else(|| prior.as_ref().map(|r| r.rank)).ok_or_else(invalid)?;
            if !(1..ServerRole::Admin.rank()).contains(&rank) {
                return Err(invalid());
            }
            let grants = fields.capabilities.or_else(|| prior.map(|r| r.capabilities)).ok_or_else(invalid)?;
            let mut caps = BTreeSet::new();
            for grant in &grants {
                if grant.scope != "*" || !caps.insert(grant_cap(&grant.capability)?) {
                    return Err(invalid());
                }
            }
            auth.delegate(rank, caps)?;
            let role = CustomRole { name: name.into(), rank, capabilities: grants };
            c.execute("INSERT INTO machine_roles VALUES(?,?,?) ON CONFLICT(server_id,id) DO UPDATE SET document=excluded.document",
                params![server.to_string(), id, encode(&role)?])?;
            Ok((changed(c, true, "server_custom_role_write", &actor.audit(), &format!("{server}/{id}"), self.now_ms())?, role.summary(&id)))
        })
    }

    pub fn delete_custom_role(
        &self,
        actor: Actor,
        server: Uuid,
        id: Uuid,
        revision: u64,
    ) -> Result<HostSnapshot> {
        self.write(|c| {
            let auth = authority(c, actor, server)?;
            auth.require_role_edit()?;
            expect_revision(c, revision)?;
            let id = id.to_string();
            let role = custom_role(c, server, &id)?;
            auth.delegate(role.rank, role.capabilities())?;
            let in_use: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM machine_server_grants WHERE server_id=? AND role=?)",
                params![server.to_string(), id],
                |row| row.get(0),
            )?;
            if in_use {
                return Err(reject(Code::RoleInUse));
            }
            c.execute(
                "DELETE FROM machine_roles WHERE server_id=? AND id=?",
                params![server.to_string(), id],
            )?;
            changed(
                c,
                true,
                "server_custom_role_delete",
                &actor.audit(),
                &format!("{server}/{id}"),
                self.now_ms(),
            )
        })
    }

    pub fn assign_role(
        &self,
        actor: Actor,
        server: Uuid,
        account: Uuid,
        add: Option<&str>,
        remove: Option<&str>,
        revision: u64,
    ) -> Result<(HostSnapshot, Option<EffectiveRole>)> {
        self.write(|c| {
            let auth = authority(c, actor, server)?;
            auth.require(if add.is_some() {
                Capability::RolesAssign
            } else {
                Capability::RolesRevoke
            })?;
            expect_revision(c, revision)?;
            let current = target(c, server, &auth, account)?;
            if let Some(id) = remove {
                resolve_role(c, server, id)?;
            }
            let role = add.map(|id| resolve_role(c, server, id)).transpose()?;
            if let Some(role) = role {
                if role == EffectiveRole::Server(ServerRole::Admin)
                    && !matches!(actor, Actor::LocalRecovery)
                {
                    return Err(reject(Code::Forbidden));
                }
                auth.delegate(role.rank(), base(c, server, role)?)?;
            }
            if current.is_some() && current != role {
                auth.require(Capability::RolesRevoke)?;
                clear_overrides(c, server, account)?;
            }
            match role {
                Some(EffectiveRole::Server(role)) => {
                    store_assignment(c, server, account, role.as_str())?
                }
                Some(EffectiveRole::Custom { id, .. }) => {
                    store_assignment(c, server, account, &id.to_string())?
                }
                None => {
                    c.execute(
                        "DELETE FROM machine_server_grants WHERE server_id=? AND account_id=?",
                        params![server.to_string(), account.to_string()],
                    )?;
                }
                _ => return Err(reject(Code::Forbidden)),
            }
            Ok((
                changed(
                    c,
                    true,
                    "server_role_change",
                    &actor.audit(),
                    &format!("{server}/{account}"),
                    self.now_ms(),
                )?,
                role,
            ))
        })
    }

    pub fn set_capability_overrides(
        &self,
        actor: Actor,
        server: Uuid,
        account: Uuid,
        revision: u64,
        overrides: &[Override],
    ) -> Result<(HostSnapshot, EffectiveRole)> {
        self.write(|c| {
            let auth = authority(c, actor, server)?;
            auth.require(Capability::RolesManage)?;
            expect_revision(c, revision)?;
            let role = target(c, server, &auth, account)?.ok_or_else(|| reject(Code::NotFound))?;
            let mut caps = BTreeSet::new();
            for item in overrides {
                if !caps.insert(grant_cap(&item.capability)?) {
                    return Err(reject(Code::InvalidRequest));
                }
            }
            auth.delegate(role.rank(), caps)?;
            c.execute("INSERT INTO machine_capability_overrides VALUES(?,?,?) ON CONFLICT(server_id,account_id) DO UPDATE SET document=excluded.document",
                params![server.to_string(), account.to_string(), encode(&overrides)?])?;
            Ok((changed(c, true, "server_capability_overrides", &actor.audit(), &format!("{server}/{account}"), self.now_ms())?, role))
        })
    }
}

fn store_assignment(c: &Connection, server: Uuid, account: Uuid, role: &str) -> Result<()> {
    c.execute("INSERT INTO machine_server_grants VALUES(?,?,?) ON CONFLICT(server_id,account_id) DO UPDATE SET role=excluded.role",
        params![server.to_string(), account.to_string(), role])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::registry::{ServerEntry, ServerState, Visibility};
    use std::sync::Arc;

    #[test]
    fn host_custom_role_grants_are_versioned_and_fenced() {
        let tmp = tempfile::tempdir().unwrap();
        let state =
            HostState::open(tmp.path().join("host"), Arc::new(vesta_state::SystemClock)).unwrap();
        let owner = state.claim(Uuid::new_v4(), "Owner", [2; 32]).unwrap();
        let actor = Actor::Account(owner.account_id);
        let target = Uuid::new_v4();
        state
            .write(|c| HostState::enroll_device_in(c, target, "Other", [3; 32], state.now_ms()))
            .unwrap();
        let server = Uuid::new_v4();
        let mut snapshot = state
            .register_server(
                actor,
                &ServerEntry {
                    server_id: server,
                    name: "Fixture".into(),
                    game_port: 31009,
                    state: ServerState::Stopped,
                    visibility: Visibility::Private,
                },
                state.snapshot().unwrap().revision,
            )
            .unwrap();
        let (created, role) = state
            .write_custom_role(
                actor,
                server,
                None,
                snapshot.revision,
                RoleFields {
                    name: Some("File readers".into()),
                    rank: Some(30),
                    capabilities: Some(vec![Grant {
                        capability: "files.read".into(),
                        scope: "*".into(),
                    }]),
                },
            )
            .unwrap();
        let id = role["id"].as_str().unwrap();
        assert_eq!(
            state
                .assign_role(actor, server, target, Some(id), None, snapshot.revision)
                .unwrap_err()
                .code(),
            "conflict"
        );
        let (assigned, effective) = state
            .assign_role(actor, server, target, Some(id), None, created.revision)
            .unwrap();
        let effective = effective.unwrap();
        assert!(
            state
                .server_capabilities(server, target, effective)
                .unwrap()
                .contains(&Capability::FilesRead)
        );
        assert_eq!(
            state
                .delete_custom_role(actor, server, id.parse().unwrap(), assigned.revision)
                .unwrap_err()
                .code(),
            "role_in_use"
        );
        (snapshot, _) = state
            .set_capability_overrides(
                actor,
                server,
                target,
                assigned.revision,
                &[Override {
                    capability: "files.read".into(),
                    effect: Effect::Deny,
                }],
            )
            .unwrap();
        assert!(snapshot.auth_generation > assigned.auth_generation);
        assert!(
            !state
                .server_capabilities(server, target, effective)
                .unwrap()
                .contains(&Capability::FilesRead)
        );
        assert_eq!(
            state
                .assign_role(
                    Actor::Account(target),
                    server,
                    target,
                    Some(id),
                    None,
                    snapshot.revision
                )
                .unwrap_err()
                .code(),
            "forbidden"
        );
    }
}
