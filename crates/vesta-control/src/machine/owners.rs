use super::{
    HostSnapshot, HostState,
    state::{account_exists, changed, expect_revision, reject, snapshot_in, uuid},
};
use crate::{
    IdentityErrorCode as Code, Result,
    identity::{BuiltinRole, Capability},
};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

/// Supplied only after session authentication or privileged local peer checks.
/// Constructing this value is not authentication. Signature/freshness checks for
/// owner changes and transfer belong to the calling host authority adapter.
#[derive(Clone, Copy, Debug)]
pub enum Actor {
    Account(Uuid),
    LocalRecovery,
}
impl Actor {
    pub(crate) fn audit(self) -> String {
        match self {
            Self::Account(id) => id.to_string(),
            Self::LocalRecovery => "local_recovery".into(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRole {
    PrimaryOwner,
    Owner,
}
/// A server role on a Host: exactly the standalone built-in roles below Owner (Owner is the
/// Host's own role), with the same ids, ranks and capabilities, so a server acts the same
/// whether a Host manages it or not. `player` is accepted as the old name for Pack User.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerRole {
    PackUser,
    Viewer,
    Moderator,
    Maintainer,
    Admin,
}
impl ServerRole {
    pub const ALL: &[Self] = &[Self::Admin, Self::Maintainer, Self::Moderator, Self::Viewer, Self::PackUser];

    pub const fn builtin(self) -> BuiltinRole {
        match self {
            Self::Admin => BuiltinRole::Administrator,
            Self::Maintainer => BuiltinRole::Maintainer,
            Self::Moderator => BuiltinRole::Moderator,
            Self::Viewer => BuiltinRole::Viewer,
            Self::PackUser => BuiltinRole::PackUser,
        }
    }

    pub const fn id(self) -> &'static str {
        self.builtin().id()
    }

    pub const fn name(self) -> &'static str {
        self.builtin().name()
    }

    pub const fn rank(self) -> i32 {
        self.builtin().rank()
    }

    pub const fn capabilities(self) -> &'static [Capability] {
        self.builtin().capabilities()
    }

    pub fn permits(self, capability: Capability) -> bool {
        self.capabilities().contains(&capability)
    }

    pub fn summary(self) -> serde_json::Value {
        let role = crate::worker::ServerRole::for_effective(EffectiveRole::Server(self));
        serde_json::json!({
            "id": self.id(),
            "name": self.name(),
            "rank": self.rank(),
            "builtin": true,
            "capabilities": role.capabilities().map(|cap| serde_json::json!({
                "capability": cap.id(), "scope": "*"
            })).collect::<Vec<_>>()
        })
    }

    /// The stored spelling (Administrator was stored as `admin` before roles were unified).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            _ => self.id(),
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "pack-user" | "player" => Ok(Self::PackUser),
            "viewer" => Ok(Self::Viewer),
            "moderator" => Ok(Self::Moderator),
            "maintainer" => Ok(Self::Maintainer),
            "admin" | "administrator" => Ok(Self::Admin),
            _ => Err(reject(Code::RoleInvalid)),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectiveRole {
    PrimaryOwner,
    Owner,
    Server(ServerRole),
}

impl EffectiveRole {
    pub const fn rank(self) -> i32 {
        match self {
            Self::Owner | Self::PrimaryOwner => BuiltinRole::Owner.rank(),
            Self::Server(role) => role.rank(),
        }
    }

    pub fn permits(self, capability: Capability) -> bool {
        match self {
            Self::Owner | Self::PrimaryOwner => BuiltinRole::Owner.capabilities().contains(&capability),
            Self::Server(role) => role.permits(capability),
        }
    }

    pub fn can_invite(self, role: ServerRole) -> bool {
        self.permits(Capability::InvitationsCreate)
            && crate::identity::can_delegate_role(self.rank(), role.rank(), Some(role.builtin()))
            && role.capabilities().iter().all(|&cap| cap.delegable() && self.permits(cap))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub device_id: Uuid,
    pub account_id: Uuid,
    pub name: String,
    pub public_key: [u8; 32],
    pub created_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerEntry {
    pub account_id: Uuid,
    pub role: HostRole,
    /// Last Minecraft name verified by a fresh account proof on this host.
    pub name: Option<String>,
}

/// Primary owner, owner, or a grant on any server that is not deleted.
pub(crate) fn holds_any_role_in(c: &Connection, account: Uuid) -> Result<bool> {
    if host_role_in(c, account)?.is_some() {
        return Ok(true);
    }
    Ok(account_exists(c, account)?
        && c.query_row(
            "SELECT 1 FROM machine_server_grants g JOIN machine_servers s USING(server_id) WHERE g.account_id=?1 AND s.state!='deleted' LIMIT 1",
            [account.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}
pub(crate) fn record_account_name_in(
    c: &Connection,
    account: Uuid,
    name: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO machine_account_names(account_id,name,verified_at_ms) VALUES(?1,?2,?3) ON CONFLICT(account_id) DO UPDATE SET name=excluded.name,verified_at_ms=excluded.verified_at_ms",
        params![account.to_string(), name, now],
    )?;
    Ok(())
}
fn account_name_in(c: &Connection, account: Uuid) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT name FROM machine_account_names WHERE account_id=?1",
        [account.to_string()],
        |r| r.get(0),
    )
    .optional()?)
}

pub fn host_role_in(c: &Connection, account: Uuid) -> Result<Option<HostRole>> {
    if !account_exists(c, account)? {
        return Ok(None);
    }
    if snapshot_in(c)?.primary_account_id == Some(account) {
        return Ok(Some(HostRole::PrimaryOwner));
    }
    Ok(c.query_row(
        "SELECT 1 FROM machine_owners WHERE account_id=?1",
        [account.to_string()],
        |_| Ok(()),
    )
    .optional()?
    .map(|_| HostRole::Owner))
}
pub(crate) fn require_owner(c: &Connection, actor: Actor) -> Result<()> {
    match actor {
        Actor::LocalRecovery => Ok(()),
        Actor::Account(id) if host_role_in(c, id)?.is_some() => Ok(()),
        _ => Err(reject(Code::Forbidden)),
    }
}
fn require_primary(c: &Connection, actor: Actor) -> Result<()> {
    match actor {
        Actor::LocalRecovery => Ok(()),
        Actor::Account(id) if host_role_in(c, id)? == Some(HostRole::PrimaryOwner) => Ok(()),
        _ => Err(reject(Code::Forbidden)),
    }
}

impl HostState {
    pub fn host_role(&self, account: Uuid) -> Result<Option<HostRole>> {
        self.read(|c| host_role_in(c, account))
    }
    /// Trusted enrollment primitive. H04 must consume current code/proof in the
    /// SAME write callback using claim_in; this convenience is not a wire route.
    pub fn claim(&self, account: Uuid, name: &str, key: [u8; 32]) -> Result<Device> {
        self.write(|c| Self::claim_in(c, account, name, key, self.now_ms()))
    }
    pub fn claim_in(
        c: &Connection,
        account: Uuid,
        name: &str,
        key: [u8; 32],
        now: i64,
    ) -> Result<Device> {
        if snapshot_in(c)?.primary_account_id.is_some() {
            return Err(reject(Code::Conflict));
        }
        let device = enroll_device_in(c, account, name, key, now)?;
        c.execute(
            "UPDATE machine_host SET primary_account_id=?1,claimed=1 WHERE singleton=1",
            [account.to_string()],
        )?;
        changed(
            c,
            true,
            "claim",
            &account.to_string(),
            &device.device_id.to_string(),
            now,
        )?;
        Ok(device)
    }
    /// Called after fresh account proof, key possession and exact link approval.
    pub fn enroll_device_in(
        c: &Connection,
        account: Uuid,
        name: &str,
        key: [u8; 32],
        now: i64,
    ) -> Result<Device> {
        let device = enroll_device_in(c, account, name, key, now)?;
        changed(
            c,
            true,
            "device_link",
            &account.to_string(),
            &device.device_id.to_string(),
            now,
        )?;
        Ok(device)
    }
    pub fn set_owner(
        &self,
        actor: Actor,
        target: Uuid,
        owner: bool,
        revision: u64,
    ) -> Result<HostSnapshot> {
        self.write(|c| {
            require_primary(c, actor)?;
            expect_revision(c, revision)?;
            if snapshot_in(c)?.primary_account_id == Some(target) {
                return Err(reject(Code::Forbidden));
            }
            if !account_exists(c, target)? {
                return Err(reject(Code::NotFound));
            }
            if owner {
                c.execute(
                    "INSERT OR IGNORE INTO machine_owners(account_id) VALUES(?1)",
                    [target.to_string()],
                )?;
            } else {
                c.execute(
                    "DELETE FROM machine_owners WHERE account_id=?1",
                    [target.to_string()],
                )?;
            }
            changed(
                c,
                true,
                "owner_change",
                &actor.audit(),
                &target.to_string(),
                self.now_ms(),
            )
        })
    }
    /// Remote caller must first verify the primary device's fresh signed transfer.
    /// Old primary remains a promoted owner; target must already be an owner.
    pub fn transfer_ownership(
        &self,
        actor: Actor,
        target: Uuid,
        revision: u64,
    ) -> Result<HostSnapshot> {
        self.write(|c| {
            require_primary(c, actor)?;
            expect_revision(c, revision)?;
            if host_role_in(c, target)? != Some(HostRole::Owner) {
                return Err(reject(Code::Conflict));
            }
            let old = snapshot_in(c)?
                .primary_account_id
                .ok_or_else(|| reject(Code::Conflict))?;
            c.execute(
                "INSERT OR IGNORE INTO machine_owners(account_id) VALUES(?1)",
                [old.to_string()],
            )?;
            c.execute(
                "DELETE FROM machine_owners WHERE account_id=?1",
                [target.to_string()],
            )?;
            c.execute(
                "UPDATE machine_host SET primary_account_id=?1 WHERE singleton=1",
                [target.to_string()],
            )?;
            changed(
                c,
                true,
                "ownership_transfer",
                &actor.audit(),
                &target.to_string(),
                self.now_ms(),
            )
        })
    }
    /// Owner-only: the primary owner first, then owners by account id.
    pub fn owners(&self, actor: Actor) -> Result<Vec<OwnerEntry>> {
        self.read(|c| {
            require_owner(c, actor)?;
            let mut entries = Vec::new();
            if let Some(primary) = snapshot_in(c)?.primary_account_id {
                entries.push(OwnerEntry {
                    account_id: primary,
                    role: HostRole::PrimaryOwner,
                    name: account_name_in(c, primary)?,
                });
            }
            let mut statement =
                c.prepare("SELECT account_id FROM machine_owners ORDER BY account_id")?;
            let ids = statement
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for id in ids {
                let account = uuid(id)?;
                if entries.iter().any(|e| e.account_id == account) {
                    continue;
                }
                entries.push(OwnerEntry {
                    account_id: account,
                    role: HostRole::Owner,
                    name: account_name_in(c, account)?,
                });
            }
            Ok(entries)
        })
    }
    pub fn device(&self, id: Uuid) -> Result<Option<Device>> {
        self.read(|c| device_in(c, id))
    }
    pub fn active_device(&self, id: Uuid) -> Result<Option<Device>> {
        self.read(|c| active_device_in(c, id))
    }
    pub fn devices(&self, actor: Actor) -> Result<Vec<Device>> {
        self.read(|c| {
            require_owner(c, actor)?;
            let mut statement =
                c.prepare("SELECT device_id FROM machine_devices ORDER BY device_id")?;
            let ids = statement
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids.into_iter()
                .map(|id| device_in(c, uuid(id)?)?.ok_or_else(|| reject(Code::NotFound)))
                .collect()
        })
    }
    pub fn revoke_device(&self, actor: Actor, id: Uuid) -> Result<HostSnapshot> {
        self.write(|c| {
            require_owner(c, actor)?;
            let device = device_in(c, id)?.ok_or_else(|| reject(Code::NotFound))?;
            if snapshot_in(c)?.primary_account_id == Some(device.account_id) {
                require_primary(c, actor)?;
            }
            if device.revoked_at_ms.is_some() {
                return snapshot_in(c);
            }
            c.execute(
                "UPDATE machine_devices SET revoked_at_ms=?1 WHERE device_id=?2",
                params![self.now_ms(), id.to_string()],
            )?;
            changed(
                c,
                true,
                "device_revoke",
                &actor.audit(),
                &id.to_string(),
                self.now_ms(),
            )
        })
    }
    pub fn server_access(
        &self,
        actor: Actor,
        server: Uuid,
    ) -> Result<Vec<(Uuid, EffectiveRole, Option<String>)>> {
        self.read(|c| {
            super::registry::require_server(c, server)?;
            let allowed = match actor {
                Actor::LocalRecovery => true,
                Actor::Account(id) => {
                    host_role_in(c, id)?.is_some()
                        || super::registry::effective_role_in(c, id, server)?
                            == Some(EffectiveRole::Server(ServerRole::Admin))
                }
            };
            if !allowed {
                return Err(reject(Code::Forbidden));
            }
            let mut q = c.prepare("SELECT account_id FROM machine_accounts ORDER BY account_id")?;
            let ids = q
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut out = Vec::new();
            for id in ids {
                let id = uuid(id)?;
                if let Some(role) = super::registry::effective_role_in(c, id, server)? {
                    out.push((id, role, account_name_in(c, id)?));
                }
            }
            Ok(out)
        })
    }
    pub fn set_server_role(
        &self,
        actor: Actor,
        server: Uuid,
        target: Uuid,
        role: Option<ServerRole>,
        revision: u64,
    ) -> Result<HostSnapshot> {
        self.write(|c| {
            expect_revision(c,revision)?;
            super::registry::require_server(c,server)?;
            let authorized=match actor { Actor::LocalRecovery=>true,Actor::Account(id)=> host_role_in(c,id)?.is_some() || super::registry::effective_role_in(c,id,server)?==Some(EffectiveRole::Server(ServerRole::Admin)) };
            if !authorized { return Err(reject(Code::Forbidden)); }
            if !account_exists(c,target)? { return Err(reject(Code::NotFound)); }
            // Machine ownership cannot be changed through server access, even by owners.
            if host_role_in(c,target)?.is_some() { return Err(reject(Code::Forbidden)); }
            let owner = match actor { Actor::LocalRecovery => true, Actor::Account(id) => host_role_in(c,id)?.is_some() };
            if !owner && (role == Some(ServerRole::Admin) || super::registry::effective_role_in(c,target,server)? == Some(EffectiveRole::Server(ServerRole::Admin))) {
                return Err(reject(Code::Forbidden));
            }
            if let Some(role)=role { c.execute("INSERT INTO machine_server_grants(server_id,account_id,role) VALUES(?1,?2,?3) ON CONFLICT(server_id,account_id) DO UPDATE SET role=excluded.role",params![server.to_string(),target.to_string(),role.as_str()])?; }
            else { c.execute("DELETE FROM machine_server_grants WHERE server_id=?1 AND account_id=?2",params![server.to_string(),target.to_string()])?; }
            changed(c,true,"server_role_change",&actor.audit(),&format!("{server}/{target}"),self.now_ms())
        })
    }
}

pub(crate) fn enroll_device_in(
    c: &Connection,
    account: Uuid,
    name: &str,
    key: [u8; 32],
    now: i64,
) -> Result<Device> {
    if c.is_autocommit() {
        return Err(reject(Code::InvalidRequest));
    }
    if name.trim().is_empty() || name.len() > 128 || account.is_nil() {
        return Err(reject(Code::InvalidRequest));
    }
    if c.query_row(
        "SELECT 1 FROM machine_devices WHERE public_key=?1",
        [key.as_slice()],
        |_| Ok(()),
    )
    .optional()?
    .is_some()
    {
        return Err(reject(Code::CredentialConflict));
    }
    c.execute(
        "INSERT OR IGNORE INTO machine_accounts(account_id) VALUES(?1)",
        [account.to_string()],
    )?;
    if !account_exists(c, account)? {
        return Err(reject(Code::Forbidden));
    }
    let device = Device {
        device_id: Uuid::new_v4(),
        account_id: account,
        name: name.into(),
        public_key: key,
        created_at_ms: now,
        revoked_at_ms: None,
    };
    c.execute("INSERT INTO machine_devices(device_id,account_id,name,public_key,created_at_ms) VALUES(?1,?2,?3,?4,?5)",params![device.device_id.to_string(),account.to_string(),name,key.as_slice(),now])?;
    Ok(device)
}
/// Current device/account check on the caller's existing snapshot or write
/// transaction. Sessions retain ids only and re-resolve this at request time.
pub fn active_device_in(c: &Connection, id: Uuid) -> Result<Option<Device>> {
    let Some(device) = device_in(c, id)? else {
        return Ok(None);
    };
    if device.revoked_at_ms.is_some() || !account_exists(c, device.account_id)? {
        return Ok(None);
    }
    Ok(Some(device))
}
pub(crate) fn device_by_key_in(c: &Connection, key: &[u8; 32]) -> Result<Option<Device>> {
    let id: Option<String> = c
        .query_row(
            "SELECT device_id FROM machine_devices WHERE public_key=?1",
            [key.as_slice()],
            |r| r.get(0),
        )
        .optional()?;
    match id {
        Some(id) => device_in(c, uuid(id)?),
        None => Ok(None),
    }
}
pub fn device_in(c: &Connection, id: Uuid) -> Result<Option<Device>> {
    let row:Option<(String,String,Vec<u8>,i64,Option<i64>)>=c.query_row("SELECT account_id,name,public_key,created_at_ms,revoked_at_ms FROM machine_devices WHERE device_id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    row.map(|(account, name, key, created, revoked)| {
        Ok(Device {
            device_id: id,
            account_id: uuid(account)?,
            name,
            public_key: key.try_into().map_err(|_| reject(Code::InvalidRequest))?,
            created_at_ms: created,
            revoked_at_ms: revoked,
        })
    })
    .transpose()
}

impl HostState {
    /// Trusted host adapter data: aggregate counts only, never device identity.
    pub fn active_device_counts(&self) -> Result<std::collections::BTreeMap<String, u32>> {
        self.read(|c| {
            let mut stmt = c.prepare("SELECT d.account_id,count(*) FROM machine_devices d JOIN machine_accounts a ON a.account_id=d.account_id WHERE d.revoked_at_ms IS NULL AND a.disabled=0 GROUP BY d.account_id")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
}
