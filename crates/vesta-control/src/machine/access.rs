//! Host capabilities are account grants independent of server memberships.
use super::{
    HostState,
    owners::{Actor, HostRole, account_name_in, holds_any_role_in, host_role_in},
    state::{account_exists, changed, expect_revision, reject, snapshot_in, uuid},
};
use crate::{IdentityErrorCode as Code, Result, identity::Capability};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use uuid::Uuid;

fn stored(c: &Connection, account: Uuid) -> Result<BTreeSet<Capability>> {
    if !account_exists(c, account)? {
        return Ok(BTreeSet::new());
    }
    let document: Option<String> = c
        .query_row(
            "SELECT document FROM machine_host_capabilities WHERE account_id=?",
            [account.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    decode_grants(document.as_deref())
}

fn decode_grants(document: Option<&str>) -> Result<BTreeSet<Capability>> {
    let Some(document) = document else {
        return Ok(BTreeSet::new());
    };
    let ids: Vec<String> = super::roles::decode(document)?;
    ids.iter()
        .map(|id| {
            Capability::from_id(id)
                .filter(|cap| cap.host_scoped() && cap.delegable())
                .ok_or_else(|| vesta_state::StateError::new("state_host_record_invalid").into())
        })
        .collect()
}

pub(super) fn has_grants_in(c: &Connection, account: Uuid) -> Result<bool> {
    Ok(!stored(c, account)?.is_empty())
}

fn allows_view(c: &Connection, account: Uuid, caps: &BTreeSet<Capability>) -> Result<bool> {
    Ok(!caps.is_empty() || holds_any_role_in(c, account)?)
}

fn capabilities_in(c: &Connection, actor: Actor) -> Result<BTreeSet<Capability>> {
    if is_owner(c, actor)? {
        return Ok(Capability::ALL
            .iter()
            .chain(Capability::HOST_MANAGEMENT)
            .copied()
            .filter(|cap| cap.host_scoped())
            .collect());
    }
    let Actor::Account(account) = actor else {
        unreachable!()
    };
    let mut caps = stored(c, account)?;
    if allows_view(c, account, &caps)? {
        caps.insert(Capability::HostView);
    }
    Ok(caps)
}

pub(super) fn is_owner(c: &Connection, actor: Actor) -> Result<bool> {
    match actor {
        Actor::LocalRecovery => Ok(true),
        Actor::Account(id) => Ok(host_role_in(c, id)?.is_some()),
    }
}

pub(super) fn require(c: &Connection, actor: Actor, cap: Capability) -> Result<()> {
    if !cap.host_scoped() || !capabilities_in(c, actor)?.contains(&cap) {
        return Err(reject(Code::Forbidden));
    }
    Ok(())
}

fn check_target(c: &Connection, actor: Actor, account: Uuid) -> Result<()> {
    if !account_exists(c, account)? {
        return Err(reject(Code::NotFound));
    }
    if host_role_in(c, account)?.is_some() {
        return Err(reject(Code::Forbidden));
    }
    if let Actor::Account(caller) = actor
        && (caller == account || !stored(c, account)?.is_subset(&capabilities_in(c, actor)?))
    {
        return Err(reject(Code::Forbidden));
    }
    Ok(())
}

fn people_row(c: &Connection, account: Uuid) -> Result<Value> {
    let role = host_role_in(c, account)?.map(|role| match role {
        HostRole::PrimaryOwner => "primary_owner",
        HostRole::Owner => "owner",
    });
    let ids: Vec<_> = stored(c, account)?.iter().map(|cap| cap.id()).collect();
    Ok(
        json!({"account_id":account,"name":account_name_in(c, account)?,"role":role,"capabilities":ids}),
    )
}

pub(super) fn host_access_sources_in(c: &Connection, account: Uuid) -> Result<bool> {
    Ok(host_role_in(c, account)?.is_some()
        || stored(c, account)?.contains(&Capability::AccessSourcesManage))
}

pub(super) fn authorize_device_revoke(c: &Connection, actor: Actor, target: Uuid) -> Result<()> {
    require(c, actor, Capability::DevicesRevoke)?;
    if !is_owner(c, actor)? {
        check_target(c, actor, target)?;
    }
    Ok(())
}

impl HostState {
    pub fn host_capabilities(&self, actor: Actor) -> Result<BTreeSet<Capability>> {
        self.read(|c| capabilities_in(c, actor))
    }

    pub fn has_host_capability(&self, actor: Actor, cap: Capability) -> Result<bool> {
        Ok(self.host_capabilities(actor)?.contains(&cap))
    }

    pub fn host_people(&self, actor: Actor) -> Result<Value> {
        self.read(|c| {
            let caps = capabilities_in(c, actor)?;
            if !caps.contains(&Capability::HostAccessManage)
                && !caps.contains(&Capability::HostMembershipsManage)
            {
                return Err(reject(Code::Forbidden));
            }
            let snapshot = snapshot_in(c)?;
            let mut q = c.prepare(
                "SELECT a.account_id,n.name,a.disabled,o.account_id IS NOT NULL,g.document
                 FROM machine_accounts a
                 LEFT JOIN machine_account_names n ON n.account_id=a.account_id
                 LEFT JOIN machine_owners o ON o.account_id=a.account_id
                 LEFT JOIN machine_host_capabilities g ON g.account_id=a.account_id AND a.disabled=0
                 ORDER BY a.account_id",
            )?;
            let mut rows = q.query([])?;
            let mut people = Vec::new();
            while let Some(row) = rows.next()? {
                let account = uuid(row.get(0)?)?;
                let name: Option<String> = row.get(1)?;
                let disabled: bool = row.get(2)?;
                let owner: bool = row.get(3)?;
                let role = if disabled {
                    None
                } else if snapshot.primary_account_id == Some(account) {
                    Some("primary_owner")
                } else {
                    owner.then_some("owner")
                };
                let grants: Option<String> = row.get(4)?;
                let ids: Vec<_> = decode_grants(grants.as_deref())?
                    .iter()
                    .map(|cap| cap.id())
                    .collect();
                people
                    .push(json!({"account_id":account,"name":name,"role":role,"capabilities":ids}));
            }
            Ok(json!({"version":snapshot.revision.to_string(),"people":people}))
        })
    }

    pub fn set_host_capabilities(
        &self,
        actor: Actor,
        account: Uuid,
        revision: u64,
        ids: &[String],
    ) -> Result<Value> {
        self.write(|c| {
            require(c, actor, Capability::HostAccessManage)?;
            expect_revision(c, revision)?;
            check_target(c, actor, account)?;
            let authority = capabilities_in(c, actor)?;
            let mut proposed = BTreeSet::new();
            for id in ids {
                let cap = Capability::from_id(id).ok_or_else(|| reject(Code::InvalidRequest))?;
                if !cap.host_scoped() || !cap.delegable() || !authority.contains(&cap) {
                    return Err(reject(Code::Forbidden));
                }
                if !proposed.insert(cap) {
                    return Err(reject(Code::InvalidRequest));
                }
            }
            if proposed.is_empty() {
                c.execute("DELETE FROM machine_host_capabilities WHERE account_id=?", [account.to_string()])?;
            } else {
                let ids: Vec<_> = proposed.iter().map(|cap| cap.id()).collect();
                c.execute(
                    "INSERT INTO machine_host_capabilities VALUES(?,?) ON CONFLICT(account_id) DO UPDATE SET document=excluded.document",
                    params![account.to_string(), super::roles::encode(&ids)?],
                )?;
            }
            let snapshot = changed(c, true, "host_capability_change", &actor.audit(), &account.to_string(), self.now_ms())?;
            Ok(json!({"version":snapshot.revision.to_string(),"person":people_row(c, account)?}))
        })
    }

    pub fn host_memberships(&self, actor: Actor, account: Uuid) -> Result<Value> {
        self.read(|c| {
            require(c, actor, Capability::HostMembershipsManage)?;
            if !account_exists(c, account)? {
                return Err(reject(Code::NotFound));
            }
            let owner = is_owner(c, actor)?;
            let mut q = c.prepare("SELECT server_id,name FROM machine_servers WHERE state!='deleted' ORDER BY server_id")?;
            let mut rows = q.query([])?;
            let mut memberships = Vec::new();
            while let Some(row) = rows.next()? {
                let server = uuid(row.get(0)?)?;
                if !owner {
                    let Actor::Account(caller) = actor else { unreachable!() };
                    if !super::roles::can_view(c, caller, server)? {
                        continue;
                    }
                }
                let role = super::registry::explicit_role_in(c, account, server)?;
                if let Some(role) = role {
                    memberships.push(json!({"server_id":server,"name":row.get::<_, String>(1)?,
                        "role_id":role.id(),"inherited":matches!(role, super::owners::EffectiveRole::Owner | super::owners::EffectiveRole::PrimaryOwner)}));
                }
            }
            Ok(json!({"version":snapshot_in(c)?.revision.to_string(),"memberships":memberships}))
        })
    }
}
