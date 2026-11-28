//! Host-owned custom roles and effective capability overrides.
use super::{
    HostSnapshot, HostState,
    owners::{Actor, EffectiveRole, ServerRole, require_server_access_view},
    registry::{effective_role_in, explicit_role_in, require_server, server_grant_in},
    state::{account_exists, changed, expect_revision, reject, snapshot_in},
};
use crate::{IdentityErrorCode as Code, Result, identity::Capability};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

fn server_roles_in(c: &Connection, server: Uuid) -> Result<Vec<Value>> {
    role_ids_in(c, server)?
        .iter()
        .map(|id| role_summary(c, server, id))
        .collect()
}

pub(super) fn role_ids_in(c: &Connection, server: Uuid) -> Result<Vec<String>> {
    let mut ids = vec!["owner".into()];
    ids.extend(ServerRole::ALL.iter().map(|role| role.id().to_owned()));
    let mut q = c.prepare("SELECT id FROM machine_roles WHERE server_id=? ORDER BY id")?;
    let mut rows = q.query([server.to_string()])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        if Uuid::parse_str(&id).is_ok() {
            ids.push(id);
        }
    }
    Ok(ids)
}

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

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct CustomRole {
    pub name: String,
    pub rank: i32,
    pub capabilities: Vec<Grant>,
}

impl CustomRole {
    fn summary(&self, id: &str) -> Value {
        json!({"id":id,"name":self.name,"rank":self.rank,"builtin":false,"capabilities":self.capabilities})
    }

    pub(super) fn capabilities(&self) -> BTreeSet<Capability> {
        self.capabilities
            .iter()
            .filter_map(|grant| Capability::from_id(&grant.capability))
            .collect()
    }
}

pub(super) fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text)
        .map_err(|_| vesta_state::StateError::new("state_host_record_invalid").into())
}

pub(super) fn encode(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|_| vesta_state::StateError::new("state_host_record_invalid").into())
}

pub(super) fn role_record(c: &Connection, server: Uuid, id: &str) -> Result<Option<CustomRole>> {
    let document: Option<String> = c
        .query_row(
            "SELECT document FROM machine_roles WHERE server_id=? AND id=?",
            params![server.to_string(), id],
            |row| row.get(0),
        )
        .optional()?;
    document.map(|value| decode(&value)).transpose()
}

fn custom_role(c: &Connection, server: Uuid, id: &str) -> Result<CustomRole> {
    role_record(c, server, id)?.ok_or_else(|| reject(Code::NotFound))
}

fn shipped(role: ServerRole) -> CustomRole {
    from_capabilities(role.name(), role.rank(), role.capabilities().iter().copied().filter(|cap| !cap.host_scoped()))
}

fn from_capabilities(name: &str, rank: i32, caps: impl IntoIterator<Item = Capability>) -> CustomRole {
    CustomRole { name: name.into(), rank, capabilities: caps.into_iter().map(|cap| Grant {
        capability:cap.id().into(), scope:"*".into(),
    }).collect() }
}

pub(super) fn role_definition(c: &Connection, server: Uuid, id: &str) -> Result<CustomRole> {
    require_server(c, server)?;
    if id == "owner" {
        let owner = crate::identity::BuiltinRole::Owner;
        return Ok(from_capabilities(owner.name(), owner.rank(), owner.capabilities().iter().copied()
            .filter(|cap| !cap.host_scoped()).chain([Capability::LifecycleEnable])));
    }
    if let Ok(role) = ServerRole::parse(id) {
        let mut definition = role_record(c, server, role.id())?.unwrap_or_else(|| shipped(role));
        if role == ServerRole::Admin {
            definition.capabilities.push(Grant { capability: Capability::LifecycleEnable.id().into(), scope: "*".into() });
        }
        return Ok(definition);
    }
    let id = Uuid::parse_str(id).map_err(|_| reject(Code::NotFound))?;
    custom_role(c, server, &id.to_string())
}

pub(super) fn role_summary(c: &Connection, server: Uuid, id: &str) -> Result<Value> {
    let definition = role_definition(c, server, id)?;
    let builtin = ServerRole::parse(id).ok();
    let id = match builtin { Some(role) => role.id(), None => id };
    let mut value = definition.summary(id);
    value["builtin"] = json!(builtin.is_some() || id == "owner");
    value["editable"] = json!(id != "owner" && builtin != Some(ServerRole::Admin));
    value["modified"] = json!(builtin.is_some() && role_record(c, server, id)?.is_some());
    Ok(value)
}

fn require_role_fields(fields: &RoleFields) -> Result<()> {
    if fields.name.is_none() && fields.rank.is_none() && fields.capabilities.is_none() {
        return Err(reject(Code::InvalidRequest));
    }
    Ok(())
}

pub(super) fn prepare_role(fields: RoleFields, prior: Option<CustomRole>) -> Result<CustomRole> {
    prepare_fields(fields, prior, false)
}

pub(super) fn prepare_fields(fields: RoleFields, prior: Option<CustomRole>, retain_reserved: bool) -> Result<CustomRole> {
    let invalid = || reject(Code::InvalidRequest);
    let name = fields.name.or_else(|| prior.as_ref().map(|r| r.name.clone())).ok_or_else(invalid)?;
    let name = name.trim();
    if name.is_empty() || name.len() > 96 || name.chars().any(char::is_control) {
        return Err(invalid());
    }
    let rank_changed = fields.rank.is_some();
    let rank = fields.rank.or_else(|| prior.as_ref().map(|r| r.rank)).ok_or_else(invalid)?;
    if (rank_changed || !retain_reserved) && !(1..ServerRole::Admin.rank()).contains(&rank) {
        return Err(invalid());
    }
    let capabilities_changed = fields.capabilities.is_some();
    let capabilities = fields.capabilities.or_else(|| prior.map(|r| r.capabilities)).ok_or_else(invalid)?;
    if capabilities_changed || !retain_reserved {
        let mut caps = BTreeSet::new();
        for grant in &capabilities {
            if grant.scope != "*" || !caps.insert(grant_cap(&grant.capability)?) {
                return Err(invalid());
            }
        }
    }
    Ok(CustomRole { name: name.into(), rank, capabilities })
}

pub(super) fn save_role(c: &Connection, server: Uuid, id: &str, role: &CustomRole) -> Result<()> {
    c.execute("INSERT INTO machine_roles VALUES(?,?,?) ON CONFLICT(server_id,id) DO UPDATE SET document=excluded.document",
        params![server.to_string(), id, encode(role)?])?;
    Ok(())
}

pub(super) fn check_role_edit(
    c: &Connection, actor: Actor, server: Uuid, id: &str,
    prior: Option<&CustomRole>, next: &CustomRole,
) -> Result<()> {
    let auth = authority(c, actor, server)?;
    if let Some(prior) = prior {
        auth.require_role_edit()?;
        auth.delegate(prior.rank, prior.capabilities())?;
        let mut q = c.prepare("SELECT account_id FROM machine_server_grants WHERE server_id=? AND role=?")?;
        let mut rows = q.query(params![server.to_string(), id])?;
        while let Some(row) = rows.next()? {
            target(c, server, &auth, super::state::uuid(row.get(0)?)?)?;
        }
    } else {
        auth.require(Capability::RolesCreate)?;
    }
    if super::default_access::configured_role(c, server)?.as_deref() == Some(id)
        && !super::default_access::eligible(id, next)
    {
        return Err(reject(Code::Forbidden));
    }
    auth.delegate(next.rank, next.capabilities())
}

fn matches_builtin(c: &Connection, server: Uuid, role: ServerRole) -> Result<EffectiveRole> {
    Ok(match role_record(c, server, role.id())? {
        Some(record) => EffectiveRole::EditedBuiltin { role, rank: record.rank },
        None => EffectiveRole::Server(role),
    })
}

pub(super) fn resolve_role(c: &Connection, server: Uuid, id: &str) -> Result<EffectiveRole> {
    if let Ok(role) = ServerRole::parse(id) {
        return matches_builtin(c, server, role);
    }
    let uuid = Uuid::parse_str(id).map_err(|_| reject(Code::NotFound))?;
    Ok(EffectiveRole::Custom {
        id: uuid,
        rank: custom_role(c, server, &uuid.to_string())?.rank,
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
        EffectiveRole::Server(role) | EffectiveRole::EditedBuiltin { role, .. } => role_definition(c, server, role.id())?.capabilities(),
        _ => role_definition(c, server, "owner")?.capabilities(),
    })
}

fn effective(
    c: &Connection,
    server: Uuid,
    account: Uuid,
    role: EffectiveRole,
) -> Result<BTreeSet<Capability>> {
    let mut caps = base(c, server, role)?;
    if !matches!(role, EffectiveRole::Owner | EffectiveRole::PrimaryOwner)
        && server_grant_in(c, server, account)?.is_none()
    {
        caps.retain(|&cap| super::default_access::inherited_capability(cap));
        return Ok(caps);
    }
    if super::access::host_access_sources_in(c, account)? {
        caps.insert(Capability::AccessSourcesManage);
    }
    for item in overrides_in(c, server, account)? {
        let cap = Capability::from_id(&item.capability).ok_or_else(|| reject(Code::RoleInvalid))?;
        if cap.host_scoped() { continue; }
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
        caps: effective(c, server, account, role)?.into_iter().filter(|cap| !cap.host_scoped()).collect(),
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
    if auth.require(Capability::InvitationsCreate).is_err() {
        return Ok(false);
    }
    let Some(role) = role else { return Ok(true); };
    let definition = role_definition(c, server, role.id())?;
    Ok(role != ServerRole::Admin && auth.delegate(definition.rank, definition.capabilities()).is_ok())
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
    let role = explicit_role_in(c, account, server)?;
    if let Some(role) = role {
        // Include latent base authority and overrides even if a deny is active.
        auth.delegate(
            role.rank(),
            base(c, server, role)?
                .into_iter()
                .chain(effective(c, server, account, role)?.into_iter().filter(|cap| !cap.host_scoped())),
        )?;
    }
    Ok(role)
}

/// Hard capabilities are never carried by custom grants or principal overrides.
pub fn host_delegable(cap: Capability) -> bool {
    cap.delegable() && !cap.host_scoped()
}

fn grant_cap(id: &str) -> Result<Capability> {
    let cap = Capability::from_id(id).ok_or_else(|| reject(Code::InvalidRequest))?;
    if !host_delegable(cap) {
        return Err(reject(Code::Forbidden));
    }
    Ok(cap)
}

impl HostState {
    /// Build a complete policy from one Host snapshot. Device enrollment,
    /// membership, role edits and Host access grants share its generation.
    pub fn worker_policy(&self, server: Uuid) -> Result<crate::worker::WorkerPolicy> {
        use crate::worker::{ServerRole as WorkerRole, WhitelistRoles, WorkerActivityIdentity, WorkerGrant, WorkerPolicy};
        self.read(|c| {
            require_server(c, server)?;
            let snapshot = super::state::snapshot_in(c)?;
            let mut q = c.prepare(
                "SELECT d.account_id,d.device_id,d.name,n.name FROM machine_devices d
                 JOIN machine_accounts a USING(account_id) LEFT JOIN machine_account_names n USING(account_id)
                 WHERE d.revoked_at_ms IS NULL AND a.disabled=0 ORDER BY d.device_id",
            )?;
            let mut rows = q.query([])?;
            let mut grants = Vec::new();
            let mut activity_identities = BTreeMap::new();
            let mut access_sources_accounts = BTreeSet::new();
            while let Some(row) = rows.next()? {
                let account_id = super::state::uuid(row.get(0)?)?;
                let device_id = super::state::uuid(row.get(1)?)?;
                if let Some(role) = effective_role_in(c, account_id, server)? {
                    let mut effective = effective(c, server, account_id, role)?;
                    let role = WorkerRole::for_effective(role);
                    if role != WorkerRole::Owner && effective.contains(&Capability::AccessSourcesManage) {
                        access_sources_accounts.insert(account_id);
                    }
                    let mut capabilities: Vec<String> = role.capabilities()
                        .filter(|cap| effective.remove(cap)).map(|cap| cap.id().into()).collect();
                    capabilities.extend(effective.into_iter().filter(|&cap| role.permits_projection(cap))
                        .map(|cap| cap.id().into()));
                    let device_name: String = row.get(2)?;
                    let display_name = row.get::<_, Option<String>>(3)?.unwrap_or_else(|| device_name.clone());
                    activity_identities.insert(device_id, WorkerActivityIdentity { display_name, device_name });
                    grants.push(WorkerGrant { account_id, device_id, role, capabilities });
                }
            }
            let mut whitelist_roles: WhitelistRoles = role_ids_in(c, server)?
                .into_iter().map(|id| (id, BTreeMap::new())).collect();
            let mut q = c.prepare(
                "SELECT n.account_id,n.name FROM machine_account_names n JOIN machine_accounts a USING(account_id)
                 WHERE a.disabled=0 ORDER BY n.account_id",
            )?;
            let mut rows = q.query([])?;
            while let Some(row) = rows.next()? {
                let account = super::state::uuid(row.get(0)?)?;
                if let Some(role) = effective_role_in(c, account, server)?
                    && let Some(profiles) = whitelist_roles.get_mut(&role.id())
                {
                    profiles.insert(account, row.get(1)?);
                }
            }
            Ok(WorkerPolicy { host_id:snapshot.host_id, server_id:server, generation:snapshot.auth_generation,
                grants, whitelist_owners: Default::default(), whitelist_roles, activity_identities, access_sources_accounts })
        })
    }

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
                let id: String = row.get(0)?;
                if Uuid::parse_str(&id).is_ok() {
                    roles.push(role.summary(&id));
                }
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
            expect_revision(c, revision)?;
            let editing = id.is_some();
            let id = id.unwrap_or_else(Uuid::new_v4).to_string();
            let prior = editing.then(|| custom_role(c, server, &id)).transpose()?;
            require_role_fields(&fields)?;
            let role = prepare_role(fields, prior.clone())?;
            check_role_edit(c, actor, server, &id, prior.as_ref(), &role)?;
            save_role(c, server, &id, &role)?;
            Ok((changed(c, true, "server_custom_role_write", &actor.audit(), &format!("{server}/{id}"), self.now_ms())?, role.summary(&id)))
        })
    }

    pub fn server_roles(&self, server: Uuid) -> Result<Vec<Value>> {
        self.read(|c| server_roles_in(c, server))
    }

    pub fn check_server_access_view(&self, actor: Actor, server: Uuid) -> Result<()> {
        self.read(|c| require_server_access_view(c, actor, server))
    }

    pub fn server_role_list(&self, actor: Actor, server: Uuid) -> Result<Value> {
        self.read(|c| {
            require_server_access_view(c, actor, server)?;
            let roles = server_roles_in(c, server)?;
            Ok(json!({"roles":roles,"version":snapshot_in(c)?.revision.to_string()}))
        })
    }

    pub fn write_builtin_role(
        &self, actor: Actor, server: Uuid, id: &str, revision: u64, fields: Option<RoleFields>,
    ) -> Result<(HostSnapshot, Value)> {
        self.write(|c| {
            if id == "owner" { return Err(reject(Code::Forbidden)); }
            let role = ServerRole::parse(id).map_err(|_| reject(Code::NotFound))?;
            if role == ServerRole::Admin { return Err(reject(Code::Forbidden)); }
            expect_revision(c, revision)?;
            let prior = role_definition(c, server, role.id())?;
            let reset = fields.is_none();
            let next = match fields {
                Some(fields) => {
                    require_role_fields(&fields)?;
                    prepare_role(fields, Some(prior.clone()))?
                }
                None => shipped(role),
            };
            check_role_edit(c, actor, server, role.as_str(), Some(&prior), &next)?;
            // Also check pre-unification Pack User assignments.
            if role == ServerRole::PackUser {
                check_role_edit(c, actor, server, "player", Some(&prior), &next)?;
            }
            if reset {
                c.execute("DELETE FROM machine_roles WHERE server_id=? AND id=?", params![server.to_string(), role.id()])?;
            } else {
                save_role(c, server, role.id(), &next)?;
            }
            super::invites::fence_role_in(c, server, role)?;
            let snapshot = changed(c, true, "server_builtin_role_write", &actor.audit(), &format!("{server}/{}",role.id()), self.now_ms())?;
            Ok((snapshot, role_summary(c, server, role.id())?))
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
            if in_use || super::default_access::configured_role(c, server)?.as_deref() == Some(&id) {
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
        self.assign_role_impl(actor, server, account, add, remove, revision, false)
    }

    pub fn set_host_membership(
        &self, actor: Actor, account: Uuid, server: Uuid, revision: u64, role: Option<&str>,
    ) -> Result<(HostSnapshot, Option<EffectiveRole>)> {
        self.assign_role_impl(actor, server, account, role, None, revision, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn assign_role_impl(
        &self, actor: Actor, server: Uuid, account: Uuid, add: Option<&str>,
        remove: Option<&str>, revision: u64, from_host: bool,
    ) -> Result<(HostSnapshot, Option<EffectiveRole>)> {
        self.write(|c| {
            if from_host {
                super::access::require(c, actor, Capability::HostMembershipsManage)?;
            }
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
            if add == Some("owner") {
                return Err(reject(Code::Forbidden));
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
                Some(EffectiveRole::Server(role) | EffectiveRole::EditedBuiltin { role, .. }) => {
                    store_assignment(c, server, account, role.as_str())?
                }
                Some(EffectiveRole::Custom { id, .. }) => {
                    store_assignment(c, server, account, &id.to_string())?
                }
                None => {
                    super::default_access::exclude_in(c, server, account)?;
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

pub(super) fn store_assignment(c: &Connection, server: Uuid, account: Uuid, role: &str) -> Result<()> {
    super::default_access::clear_exclusion(c, server, account)?;
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
