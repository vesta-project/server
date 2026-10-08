//! Private SQL/row conversion boundary. All methods use ONE caller-borrowed
//! configured transaction connection; no method opens or owns a transaction.
use super::{
    AssignmentId, BootstrapCode, BuiltinRole, Capability, CodeLifetime, CustomRole, Device,
    DeviceId, DeviceLabel, DisplayName, Effect, Fingerprint, OverrideId, PolicySnapshot, Principal,
    PrincipalId, PrincipalStatus, Purpose, ResourceScope, RoleGrant, RoleId, Subject,
    SubjectRecord,
};
use super::{authorization::Role, domain::system_time};
use crate::{ControlError, IdentityErrorCode, Result};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use vesta_state::StateError;

const CODE_BYTES: usize = 20;
const EXPIRY_BATCH: i64 = 500;

pub(super) struct Repository<'a> {
    connection: &'a Connection,
    server: &'a str,
    pub(super) now: i64,
}
impl<'a> Repository<'a> {
    pub(super) fn new(connection: &'a Connection, server: &'a str, now: i64) -> Self {
        Self {
            connection,
            server,
            now,
        }
    }

    /// The caller-owned unit of work. Repositories never open, commit or roll back a
    /// transaction, so an audit append through this connection shares the caller's
    /// transaction instead of starting a nested one.
    pub(super) fn connection(&self) -> &Connection {
        self.connection
    }

    pub(super) fn policy(&self, subject: Option<&Subject>) -> Result<PolicySnapshot> {
        let Some(subject) = subject else {
            return Ok(PolicySnapshot::none());
        };
        let current: Option<i32> = self
            .connection
            .query_row(
                "SELECT 1 FROM principals p JOIN devices d ON d.principal_id = p.id \
             WHERE p.id = ? AND p.server_id = ? AND p.status = 'active' \
             AND d.id = ? AND d.server_id = ? AND d.revoked_at IS NULL",
                params![
                    subject.principal.as_str(),
                    self.server,
                    subject.device.as_str(),
                    self.server
                ],
                |row| row.get(0),
            )
            .optional()?;
        if current.is_none() {
            return Ok(PolicySnapshot::none());
        }
        // Resolved facts accumulate in locals and become a snapshot exactly once, through
        // the module-private constructor. No intermediate object is ever granted authority.
        let mut rank = 0;
        let mut roles = Vec::new();
        let mut builtins = HashSet::new();
        let mut allows = Vec::new();
        let mut denies = Vec::new();
        for (id, assigned, builtin) in self.active_roles(&subject.principal)? {
            let id = stored(RoleId::parse(&id))?;
            if let Some(builtin) = role_header(&id, assigned, builtin)? {
                builtins.insert(builtin);
            }
            rank = rank.max(assigned);
            self.role_grants(&id, false, &mut allows)?;
            roles.push(id);
        }
        let mut statement = self.connection.prepare(
            "SELECT capability, scope, effect FROM principal_overrides WHERE principal_id = ? \
             AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?) ORDER BY id",
        )?;
        let mut rows = statement.query(params![subject.principal.as_str(), self.now])?;
        while let Some(row) = rows.next()? {
            let capability: String = row.get(0)?;
            let scope: String = row.get(1)?;
            let effect: String = row.get(2)?;
            match effect.as_str() {
                "allow" => add_stored_grant(&mut allows, &capability, &scope, false)?,
                "deny" => add_stored_grant(&mut denies, &capability, &scope, true)?,
                _ => return Err(corrupt()),
            }
        }
        Ok(PolicySnapshot::from_resolved(
            Some(subject.principal.clone()),
            Some(subject.device.clone()),
            rank,
            roles,
            builtins,
            allows,
            denies,
        ))
    }

    /// Ordering is `(created_at, id)` so `PolicySnapshot::roles()` is stable for a given
    /// database. Two assignments made in the same millisecond — which `replace_role` does on
    /// every reassignment — are ordered by their generated id, so the order is reproducible
    /// within one database but not across rebuilds. This is observation only: `rank` is a
    /// maximum, built-in membership is a set, and grants are unioned.
    fn active_roles(&self, principal: &PrincipalId) -> Result<Vec<(String, i32, i32)>> {
        let mut statement = self.connection.prepare(
            "SELECT r.id, r.rank, r.builtin FROM role_assignments a JOIN roles r ON r.id = a.role_id \
             WHERE a.principal_id = ? AND a.revoked_at IS NULL \
             AND (a.expires_at IS NULL OR a.expires_at > ?) ORDER BY a.created_at, a.id",
        )?;
        let rows = statement.query_map(params![principal.as_str(), self.now], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub(super) fn rank(&self, principal: &PrincipalId) -> Result<i32> {
        let mut max = 0;
        for (id, rank, builtin) in self.active_roles(principal)? {
            role_header(&stored(RoleId::parse(&id))?, rank, builtin)?;
            max = max.max(rank);
        }
        Ok(max)
    }

    /// Delegation is stricter than observation: an unsupported allow makes the
    /// role unusable for transfer, not a silently attenuated invented role.
    pub(super) fn role(&self, id: &RoleId) -> Result<Role> {
        let header: Option<(i32, i32)> = self
            .connection
            .query_row(
                "SELECT rank, builtin FROM roles WHERE id = ?",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (rank, builtin) = header.ok_or_else(|| rejected(IdentityErrorCode::NotFound))?;
        let builtin = role_header(id, rank, builtin)?;
        let mut grants = Vec::new();
        self.role_grants(id, true, &mut grants)?;
        Ok(Role {
            id: id.clone(),
            rank,
            builtin,
            grants,
        })
    }

    fn role_grants(&self, id: &RoleId, strict: bool, grants: &mut Vec<RoleGrant>) -> Result<()> {
        let mut statement = self.connection.prepare(
            "SELECT capability, scope FROM role_capabilities WHERE role_id = ? ORDER BY capability, scope",
        )?;
        let mut rows = statement.query([id.as_str()])?;
        while let Some(row) = rows.next()? {
            let capability: String = row.get(0)?;
            let scope: String = row.get(1)?;
            if strict {
                let grant = Capability::from_id(&capability)
                    .and_then(|capability| {
                        ResourceScope::parse(&scope)
                            .ok()
                            .and_then(|scope| RoleGrant::new(capability, scope).ok())
                    })
                    .ok_or_else(|| rejected(IdentityErrorCode::RoleInvalid))?;
                grants.push(grant);
            } else {
                add_stored_grant(grants, &capability, &scope, false)?;
            }
        }
        Ok(())
    }

    pub(super) fn principal_status(&self, id: &PrincipalId) -> Result<PrincipalStatus> {
        let status: Option<String> = self
            .connection
            .query_row(
                "SELECT status FROM principals WHERE id = ? AND server_id = ?",
                params![id.as_str(), self.server],
                |row| row.get(0),
            )
            .optional()?;
        decode_status(&status.ok_or_else(|| rejected(IdentityErrorCode::NotFound))?)
    }
    pub(super) fn active_principal(&self, id: &PrincipalId) -> Result<()> {
        if self.principal_status(id)? != PrincipalStatus::Active {
            return Err(rejected(IdentityErrorCode::InvalidRequest));
        }
        Ok(())
    }

    pub(super) fn owner_exists(&self) -> Result<bool> {
        // Corrupt reserved headers must not reopen bootstrap or confer a tier.
        let owner: RoleId = BuiltinRole::Owner.into();
        self.role(&owner)?;
        let exists: Option<i32> = self.connection.query_row(
            "SELECT 1 FROM role_assignments a JOIN principals p ON p.id = a.principal_id \
             WHERE a.role_id = ? AND a.revoked_at IS NULL \
             AND (a.expires_at IS NULL OR a.expires_at > ?) AND p.status = 'active' AND p.server_id = ? LIMIT 1",
            params![owner.as_str(), self.now, self.server], |row| row.get(0),
        ).optional()?;
        Ok(exists.is_some())
    }

    pub(super) fn record(&self, id: &PrincipalId) -> Result<Option<SubjectRecord>> {
        let raw: Option<(String, String, i64)> = self.connection.query_row(
            "SELECT display_name, status, created_at FROM principals WHERE id = ? AND server_id = ?",
            params![id.as_str(), self.server], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        let Some((name, status, created)) = raw else {
            return Ok(None);
        };
        let principal = Principal {
            id: id.clone(),
            name: stored(DisplayName::parse(&name))?,
            status: decode_status(&status)?,
            created_at: system_time(created)?,
        };
        let mut devices = Vec::new();
        let mut statement = self.connection.prepare(
            "SELECT id, label, public_key_sha256, created_at, revoked_at FROM devices \
             WHERE principal_id = ? AND server_id = ? ORDER BY created_at, id",
        )?;
        let mut rows = statement.query(params![id.as_str(), self.server])?;
        while let Some(row) = rows.next()? {
            devices.push(Device {
                id: stored(DeviceId::parse(&row.get::<_, String>(0)?))?,
                principal: id.clone(),
                label: stored(DeviceLabel::parse(&row.get::<_, String>(1)?))?,
                fingerprint: stored(Fingerprint::parse(&row.get::<_, String>(2)?))?,
                created_at: system_time(row.get(3)?)?,
                revoked_at: row.get::<_, Option<i64>>(4)?.map(system_time).transpose()?,
            });
        }
        Ok(Some(SubjectRecord { principal, devices }))
    }

    pub(super) fn use_nonce(&self, nonce: &str, purpose: Purpose) -> Result<()> {
        let digest = sha256(&format!("vesta-local-proof-v1:{nonce}"));
        if self.connection.execute(
            "INSERT INTO local_authority_uses (nonce_hash, purpose, used_at) VALUES (?,?,?) \
             ON CONFLICT(nonce_hash) DO NOTHING",
            params![digest, purpose.id(), self.now],
        )? != 1
        {
            return Err(rejected(IdentityErrorCode::LocalAuthorityRequired));
        }
        Ok(())
    }

    pub(super) fn issue_code(
        &self,
        purpose: Purpose,
        lifetime: CodeLifetime,
    ) -> Result<BootstrapCode> {
        let expiry = lifetime.deadline(self.now)?;
        let expires_at = system_time(expiry)?;
        self.connection.execute(
            "UPDATE bootstrap_codes SET invalidated_at = ? WHERE server_id = ? \
             AND consumed_at IS NULL AND invalidated_at IS NULL",
            params![self.now, self.server],
        )?;
        let mut bytes = [0_u8; CODE_BYTES];
        random(&mut bytes)?;
        let code = URL_SAFE_NO_PAD.encode(bytes);
        self.connection.execute(
            "INSERT INTO bootstrap_codes (id, server_id, purpose, code_hash, created_at, expires_at) VALUES (?,?,?,?,?,?)",
            params![random_id()?, self.server, purpose.id(), sha256(&code), self.now, expiry],
        )?;
        Ok(BootstrapCode {
            code,
            purpose,
            expires_at_ms: expiry,
            expires_at,
        })
    }

    pub(super) fn consume_code(&self, code: &str) -> Result<(String, Purpose)> {
        let found: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT id, purpose FROM bootstrap_codes WHERE code_hash = ? AND server_id = ? \
             AND consumed_at IS NULL AND invalidated_at IS NULL AND expires_at > ?",
                params![sha256(code), self.server, self.now],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (id, purpose) = found.ok_or_else(|| rejected(IdentityErrorCode::CodeInvalid))?;
        // Even imported/damaged purpose rows cannot authorize a different use.
        let purpose = match purpose.as_str() {
            "owner_bootstrap" => Purpose::OwnerBootstrap,
            "owner_recovery" => Purpose::OwnerRecovery,
            _ => return Err(rejected(IdentityErrorCode::CodeInvalid)),
        };
        stored(AssignmentId::parse(&id))?; // validate internal identifier before audit metadata
        if self.connection.execute(
            "UPDATE bootstrap_codes SET consumed_at = ? WHERE id = ? AND consumed_at IS NULL",
            params![self.now, id],
        )? != 1
        {
            return Err(rejected(IdentityErrorCode::CodeInvalid));
        }
        Ok((id, purpose))
    }

    pub(super) fn insert_principal(&self, name: &DisplayName) -> Result<PrincipalId> {
        let id = PrincipalId::parse(&random_id()?)?;
        self.connection.execute(
            "INSERT INTO principals (id, server_id, display_name, status, created_at, updated_at) VALUES (?,?,?,'active',?,?)",
            params![id.as_str(), self.server, name.as_str(), self.now, self.now],
        )?;
        Ok(id)
    }
    pub(super) fn insert_device(
        &self,
        principal: &PrincipalId,
        label: &DeviceLabel,
        fingerprint: &Fingerprint,
    ) -> Result<DeviceId> {
        // Imported uppercase digests must conflict too, including revoked devices.
        let existing: Option<i32> = self
            .connection
            .query_row(
                "SELECT 1 FROM devices WHERE public_key_sha256 = ? COLLATE NOCASE LIMIT 1",
                [fingerprint.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Err(rejected(IdentityErrorCode::Conflict));
        }
        let id = DeviceId::parse(&random_id()?)?;
        self.connection.execute(
            "INSERT INTO devices (id, server_id, principal_id, label, public_key_sha256, created_at) VALUES (?,?,?,?,?,?)",
            params![id.as_str(), self.server, principal.as_str(), label.as_str(), fingerprint.as_str(), self.now],
        )?;
        Ok(id)
    }

    pub(super) fn replace_role(
        &self,
        principal: &PrincipalId,
        role: &RoleId,
        expiry: Option<i64>,
        by: &str,
    ) -> Result<AssignmentId> {
        // Expired but unrevoked assignments are also replaced; no resurrection.
        self.connection.execute(
            "UPDATE role_assignments SET revoked_at = ?, revoked_by = ? WHERE principal_id = ? AND revoked_at IS NULL",
            params![self.now, by, principal.as_str()],
        )?;
        let id = AssignmentId::parse(&random_id()?)?;
        self.connection.execute(
            "INSERT INTO role_assignments (id, principal_id, role_id, granted_by, created_at, expires_at) VALUES (?,?,?,?,?,?)",
            params![id.as_str(), principal.as_str(), role.as_str(), by, self.now, expiry],
        )?;
        Ok(id)
    }
    pub(super) fn insert_override(
        &self,
        principal: &PrincipalId,
        grant: &RoleGrant,
        effect: Effect,
        expiry: Option<i64>,
        by: &str,
    ) -> Result<OverrideId> {
        let id = OverrideId::parse(&random_id()?)?;
        self.connection.execute(
            "INSERT INTO principal_overrides (id, principal_id, capability, scope, effect, granted_by, created_at, expires_at) \
             VALUES (?,?,?,?,?,?,?,?)",
            params![id.as_str(), principal.as_str(), grant.capability().id(), grant.scope().text(), effect.stored(), by, self.now, expiry],
        )?;
        Ok(id)
    }
    pub(super) fn create_role(&self, role: &CustomRole, by: &str) -> Result<RoleId> {
        let exists: Option<i32> = self
            .connection
            .query_row(
                "SELECT 1 FROM roles WHERE name = ? COLLATE NOCASE",
                [role.name().as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_some() {
            return Err(rejected(IdentityErrorCode::Conflict));
        }
        let id = RoleId::parse(&format!("custom-{}", random_id()?))?;
        self.connection.execute(
            "INSERT INTO roles (id, name, rank, builtin, created_by, created_at) VALUES (?,?,?,0,?,?)",
            params![id.as_str(), role.name().as_str(), role.rank().value(), by, self.now],
        )?;
        for grant in role.grants() {
            self.connection.execute(
                "INSERT INTO role_capabilities (role_id, capability, scope) VALUES (?,?,?)",
                params![id.as_str(), grant.capability().id(), grant.scope().text()],
            )?;
        }
        Ok(id)
    }

    pub(super) fn grant_principal(&self, table: GrantTable, id: &str) -> Result<PrincipalId> {
        let principal: Option<String> = self
            .connection
            .query_row(
                &format!(
                    "SELECT g.principal_id FROM {} g JOIN principals p ON p.id = g.principal_id \
                WHERE g.id = ? AND g.revoked_at IS NULL AND p.server_id = ?",
                    table.name()
                ),
                params![id, self.server],
                |row| row.get(0),
            )
            .optional()?;
        stored(PrincipalId::parse(
            &principal.ok_or_else(|| rejected(IdentityErrorCode::NotFound))?,
        ))
    }
    pub(super) fn revoke_grant(&self, table: GrantTable, id: &str, by: &str) -> Result<()> {
        if self.connection.execute(
            &format!(
                "UPDATE {} SET revoked_at = ?, revoked_by = ? WHERE id = ? AND revoked_at IS NULL",
                table.name()
            ),
            params![self.now, by, id],
        )? != 1
        {
            return Err(rejected(IdentityErrorCode::NotFound));
        }
        Ok(())
    }
    pub(super) fn device_principal(&self, id: &DeviceId) -> Result<PrincipalId> {
        let principal: Option<String> = self
            .connection
            .query_row(
                "SELECT d.principal_id FROM devices d JOIN principals p ON p.id = d.principal_id \
             WHERE d.id = ? AND d.server_id = ? AND p.server_id = ? AND d.revoked_at IS NULL",
                params![id.as_str(), self.server, self.server],
                |row| row.get(0),
            )
            .optional()?;
        stored(PrincipalId::parse(
            &principal.ok_or_else(|| rejected(IdentityErrorCode::NotFound))?,
        ))
    }
    pub(super) fn revoke_device(&self, id: &DeviceId, by: &str) -> Result<()> {
        if self.connection.execute(
            "UPDATE devices SET revoked_at = ?, revoked_by = ? WHERE id = ? AND revoked_at IS NULL",
            params![self.now, by, id.as_str()],
        )? != 1
        {
            return Err(rejected(IdentityErrorCode::NotFound));
        }
        Ok(())
    }

    pub(super) fn recover_access(&self) -> Result<(usize, usize)> {
        let principals = {
            let mut statement = self.connection.prepare(
                "SELECT DISTINCT a.principal_id FROM role_assignments a JOIN principals p ON p.id = a.principal_id \
                 WHERE a.role_id IN (?,?) AND a.revoked_at IS NULL AND p.server_id = ? \
                 AND (a.expires_at IS NULL OR a.expires_at > ?)",
            )?;
            let rows = statement.query_map(
                params![
                    BuiltinRole::Owner.id(),
                    BuiltinRole::Administrator.id(),
                    self.server,
                    self.now
                ],
                |row| row.get::<_, String>(0),
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let assignments = self.connection.execute(
            "UPDATE role_assignments SET revoked_at = ?, revoked_by = 'local-authority' WHERE role_id IN (?,?) \
             AND revoked_at IS NULL AND principal_id IN (SELECT id FROM principals WHERE server_id = ?)",
            params![self.now, BuiltinRole::Owner.id(), BuiltinRole::Administrator.id(), self.server],
        )?;
        let mut devices = 0;
        for principal in principals {
            let principal = stored(PrincipalId::parse(&principal))?;
            devices += self.connection.execute(
                "UPDATE devices SET revoked_at = ?, revoked_by = 'local-authority' \
                 WHERE principal_id = ? AND server_id = ? AND revoked_at IS NULL",
                params![self.now, principal.as_str(), self.server],
            )?;
        }
        Ok((assignments, devices))
    }

    /// Marker and audit are owned by the same service transaction. Limit is per
    /// table, and evaluation already enforces expiry without relying on this sweep.
    pub(super) fn expired(&self, table: GrantTable) -> Result<Vec<(String, PrincipalId)>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT g.id, g.principal_id FROM {} g JOIN principals p ON p.id = g.principal_id \
             WHERE g.expires_at IS NOT NULL AND g.expires_at <= ? AND g.revoked_at IS NULL \
             AND g.expiry_recorded_at IS NULL AND p.server_id = ? ORDER BY g.expires_at, g.id LIMIT ?", table.name(),
        ))?;
        let mut rows = statement.query(params![self.now, self.server, EXPIRY_BATCH])?;
        let mut expired = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            match table {
                GrantTable::Assignments => {
                    stored(AssignmentId::parse(&id))?;
                }
                GrantTable::Overrides => {
                    stored(OverrideId::parse(&id))?;
                }
            }
            expired.push((id, stored(PrincipalId::parse(&row.get::<_, String>(1)?))?));
        }
        Ok(expired)
    }
    pub(super) fn mark_expired(&self, table: GrantTable, id: &str) -> Result<()> {
        if self.connection.execute(
            &format!(
                "UPDATE {} SET expiry_recorded_at = ? WHERE id = ? AND expiry_recorded_at IS NULL",
                table.name()
            ),
            params![self.now, id],
        )? != 1
        {
            return Err(corrupt());
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) enum GrantTable {
    Assignments,
    Overrides,
}
impl GrantTable {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Assignments => "role_assignments",
            Self::Overrides => "principal_overrides",
        }
    }
    pub(super) const fn action(self) -> &'static str {
        match self {
            Self::Assignments => "authority.role.revoke",
            Self::Overrides => "authority.override.revoke",
        }
    }
}

fn role_header(id: &RoleId, rank: i32, flag: i32) -> Result<Option<BuiltinRole>> {
    match (BuiltinRole::from_id(id.as_str()), flag) {
        (Some(role), 1) if rank == role.rank() => Ok(Some(role)),
        (None, 0) if (1..BuiltinRole::Administrator.rank()).contains(&rank) => Ok(None),
        _ => Err(corrupt()),
    }
}

fn add_stored_grant(grants: &mut Vec<RoleGrant>, id: &str, text: &str, deny: bool) -> Result<()> {
    let Some(capability) = Capability::from_id(id) else {
        // A deny we cannot interpret fails the entire resolution closed.
        return if deny { Err(corrupt()) } else { Ok(()) };
    };
    let grant = ResourceScope::parse(text).and_then(|scope| RoleGrant::new(capability, scope));
    match grant {
        Ok(grant) => grants.push(grant),
        Err(_) if deny => grants.push(RoleGrant::new(capability, ResourceScope::ALL)?),
        Err(_) => {} // unsupported allow cannot grant
    }
    Ok(())
}
fn decode_status(status: &str) -> Result<PrincipalStatus> {
    match status {
        "active" => Ok(PrincipalStatus::Active),
        "disabled" => Ok(PrincipalStatus::Disabled),
        _ => Err(corrupt()),
    }
}
fn stored<T>(value: Result<T>) -> Result<T> {
    value.map_err(|_| corrupt())
}
fn corrupt() -> ControlError {
    StateError::new("state_identity_invalid").into()
}
fn rejected(code: IdentityErrorCode) -> ControlError {
    ControlError::rejected(code)
}
fn sha256(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}
fn random(bytes: &mut [u8]) -> Result<()> {
    getrandom::fill(bytes).map_err(|_| StateError::new("state_random_unavailable").into())
}
fn random_id() -> Result<String> {
    let mut bytes = [0_u8; 16];
    random(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(uuid::Uuid::from_bytes(bytes).to_string())
}
