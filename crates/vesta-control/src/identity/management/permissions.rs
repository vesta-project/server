//! Transactional remote permission administration. No caller-supplied policy is trusted.
use super::*;
use crate::identity::{
    Capability, CustomRank, CustomRole, PolicySnapshot, ResourceScope, RoleGrant, RoleName,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[path = "branding.rs"]
pub mod branding;

const MAX_RECEIPTS: i64 = 4096;
/// Receipts answer retries for this long, then are purged in bounded batches.
const RECEIPT_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const RECEIPT_PURGE_BATCH: i64 = 128;
const MAX_ROLES: i64 = 128;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermissionError {
    SessionInvalid,
    Forbidden,
    InvalidRequest,
    NotFound,
    PreconditionFailed,
    IdempotencyConflict,
    Conflict,
    Saturated,
    Unavailable,
}
impl From<ManagementError> for PermissionError {
    fn from(e: ManagementError) -> Self {
        match e {
            ManagementError::SessionInvalid => Self::SessionInvalid,
            ManagementError::Forbidden => Self::Forbidden,
            ManagementError::InvalidRequest => Self::InvalidRequest,
            ManagementError::Saturated => Self::Saturated,
            _ => Self::Unavailable,
        }
    }
}
impl From<rusqlite::Error> for PermissionError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Unavailable
    }
}
impl From<crate::ControlError> for PermissionError {
    fn from(_: crate::ControlError) -> Self {
        Self::Unavailable
    }
}
type AdminResult<T> = std::result::Result<T, PermissionError>;
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityInput {
    pub capability: String,
    pub scope: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoleInput {
    pub name: String,
    pub rank: i32,
    pub capabilities: Vec<CapabilityInput>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RolePatch {
    pub expected_version: String,
    pub name: Option<String>,
    pub rank: Option<i32>,
    pub capabilities: Option<Vec<CapabilityInput>>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UserRoles {
    pub add: Vec<String>,
    pub remove: Vec<String>,
    pub expected_version: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccessPatch {
    pub expected_version: String,
    pub anonymous: Option<String>,
    pub new_user: Option<String>,
    pub whitelist: Option<String>,
    pub whitelist_enabled: Option<bool>,
    pub sync_whitelist: Option<bool>,
    pub password: Option<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    pub role: String,
    pub expires_minutes: u16,
    #[serde(default)]
    pub multi_use: bool,
    #[serde(default)]
    pub max_uses: Option<u32>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRename {
    pub name: String,
}
#[derive(Clone, Serialize)]
pub enum PermissionMutation {
    UserRoles(String, UserRoles),
    CreateRole(RoleInput),
    PatchRole(String, RolePatch),
    DeleteRole(String, String),
    Access(AccessPatch),
    Invitation(Invitation),
    RevokeDevice(String),
    RevokeInvitation(String),
    RenameDevice(String, DeviceRename),
    Server(branding::ServerPatch),
    PutIcon(Option<String>, Vec<u8>),
    DeleteIcon(String),
}
impl PermissionMutation {
    fn capability(&self) -> Option<Capability> {
        Some(match self {
            Self::UserRoles(_, _) => Capability::PrincipalsManage,
            Self::CreateRole(_) => Capability::RolesCreate,
            Self::PatchRole(_, _) | Self::DeleteRole(_, _) => Capability::RolesManage,
            Self::Access(_) => Capability::AccessSourcesManage,
            Self::Invitation(_) | Self::RevokeInvitation(_) => Capability::InvitationsCreate,
            Self::RenameDevice(_, _) => return None,
            Self::Server(_) | Self::PutIcon(..) | Self::DeleteIcon(_) => Capability::ConfigEdit,
            Self::RevokeDevice(_) => Capability::DevicesRevoke,
        })
    }
    fn route(&self) -> (&'static str, String) {
        match self {
            Self::UserRoles(id, _) => ("PUT", format!("users/{id}/roles")),
            Self::CreateRole(_) => ("POST", "roles".into()),
            Self::PatchRole(id, _) => ("PATCH", format!("roles/{id}")),
            Self::DeleteRole(id, _) => ("DELETE", format!("roles/{id}")),
            Self::Access(_) => ("PATCH", "access-sources".into()),
            Self::Invitation(_) => ("POST", "invitations".into()),
            Self::RevokeDevice(id) => ("DELETE", format!("devices/{id}")),
            Self::RevokeInvitation(id) => ("DELETE", format!("invitations/{id}")),
            Self::RenameDevice(id, _) => ("PATCH", format!("devices/{id}")),
            Self::Server(_) => ("PATCH", "server".into()),
            Self::PutIcon(..) => ("PUT", "server/icon".into()),
            Self::DeleteIcon(_) => ("DELETE", "server/icon".into()),
        }
    }
}
pub enum PermissionQuery {
    Users { offset: u32, limit: u16 },
    User(String),
    Roles,
    Access,
    Server,
    Devices(String),
    ManageableDevices { offset: u32, limit: u16 },
    Invitations { offset: u32, limit: u16 },
}
fn valid_page(offset: u32, limit: u16) -> AdminResult<()> {
    if offset > 1_000_000 || !(1..=100).contains(&limit) {
        return Err(PermissionError::InvalidRequest);
    }
    Ok(())
}
fn require(policy: &PolicySnapshot, capability: Capability) -> AdminResult<()> {
    if policy.check(capability, None).allowed() {
        Ok(())
    } else {
        Err(PermissionError::Forbidden)
    }
}
fn lower(policy: &PolicySnapshot, principal: &PrincipalId, rank: i32) -> AdminResult<()> {
    if policy.principal() == Some(principal) || rank >= policy.rank() {
        Err(PermissionError::Forbidden)
    } else {
        Ok(())
    }
}
fn role_id(text: &str) -> AdminResult<RoleId> {
    RoleId::parse(text).map_err(|_| PermissionError::InvalidRequest)
}
fn principal_id(text: &str) -> AdminResult<PrincipalId> {
    PrincipalId::parse(text).map_err(|_| PermissionError::InvalidRequest)
}
fn role(repo: &Repository<'_>, text: &str) -> AdminResult<crate::identity::authorization::Role> {
    repo.role(&role_id(text)?)
        .map_err(|_| PermissionError::NotFound)
}
fn delegated(policy: &PolicySnapshot, grants: &[RoleGrant]) -> AdminResult<()> {
    for grant in grants {
        if !grant.capability().delegable()
            || !policy.holds_entirely(grant.capability(), grant.scope())
        {
            return Err(PermissionError::Forbidden);
        }
    }
    Ok(())
}
fn lower_role(
    policy: &PolicySnapshot,
    r: &crate::identity::authorization::Role,
) -> AdminResult<()> {
    if r.rank >= policy.rank() || r.builtin.is_some_and(BuiltinRole::local_grant_only) {
        return Err(PermissionError::Forbidden);
    }
    Ok(())
}
fn custom(input: &RoleInput) -> AdminResult<CustomRole> {
    if input.capabilities.len() > 64 {
        return Err(PermissionError::InvalidRequest);
    }
    let grants = input
        .capabilities
        .iter()
        .map(|g| {
            let capability =
                Capability::from_id(&g.capability).ok_or(PermissionError::InvalidRequest)?;
            RoleGrant::new(
                capability,
                ResourceScope::parse(&g.scope).map_err(|_| PermissionError::InvalidRequest)?,
            )
            .map_err(|_| PermissionError::InvalidRequest)
        })
        .collect::<AdminResult<Vec<_>>>()?;
    CustomRole::new(
        RoleName::parse(&input.name).map_err(|_| PermissionError::InvalidRequest)?,
        CustomRank::new(input.rank).map_err(|_| PermissionError::InvalidRequest)?,
        grants,
    )
    .map_err(|_| PermissionError::InvalidRequest)
}
fn version(value: &Value) -> String {
    hex::encode(digest::digest(
        &digest::SHA256,
        &serde_json::to_vec(value).expect("JSON value"),
    ))
}
fn expected(text: &str, value: &Value) -> AdminResult<()> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(PermissionError::InvalidRequest);
    }
    if text != version(value) {
        return Err(PermissionError::PreconditionFailed);
    }
    Ok(())
}
fn with_version(mut value: Value) -> Value {
    let v = version(&value);
    value
        .as_object_mut()
        .expect("object")
        .insert("version".into(), json!(v));
    value
}
fn role_value(repo: &Repository<'_>, id: &str) -> AdminResult<Value> {
    let r = role(repo, id)?;
    let name: String =
        repo.connection()
            .query_row("SELECT name FROM roles WHERE id=?", [id], |r| r.get(0))?;
    Ok(
        json!({"id":id,"name":name,"rank":r.rank,"builtin":r.builtin.is_some(),
        "capabilities":r.grants.iter().map(|g|json!({"capability":g.capability().id(),"scope":g.scope().text()})).collect::<Vec<_>>()}),
    )
}
fn role_values(repo: &Repository<'_>) -> AdminResult<Vec<Value>> {
    let mut s = repo
        .connection()
        .prepare("SELECT id FROM roles ORDER BY rank DESC,id LIMIT 129")?;
    let ids = s
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if ids.len() > MAX_ROLES as usize {
        return Err(PermissionError::Saturated);
    }
    ids.iter()
        .map(|id| role_value(repo, id).map(with_version))
        .collect()
}
fn user_value(repo: &Repository<'_>, id: &str) -> AdminResult<Value> {
    let principal = principal_id(id)?;
    let name: Option<String> = repo
        .connection()
        .query_row(
            "SELECT coalesce(n.name,p.display_name) FROM principals p LEFT JOIN user_minecraft_names n ON n.principal_id=p.id WHERE p.id=? AND p.server_id=? AND p.status='active'",
            params![id, repo_server(repo)?],
            |r| r.get(0),
        )
        .optional()?;
    let name = name.ok_or(PermissionError::NotFound)?;
    let mut s = repo.connection().prepare("SELECT role_id,expires_at FROM role_assignments WHERE principal_id=? AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>?) ORDER BY role_id,id LIMIT 129")?;
    let assignments = s
        .query_map(params![id, repo.now], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if assignments.len() > 128 {
        return Err(PermissionError::Saturated);
    }
    let mut roles = Vec::new();
    for (id, expiry) in assignments {
        roles.push(json!({"role":role_value(repo,&id)?,"expires_at_ms":expiry}));
    }
    let default: Option<String> = repo
        .connection()
        .query_row(
            "SELECT role_id FROM user_default_roles WHERE principal_id=?",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let default = default.map(|id| role_value(repo, &id)).transpose()?;
    let mut s = repo.connection().prepare("SELECT capability,scope,effect,expires_at FROM principal_overrides WHERE principal_id=? AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>?) ORDER BY id LIMIT 129")?;
    let overrides=s.query_map(params![id,repo.now],|r|Ok(json!({"capability":r.get::<_,String>(0)?,"scope":r.get::<_,String>(1)?,"effect":r.get::<_,String>(2)?,"expires_at_ms":r.get::<_,Option<i64>>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    if overrides.len() > 128 {
        return Err(PermissionError::Saturated);
    }
    let count: i64 = repo.connection().query_row(
        "SELECT count(*) FROM devices WHERE principal_id=? AND revoked_at IS NULL",
        [id],
        |r| r.get(0),
    )?;
    let uuid: Option<String> = repo
        .connection()
        .query_row(
            "SELECT minecraft_uuid FROM user_minecraft_accounts WHERE principal_id=?",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let minecraft_name: Option<String> = repo
        .connection()
        .query_row(
            "SELECT name FROM user_minecraft_names WHERE principal_id=?",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let minecraft = uuid
        .map(|uuid| json!({"uuid":uuid,"name":minecraft_name}))
        .into_iter()
        .collect::<Vec<_>>();
    Ok(
        json!({"id":id,"display_name":name,"minecraft_accounts":minecraft,"roles":roles,
        "default_role":default,"overrides":overrides,"rank":repo.rank(&principal)?,"device_count":count}),
    )
}
fn repo_server(repo: &Repository<'_>) -> AdminResult<String> {
    // Server identity is immutable and the database is server-local.
    Ok(repo.connection().query_row(
        "SELECT server_id FROM server_identity WHERE singleton=1",
        [],
        |r| r.get(0),
    )?)
}
fn active_roles(repo: &Repository<'_>, principal: &PrincipalId) -> AdminResult<Vec<String>> {
    let mut s=repo.connection().prepare("SELECT DISTINCT role_id FROM role_assignments WHERE principal_id=? AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>?) ORDER BY role_id LIMIT 129")?;
    let ids = s
        .query_map(params![principal.as_str(), repo.now], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if ids.len() > 128 {
        return Err(PermissionError::Saturated);
    }
    Ok(ids)
}
/// Check the complete resulting principal, not only the newly delegated role.
/// Rank increases amplify existing rank-sensitive permissions such as devices.revoke.
fn delegated_user(
    repo: &Repository<'_>,
    policy: &PolicySnapshot,
    principal: &PrincipalId,
    roles: &[String],
    replacement: Option<&crate::identity::authorization::Role>,
) -> AdminResult<()> {
    for id in roles {
        let current;
        let r = if let Some(r) = replacement.filter(|r| r.id.as_str() == id) {
            r
        } else {
            current = role(repo, id)?;
            &current
        };
        if r.rank >= policy.rank() {
            return Err(PermissionError::Forbidden);
        }
        delegated(policy, &r.grants)?;
    }
    let mut statement = repo.connection().prepare(
        "SELECT capability,scope FROM principal_overrides WHERE principal_id=? AND effect='allow' AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>?)",
    )?;
    let grants = statement
        .query_map(params![principal.as_str(), repo.now], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (capability, scope) in grants {
        let capability = Capability::from_id(&capability).ok_or(PermissionError::Unavailable)?;
        let grant = RoleGrant::new(
            capability,
            ResourceScope::parse(&scope).map_err(|_| PermissionError::Unavailable)?,
        )?;
        delegated(policy, &[grant])?;
    }
    Ok(())
}
pub(super) fn insert_assignment(
    repo: &Repository<'_>,
    principal: &PrincipalId,
    role: &RoleId,
    by: &str,
) -> AdminResult<()> {
    let secret = random_secret()?;
    let id = hex::encode(digest::digest(&digest::SHA256, secret.expose().as_bytes()));
    repo.connection().execute("INSERT INTO role_assignments(id,principal_id,role_id,granted_by,created_at) VALUES(?,?,?,?,?)",params![id,principal.as_str(),role.as_str(),by,repo.now])?;
    Ok(())
}
fn safe_sources(repo: &Repository<'_>, settings: &AccessSettings) -> AdminResult<()> {
    for id in [
        &settings.anonymous,
        &settings.new_user,
        &settings.whitelist,
        &settings.password,
    ] {
        access::role_allows(repo, id, Capability::PublicInfo)?;
    }
    Ok(())
}
fn session_current(now: i64, expires: i64, monotonic: std::time::Instant) -> AdminResult<()> {
    if now >= expires || std::time::Instant::now() >= monotonic {
        return Err(PermissionError::SessionInvalid);
    }
    Ok(())
}
impl ManagementAccess {
    /// First-server setup uses current account-bound Owner authority in the same
    /// transaction as its effect. Captured bearer data never substitutes for a
    /// fresh device/account/role check at this boundary.
    pub(crate) fn owner_transaction<T>(
        &self,
        token: &str,
        work: impl FnOnce(&rusqlite::Connection, &Subject, i64) -> AdminResult<T>,
    ) -> AdminResult<T> {
        let subject = self.authenticate(token)?;
        let stores = self
            .stores
            .lock()
            .map_err(|_| PermissionError::Unavailable)?;
        let session = stores
            .sessions
            .get(&bound_hash(b"session", self.server_id(), token))
            .ok_or(PermissionError::SessionInvalid)?;
        let result = self.admin_write(|repo| {
            session_current(repo.now, session.expires, session.monotonic_expires)?;
            let (current, _) = self.subject_in(repo, &subject.device)?;
            if current != subject
                || !repo
                    .policy(Some(&current))?
                    .holds_builtin(BuiltinRole::Owner)
            {
                return Err(PermissionError::Forbidden);
            }
            work(repo.connection(), &current, repo.now)
        });
        // Same store -> DB ordering as the canonical session projection. Session
        // revocation cannot finish between validation and this SQL/audit effect.
        drop(stores);
        result
    }

    fn permission_session(&self, token: &str) -> AdminResult<(Subject, i64, std::time::Instant)> {
        let subject = self.authenticate(token)?;
        let stores = self
            .stores
            .lock()
            .map_err(|_| PermissionError::Unavailable)?;
        let key = bound_hash(b"session", self.server_id(), token);
        let session = stores
            .sessions
            .get(&key)
            .ok_or(PermissionError::SessionInvalid)?;
        Ok((subject, session.expires, session.monotonic_expires))
    }
    pub fn permission_query(&self, token: &str, query: PermissionQuery) -> AdminResult<Value> {
        let (subject, expires, monotonic) = self.permission_session(token)?;
        self.admin_write(|repo| {
            session_current(repo.now, expires, monotonic)?;
            let policy=repo.policy(Some(&subject))?;
            let capability=match &query { PermissionQuery::Access=>Capability::AccessSourcesManage,
                PermissionQuery::Devices(_) | PermissionQuery::ManageableDevices{..}=>Capability::DevicesRevoke,
                PermissionQuery::Invitations{..}=>Capability::InvitationsCreate,
                PermissionQuery::Server=>Capability::StatusView,_=>Capability::PrincipalsView };
            require(&policy,capability)?;
            match query {
                PermissionQuery::Users{offset,limit}=> {
                    if limit==0 || limit>100 || offset>1000000 { return Err(PermissionError::InvalidRequest); }
                    let mut s=repo.connection().prepare("SELECT id FROM principals WHERE server_id=? AND status='active' ORDER BY id LIMIT ? OFFSET ?")?;
                    let ids=s.query_map(params![self.server_id(),i64::from(limit)+1,offset],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    let more=ids.len()>usize::from(limit);
                    let users=ids.iter().take(usize::from(limit)).map(|id|user_value(repo,id).map(with_version)).collect::<AdminResult<Vec<_>>>()?;
                    Ok(json!({"users":users,"next_offset":more.then_some(offset+u32::from(limit))}))
                }
                PermissionQuery::Invitations{offset,limit} => {
                    valid_page(offset, limit)?;
                    let mut statement=repo.connection().prepare("SELECT i.id,c.role_id,r.name,r.rank,c.created_at,c.expires_at,coalesce(n.name,p.display_name) FROM management_invitation_ids i JOIN management_pairing_codes c ON c.code_hash=i.code_hash JOIN management_invitation_issuers a ON a.code_hash=c.code_hash JOIN roles r ON r.id=c.role_id JOIN principals p ON p.id=a.principal_id LEFT JOIN user_minecraft_names n ON n.principal_id=p.id WHERE c.server_id=? AND c.expires_at>? AND (a.principal_id=? OR r.rank<?) ORDER BY c.created_at DESC,i.id LIMIT ? OFFSET ?")?;
                    let mut invitations=statement.query_map(params![self.server_id(),repo.now,subject.principal.as_str(),policy.rank(),i64::from(limit)+1,offset],|r|Ok(json!({"id":r.get::<_,String>(0)?,"role":{"id":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"rank":r.get::<_,i32>(3)?},"created_at_ms":r.get::<_,i64>(4)?,"expires_at_ms":r.get::<_,i64>(5)?,"created_by":{"display_name":r.get::<_,String>(6)?}})))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    let more=invitations.len()>usize::from(limit); invitations.truncate(usize::from(limit));
                    Ok(json!({"invitations":invitations,"next_offset":more.then_some(offset+u32::from(limit))}))
                }
                PermissionQuery::ManageableDevices{offset,limit} => {
                    valid_page(offset, limit)?;
                    let mut statement=repo.connection().prepare("SELECT d.id,d.principal_id,coalesce(n.name,p.display_name),d.label,d.created_at,d.revoked_at IS NOT NULL FROM devices d JOIN principals p ON p.id=d.principal_id LEFT JOIN user_minecraft_names n ON n.principal_id=p.id WHERE d.server_id=? AND p.status='active' AND (d.principal_id=? OR coalesce((SELECT max(r.rank) FROM role_assignments a JOIN roles r ON r.id=a.role_id WHERE a.principal_id=d.principal_id AND a.revoked_at IS NULL AND (a.expires_at IS NULL OR a.expires_at>?)),0)<?) ORDER BY d.id LIMIT ? OFFSET ?")?;
                    let mut devices=statement.query_map(params![self.server_id(),subject.principal.as_str(),repo.now,policy.rank(),i64::from(limit)+1,offset],|r| {
                        let id:String=r.get(0)?;
                        Ok(json!({"id":id,"user":{"id":r.get::<_,String>(1)?,"display_name":r.get::<_,String>(2)?},"name":r.get::<_,String>(3)?,"created_at_ms":r.get::<_,i64>(4)?,"last_seen_at_ms":null,"revoked":r.get::<_,bool>(5)?,"current":id==subject.device.as_str()}))
                    })?.collect::<rusqlite::Result<Vec<_>>>()?;
                    let more=devices.len()>usize::from(limit); devices.truncate(usize::from(limit));
                    Ok(json!({"devices":devices,"next_offset":more.then_some(offset+u32::from(limit))}))
                }
                PermissionQuery::Server=>branding::projection(repo),
                PermissionQuery::User(id)=>user_value(repo,&id).map(with_version),
                PermissionQuery::Roles=>Ok(json!({"roles":role_values(repo)?})),
                PermissionQuery::Access=>Ok(with_version(json!({"settings":access::source_settings(repo)?}))),
                PermissionQuery::Devices(id)=> {
                    let principal=principal_id(&id)?;
                    if policy.principal()!=Some(&principal) { lower(&policy,&principal,repo.rank(&principal)?)?; }
                    user_value(repo,&id)?;
                    let mut s=repo.connection().prepare("SELECT id,label,revoked_at IS NOT NULL FROM devices WHERE principal_id=? ORDER BY id LIMIT 1001")?;
                    let devices=s.query_map([id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"revoked":r.get::<_,bool>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    if devices.len()>1000 { return Err(PermissionError::Saturated); }
                    Ok(json!({"devices":devices}))
                }
            }
        })
    }
    pub fn branding_icon(&self, token: &str) -> AdminResult<Option<Vec<u8>>> {
        let (subject, expires, monotonic) = self.permission_session(token)?;
        self.admin_write(|repo| {
            session_current(repo.now, expires, monotonic)?;
            require(&repo.policy(Some(&subject))?, Capability::StatusView)?;
            branding::icon(repo)
        })
    }
    fn admin_write<T>(
        &self,
        work: impl FnOnce(&Repository<'_>) -> AdminResult<T>,
    ) -> AdminResult<T> {
        match self.db.write_with(|tx| {
            work(&Repository::new(
                tx.connection(),
                self.server_id(),
                self.now(),
            ))
        }) {
            Ok(v) => Ok(v),
            Err(TransactionError::Work(e)) => Err(e),
            Err(_) => Err(PermissionError::Unavailable),
        }
    }
    pub fn permission_mutate(
        &self,
        token: &str,
        key: &str,
        command: PermissionMutation,
    ) -> AdminResult<Value> {
        if !(16..=128).contains(&key.len()) || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(PermissionError::InvalidRequest);
        }
        let (subject, expires, monotonic) = self.permission_session(token)?;
        let (method, path) = command.route();
        let scope = serde_json::to_vec(&(
            self.server_id(),
            subject.principal.as_str(),
            subject.device.as_str(),
            method,
            &path,
            key,
        ))
        .map_err(|_| PermissionError::InvalidRequest)?;
        let key_hash = digest::digest(&digest::SHA256, &scope);
        let intent = serde_json::to_vec(&command).map_err(|_| PermissionError::InvalidRequest)?;
        if intent.len()
            > if matches!(command, PermissionMutation::PutIcon(..)) {
                1_100_000
            } else {
                16384
            }
        {
            return Err(PermissionError::InvalidRequest);
        }
        let intent_hash = digest::digest(&digest::SHA256, &intent);
        self.admin_write(|repo| {
            session_current(repo.now, expires, monotonic)?;
            let policy=repo.policy(Some(&subject))?;
            if policy.principal()!=Some(&subject.principal) {return Err(PermissionError::Forbidden);}
            if let Some(capability)=command.capability() {require(&policy,capability)?;}
            let previous:Option<(Vec<u8>,String)>=repo.connection().query_row("SELECT intent_hash,result_json FROM management_permission_receipts WHERE key_hash=?",[key_hash.as_ref()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((hash,result))=previous {
                if hash!=intent_hash.as_ref() {return Err(PermissionError::IdempotencyConflict);}
                return serde_json::from_str(&result).map_err(|_|PermissionError::Unavailable);
            }
            repo.connection().execute("DELETE FROM management_permission_receipts WHERE key_hash IN (SELECT key_hash FROM management_permission_receipts WHERE completed_at<? ORDER BY completed_at LIMIT ?)",params![repo.now.saturating_sub(RECEIPT_RETENTION_MS),RECEIPT_PURGE_BATCH])?;
            let count:i64=repo.connection().query_row("SELECT count(*) FROM management_permission_receipts",[],|r|r.get(0))?;
            if count>=MAX_RECEIPTS {return Err(PermissionError::Saturated);}
            let result=self.mutate_in(repo,&subject,&policy,&command)?;
            self.audit(repo,device_actor(&subject)?,"management.permissions.mutate",&path)?;
            // Invitation secrets are never persisted; retries return the receipt without the code.
            let mut receipt=result.clone();
            if matches!(command,PermissionMutation::Invitation(_)) {receipt.as_object_mut().expect("object").remove("code");}
            let result_json=serde_json::to_string(&receipt).map_err(|_|PermissionError::Unavailable)?;
            repo.connection().execute("INSERT INTO management_permission_receipts(key_hash,intent_hash,result_json,completed_at) VALUES(?,?,?,?)",params![key_hash.as_ref(),intent_hash.as_ref(),result_json,repo.now])?;
            Ok(result)
        })
    }
    fn mutate_in(
        &self,
        repo: &Repository<'_>,
        subject: &Subject,
        policy: &PolicySnapshot,
        command: &PermissionMutation,
    ) -> AdminResult<Value> {
        let by = subject.principal.as_str();
        match command {
            PermissionMutation::Server(input) => branding::patch(repo, input),
            PermissionMutation::PutIcon(expected, bytes) => {
                branding::put_icon(repo, expected.as_deref(), bytes)
            }
            PermissionMutation::DeleteIcon(expected) => branding::delete_icon(repo, expected),
            PermissionMutation::UserRoles(id, input) => {
                if input.add.len() + input.remove.len() > 64
                    || input.add.is_empty() && input.remove.is_empty()
                {
                    return Err(PermissionError::InvalidRequest);
                }
                let principal = principal_id(id)?;
                lower(policy, &principal, repo.rank(&principal)?)?;
                let owner_grant = policy.roles().iter().any(|r| r.as_str() == "owner")
                    && repo.connection().query_row(
                        "SELECT EXISTS(SELECT 1 FROM user_minecraft_accounts WHERE principal_id=?)",
                        [principal.as_str()],
                        |r| r.get::<_, bool>(0),
                    )?;
                let mut seen = std::collections::HashSet::new();
                for id in input.add.iter().chain(&input.remove) {
                    if !seen.insert(id) {
                        return Err(PermissionError::InvalidRequest);
                    }
                    let r = role(repo, id)?;
                    if input.add.contains(id) {
                        if !(owner_grant && matches!(id.as_str(), "owner" | "administrator")) {
                            lower_role(policy, &r)?;
                            delegated(policy, &r.grants)?;
                        }
                    } else if r.rank >= policy.rank() || r.builtin == Some(BuiltinRole::Owner) {
                        // Removal follows rank; only the top role is console-only to remove.
                        return Err(PermissionError::Forbidden);
                    }
                }
                if !input.add.is_empty() {
                    require(policy, Capability::RolesAssign)?;
                }
                if !input.remove.is_empty() {
                    require(policy, Capability::RolesRevoke)?;
                }
                expected(&input.expected_version, &user_value(repo, id)?)?;
                let mut resulting = active_roles(repo, &principal)?;
                resulting.retain(|id| !input.remove.contains(id));
                for id in &input.add {
                    if !resulting.contains(id) {
                        resulting.push(id.clone());
                    }
                }
                if resulting.len() > 64 {
                    return Err(PermissionError::InvalidRequest);
                }
                if !owner_grant
                    || !input
                        .add
                        .iter()
                        .any(|r| matches!(r.as_str(), "owner" | "administrator"))
                {
                    delegated_user(repo, policy, &principal, &resulting, None)?;
                }
                for id in &input.remove {
                    repo.connection().execute("UPDATE role_assignments SET revoked_at=?,revoked_by=? WHERE principal_id=? AND role_id=? AND revoked_at IS NULL",params![repo.now,by,principal.as_str(),id])?;
                }
                for id in &input.add {
                    if !active_roles(repo, &principal)?.contains(id) {
                        insert_assignment(repo, &principal, &role_id(id)?, by)?;
                    }
                }
                user_value(repo, principal.as_str()).map(with_version)
            }
            PermissionMutation::CreateRole(input) => {
                if input.rank >= policy.rank() {
                    return Err(PermissionError::Forbidden);
                }
                let custom = custom(input)?;
                delegated(policy, custom.grants())?;
                let count: i64 =
                    repo.connection()
                        .query_row("SELECT count(*) FROM roles", [], |r| r.get(0))?;
                if count >= MAX_ROLES {
                    return Err(PermissionError::Saturated);
                }
                self.check_name(repo, &input.name, None)?;
                let id = repo.create_role(&custom, by)?;
                role_value(repo, id.as_str()).map(with_version)
            }
            PermissionMutation::PatchRole(id, input) => {
                let old = role(repo, id)?;
                if old.builtin.is_some() || old.rank >= policy.rank() {
                    return Err(PermissionError::Forbidden);
                }
                expected(&input.expected_version, &role_value(repo, id)?)?;
                if input.name.is_none() && input.rank.is_none() && input.capabilities.is_none() {
                    return Err(PermissionError::InvalidRequest);
                }
                let dependents = self.role_dependents(repo, policy, id)?;
                let current = role_value(repo, id)?;
                let capabilities = input.capabilities.clone().unwrap_or_else(|| {
                    old.grants
                        .iter()
                        .map(|g| CapabilityInput {
                            capability: g.capability().id().into(),
                            scope: g.scope().text(),
                        })
                        .collect()
                });
                let input = RoleInput {
                    name: input
                        .name
                        .clone()
                        .unwrap_or_else(|| current["name"].as_str().expect("name").into()),
                    rank: input.rank.unwrap_or(old.rank),
                    capabilities,
                };
                if input.rank >= policy.rank() {
                    return Err(PermissionError::Forbidden);
                }
                let custom = custom(&input)?;
                delegated(policy, custom.grants())?;
                self.check_name(repo, &input.name, Some(id))?;
                self.protect_automatic_role(repo, id, custom.grants())?;
                if input.rank > old.rank {
                    let replacement = crate::identity::authorization::Role {
                        id: old.id.clone(),
                        rank: input.rank,
                        builtin: None,
                        grants: custom.grants().to_vec(),
                    };
                    for principal in dependents {
                        delegated_user(
                            repo,
                            policy,
                            &principal,
                            &active_roles(repo, &principal)?,
                            Some(&replacement),
                        )?;
                    }
                }
                repo.connection().execute(
                    "UPDATE roles SET name=?,rank=? WHERE id=?",
                    params![input.name, input.rank, id],
                )?;
                repo.connection()
                    .execute("DELETE FROM role_capabilities WHERE role_id=?", [id])?;
                for g in custom.grants() {
                    repo.connection().execute(
                        "INSERT INTO role_capabilities VALUES(?,?,?)",
                        params![id, g.capability().id(), g.scope().text()],
                    )?;
                }
                role_value(repo, id).map(with_version)
            }
            PermissionMutation::DeleteRole(id, v) => {
                let old = role(repo, id)?;
                if old.builtin.is_some() || old.rank >= policy.rank() {
                    return Err(PermissionError::Forbidden);
                }
                expected(v, &role_value(repo, id)?)?;
                self.role_dependents(repo, policy, id)?;
                let settings = access::source_settings(repo)?;
                if [
                    &settings.anonymous,
                    &settings.new_user,
                    &settings.whitelist,
                    &settings.password,
                ]
                .contains(&id)
                {
                    return Err(PermissionError::Conflict);
                }
                // Historical assignments and source references are retained; deleting an in-use role is refused.
                let used:i64=repo.connection().query_row("SELECT (SELECT count(*) FROM role_assignments WHERE role_id=?1)+(SELECT count(*) FROM user_default_roles WHERE role_id=?1)+(SELECT count(*) FROM device_password_grants WHERE role_id=?1)+(SELECT count(*) FROM management_owner_links WHERE role_id=?1)+(SELECT count(*) FROM management_pairing_codes WHERE role_id=?1)",[id],|r|r.get(0))?;
                if used != 0 {
                    return Err(PermissionError::Conflict);
                }
                repo.connection()
                    .execute("DELETE FROM role_capabilities WHERE role_id=?", [id])?;
                repo.connection()
                    .execute("DELETE FROM roles WHERE id=?", [id])?;
                Ok(json!({"deleted":true}))
            }
            PermissionMutation::Access(input) => {
                let mut settings = access::source_settings(repo)?;
                expected(&input.expected_version, &json!({"settings":settings}))?;
                if input.anonymous.is_none()
                    && input.new_user.is_none()
                    && input.whitelist.is_none()
                    && input.whitelist_enabled.is_none()
                    && input.sync_whitelist.is_none()
                    && input.password.is_none()
                {
                    return Err(PermissionError::InvalidRequest);
                }
                if let Some(v) = &input.anonymous {
                    settings.anonymous = v.clone();
                }
                if let Some(v) = &input.new_user {
                    settings.new_user = v.clone();
                }
                if let Some(v) = &input.whitelist {
                    settings.whitelist = v.clone();
                }
                if let Some(v) = input.sync_whitelist {
                    require(policy, Capability::PlayersModerate)?;
                    settings.sync_whitelist = v;
                    repo.connection().execute("INSERT INTO whitelist_sync_actor VALUES(1,?,?) ON CONFLICT(singleton) DO UPDATE SET principal_id=excluded.principal_id,device_id=excluded.device_id",params![subject.principal.as_str(),subject.device.as_str()])?;
                }
                if let Some(v) = input.whitelist_enabled {
                    settings.whitelist_enabled = v;
                }
                if let Some(v) = &input.password {
                    settings.password = v.clone();
                }
                safe_sources(repo, &settings)?;
                for id in [
                    &settings.anonymous,
                    &settings.new_user,
                    &settings.whitelist,
                    &settings.password,
                ] {
                    delegated(policy, &role(repo, id)?.grants)?;
                }
                let json =
                    serde_json::to_string(&settings).map_err(|_| PermissionError::Unavailable)?;
                if json.len() > 4096 {
                    return Err(PermissionError::InvalidRequest);
                }
                repo.connection().execute(
                    "UPDATE access_role_sources SET settings_json=? WHERE singleton=1",
                    [json],
                )?;
                Ok(with_version(json!({"settings":settings})))
            }
            PermissionMutation::Invitation(input) => {
                if !(1..=MAX_PAIR_MINUTES).contains(&input.expires_minutes)
                    || input.max_uses == Some(0)
                    || (!input.multi_use && input.max_uses.is_some())
                {
                    return Err(PermissionError::InvalidRequest);
                }
                let r = role(repo, &input.role)?;
                lower_role(policy, &r)?;
                delegated(policy, &r.grants)?;
                repo.connection().execute(
                    "DELETE FROM management_pairing_codes WHERE expires_at<=?",
                    [repo.now],
                )?;
                let count: i64 = repo.connection().query_row(
                    "SELECT count(*) FROM management_pairing_codes",
                    [],
                    |r| r.get(0),
                )?;
                if count >= MAX_CODES as i64 {
                    return Err(PermissionError::Saturated);
                }
                let code = random_secret()?;
                let hash = bound_hash(b"pair", self.server_id(), code.expose());
                let expires = deadline(repo.now, i64::from(input.expires_minutes) * 60000)?;
                repo.connection().execute(
                    "INSERT INTO management_pairing_codes VALUES(?,?,?,?,?)",
                    params![
                        hash.as_slice(),
                        self.server_id(),
                        input.role,
                        repo.now,
                        expires
                    ],
                )?;
                if input.multi_use {
                    repo.connection().execute(
                        "INSERT INTO management_invitation_uses(code_hash,max_uses) VALUES(?,?)",
                        params![hash.as_slice(), input.max_uses],
                    )?;
                }
                let id = random_secret()?;
                repo.connection().execute(
                    "INSERT INTO management_invitation_ids(id,code_hash) VALUES(?,?)",
                    params![id.expose(), hash.as_slice()],
                )?;
                repo.connection().execute(
                    "INSERT INTO management_invitation_issuers VALUES(?,?,?)",
                    params![
                        hash.as_slice(),
                        subject.principal.as_str(),
                        subject.device.as_str()
                    ],
                )?;
                Ok(
                    json!({"issued":true,"id":id.expose(),"code":code.expose(),"role":input.role,"expires_at_ms":expires}),
                )
            }
            PermissionMutation::RevokeInvitation(id) => {
                let found:Option<(Vec<u8>,String,i32)>=repo.connection().query_row("SELECT c.code_hash,a.principal_id,r.rank FROM management_invitation_ids i JOIN management_pairing_codes c ON c.code_hash=i.code_hash JOIN management_invitation_issuers a ON a.code_hash=c.code_hash JOIN roles r ON r.id=c.role_id WHERE i.id=? AND c.server_id=? AND c.expires_at>?",params![id,self.server_id(),repo.now],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
                let (hash, creator, rank) = found.ok_or(PermissionError::NotFound)?;
                if creator != by && rank >= policy.rank() {
                    return Err(PermissionError::Forbidden);
                }
                repo.connection().execute(
                    "DELETE FROM management_pairing_codes WHERE code_hash=?",
                    [hash],
                )?;
                Ok(json!({"revoked":true}))
            }
            PermissionMutation::RenameDevice(id, input) => {
                DeviceId::parse(id).map_err(|_| PermissionError::InvalidRequest)?;
                let principal: Option<String> = repo
                    .connection()
                    .query_row(
                        "SELECT principal_id FROM devices WHERE id=? AND server_id=?",
                        params![id, self.server_id()],
                        |r| r.get(0),
                    )
                    .optional()?;
                let principal = principal_id(&principal.ok_or(PermissionError::NotFound)?)?;
                if principal != subject.principal {
                    return Err(PermissionError::Forbidden);
                }
                let name =
                    DeviceLabel::parse(&input.name).map_err(|_| PermissionError::InvalidRequest)?;
                repo.connection().execute(
                    "UPDATE devices SET label=? WHERE id=?",
                    params![name.as_str(), id],
                )?;
                Ok(json!({"id":id,"name":name.as_str()}))
            }
            PermissionMutation::RevokeDevice(id) => {
                let device = DeviceId::parse(id).map_err(|_| PermissionError::InvalidRequest)?;
                let principal = repo
                    .device_principal(&device)
                    .map_err(|_| PermissionError::NotFound)?;
                if policy.principal() != Some(&principal) {
                    lower(policy, &principal, repo.rank(&principal)?)?;
                }
                repo.revoke_device(&device, by)?;
                Ok(json!({"revoked":true}))
            }
        }
    }
    fn check_name(
        &self,
        repo: &Repository<'_>,
        name: &str,
        except: Option<&String>,
    ) -> AdminResult<()> {
        let found: Option<String> = repo
            .connection()
            .query_row(
                "SELECT id FROM roles WHERE name=? COLLATE NOCASE",
                [name],
                |r| r.get(0),
            )
            .optional()?;
        if found.as_ref().is_some_and(|id| Some(id) != except) {
            return Err(PermissionError::Conflict);
        }
        Ok(())
    }
    fn role_dependents(
        &self,
        repo: &Repository<'_>,
        policy: &PolicySnapshot,
        id: &str,
    ) -> AdminResult<Vec<PrincipalId>> {
        let mut s=repo.connection().prepare("SELECT DISTINCT principal_id FROM role_assignments WHERE role_id=? AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>?) UNION SELECT principal_id FROM user_default_roles WHERE role_id=? UNION SELECT d.principal_id FROM device_password_grants g JOIN devices d ON d.id=g.device_id WHERE g.role_id=?")?;
        let users = s
            .query_map(params![id, repo.now, id, id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let users = users
            .iter()
            .map(|user| principal_id(user))
            .collect::<AdminResult<Vec<_>>>()?;
        for user in &users {
            lower(policy, user, repo.rank(user)?)?;
        }
        Ok(users)
    }
    fn protect_automatic_role(
        &self,
        repo: &Repository<'_>,
        id: &str,
        grants: &[RoleGrant],
    ) -> AdminResult<()> {
        let settings = access::source_settings(repo)?;
        let referenced:i64=repo.connection().query_row("SELECT (SELECT count(*) FROM user_default_roles WHERE role_id=?1)+(SELECT count(*) FROM device_password_grants WHERE role_id=?1)",[id],|r|r.get(0))?;
        if (referenced > 0
            || [
                &settings.anonymous,
                &settings.new_user,
                &settings.whitelist,
                &settings.password,
            ]
            .iter()
            .any(|v| v.as_str() == id))
            && grants
                .iter()
                .any(|g| !access::safe(g.capability()) || !g.scope().is_all())
        {
            return Err(PermissionError::InvalidRequest);
        }
        Ok(())
    }
    /// Protected native console only. Unlike role replacement, this preserves other grants.
    pub fn superadmin_local(&self, user: &str, grant: bool) -> AdminResult<()> {
        let user = principal_id(user)?;
        self.admin_write(|repo| {
            repo.active_principal(&user)?;
            let holds=active_roles(repo,&user)?.iter().any(|id|id==BuiltinRole::Owner.id());
            if grant && !holds {insert_assignment(repo,&user,&RoleId::from(BuiltinRole::Owner),"local-os-owner")?;}
            if !grant && holds {
                let count:i64=repo.connection().query_row("SELECT count(DISTINCT p.id) FROM principals p JOIN role_assignments a ON a.principal_id=p.id WHERE p.status='active' AND a.role_id='owner' AND a.revoked_at IS NULL AND (a.expires_at IS NULL OR a.expires_at>?)",[repo.now],|r|r.get(0))?;
                if count<=1{return Err(PermissionError::Conflict);}
                repo.connection().execute("UPDATE role_assignments SET revoked_at=?,revoked_by='local-os-owner' WHERE principal_id=? AND role_id='owner' AND revoked_at IS NULL",params![repo.now,user.as_str()])?;
                // A surviving enrollment link must not silently restore the removed top role.
                repo.connection().execute("DELETE FROM management_owner_links WHERE role_id='owner' AND minecraft_uuid IN (SELECT minecraft_uuid FROM user_minecraft_accounts WHERE principal_id=?)",[user.as_str()])?;
            }
            self.audit(repo,local_actor()?,if grant{"management.superadmin.grant"}else{"management.superadmin.revoke"},user.as_str())?;
            Ok(())
        })
    }
    pub(super) fn invitation_issuer(
        &self,
        repo: &Repository<'_>,
        hash: &Hash,
    ) -> Result<Option<Subject>> {
        let issuer:Option<(String,String)>=repo.connection().query_row("SELECT principal_id,device_id FROM management_invitation_issuers WHERE code_hash=?",[hash.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        issuer
            .map(|(p, d)| {
                Ok(Subject {
                    principal: PrincipalId::parse(&p)?,
                    device: DeviceId::parse(&d)?,
                })
            })
            .transpose()
    }
    pub(super) fn redeem_invitation(
        &self,
        repo: &Repository<'_>,
        issuer: &Subject,
        target: &PrincipalId,
        id: &RoleId,
    ) -> Result<()> {
        let work = || -> AdminResult<()> {
            let (current_issuer, _) = self.subject_in(repo, &issuer.device)?;
            if current_issuer != *issuer {
                return Err(PermissionError::Forbidden);
            }
            let policy = repo.policy(Some(issuer))?;
            require(&policy, Capability::InvitationsCreate)?;
            lower(&policy, target, repo.rank(target)?)?;
            let r = role(repo, id.as_str())?;
            lower_role(&policy, &r)?;
            delegated(&policy, &r.grants)?;
            let mut resulting = active_roles(repo, target)?;
            if !resulting.iter().any(|existing| existing == id.as_str()) {
                resulting.push(id.as_str().into());
            }
            delegated_user(repo, &policy, target, &resulting, None)?;
            insert_assignment(repo, target, id, issuer.principal.as_str())
        };
        work().map_err(|_| ManagementError::Forbidden)
    }
}
