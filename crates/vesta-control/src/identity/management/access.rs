//! One canonical user/session with distinct capability domains. Automatic role
//! sources are validated here and can never contribute management capabilities.
use super::*;
use crate::config::PackAccessMode;
use crate::identity::minecraft::{
    MinecraftPurpose, MinecraftUuid, ServerUuid, VerifiedMinecraftProfile,
};
use crate::identity::{
    Capability, Decision, DenyReason, PasswordVerification, PasswordVerificationStatus,
};
use crate::pack_access::{PackAccessPolicy, PackGrant, PackSession};
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AccessSettings {
    pub anonymous: String,
    pub new_user: String,
    pub whitelist: String,
    pub whitelist_enabled: bool,
    pub password: String,
}
impl Default for AccessSettings {
    fn default() -> Self {
        Self {
            anonymous: "public".into(),
            new_user: "public".into(),
            whitelist: "player".into(),
            whitelist_enabled: true,
            password: "player".into(),
        }
    }
}
/// Opaque lookup/admission reference, never a bearer credential or policy grant.
#[derive(Clone, Copy, Eq, PartialEq, Hash)]
pub struct DeviceSessionReference {
    pub(crate) key: Hash,
    pub(crate) server: ServerUuid,
    pub(crate) anonymous: bool,
}
impl fmt::Debug for DeviceSessionReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceSessionReference[<redacted>]")
    }
}
fn safe(capability: Capability) -> bool {
    matches!(
        capability,
        Capability::PublicInfo
            | Capability::PackDownload
            | Capability::PackRealtime
            | Capability::PackAccess
    )
}
fn role_allows(repo: &Repository<'_>, role: &str, capability: Capability) -> Result<bool> {
    let role = repo.role(&RoleId::parse(role).map_err(|_| ManagementError::InvalidRequest)?)?;
    if role
        .grants
        .iter()
        .any(|g| !safe(g.capability()) || !g.scope().is_all())
    {
        return Err(ManagementError::InvalidRequest);
    }
    Ok(role.grants.iter().any(|g| {
        g.capability() == capability
            || (g.capability() == Capability::PackAccess
                && matches!(
                    capability,
                    Capability::PackDownload | Capability::PackRealtime
                ))
    }))
}
fn source_settings(repo: &Repository<'_>) -> Result<AccessSettings> {
    let text: String = repo.connection().query_row(
        "SELECT settings_json FROM access_role_sources WHERE singleton = 1",
        [],
        |r| r.get(0),
    )?;
    serde_json::from_str(&text).map_err(|_| ManagementError::Unavailable)
}
fn sources(repo: &Repository<'_>) -> Result<AccessSettings> {
    let settings = source_settings(repo)?;
    for role in [
        &settings.anonymous,
        &settings.new_user,
        &settings.whitelist,
        &settings.password,
    ] {
        role_allows(repo, role, Capability::PublicInfo)?;
    }
    Ok(settings)
}
impl ManagementAccess {
    pub(super) fn check_device_capacity(
        &self,
        repo: &Repository<'_>,
        owner_source: bool,
    ) -> Result<()> {
        let count: u32 = repo.connection().query_row(
            "SELECT count(*) FROM management_device_keys",
            [],
            |r| r.get(0),
        )?;
        let limit = if owner_source {
            MAX_REGISTERED_DEVICES
        } else {
            MAX_PUBLIC_DEVICE_KEYS
        };
        if count as usize >= limit {
            Err(ManagementError::Saturated)
        } else {
            Ok(())
        }
    }
    pub fn access_local(&self) -> Result<AccessSettings> {
        self.write(source_settings)
    }
    pub fn set_access_local(&self, settings: AccessSettings) -> Result<()> {
        self.write(|repo| {
            for role in [
                &settings.anonymous,
                &settings.new_user,
                &settings.whitelist,
                &settings.password,
            ] {
                role_allows(repo, role, Capability::PublicInfo)?;
            }
            let json =
                serde_json::to_string(&settings).map_err(|_| ManagementError::Unavailable)?;
            if json.len() > 4096 {
                return Err(ManagementError::InvalidRequest);
            }
            repo.connection().execute(
                "UPDATE access_role_sources SET settings_json = ? WHERE singleton = 1",
                [&json],
            )?;
            self.audit(
                repo,
                local_actor()?,
                "access.sources.set",
                "automatic-pack-roles",
            )
        })
    }
    pub fn patch_access_local(&self, fields: &[(String, String)]) -> Result<AccessSettings> {
        if fields.is_empty() || fields.len() > 5 {
            return Err(ManagementError::InvalidRequest);
        }
        self.write(|repo| {
            // The protected local owner may repair a role source which became
            // unsafe. Validate the complete patched result before admission.
            let mut settings = source_settings(repo)?;
            let mut seen = std::collections::HashSet::new();
            for (key, value) in fields {
                if !seen.insert(key) {
                    return Err(ManagementError::InvalidRequest);
                }
                match key.as_str() {
                    "anonymous" => settings.anonymous = value.clone(),
                    "new_user" => settings.new_user = value.clone(),
                    "whitelist" => settings.whitelist = value.clone(),
                    "password" => settings.password = value.clone(),
                    "whitelist_enabled" => {
                        settings.whitelist_enabled = match value.as_str() {
                            "on" => true,
                            "off" => false,
                            _ => return Err(ManagementError::InvalidRequest),
                        }
                    }
                    _ => return Err(ManagementError::InvalidRequest),
                }
            }
            for role in [
                &settings.anonymous,
                &settings.new_user,
                &settings.whitelist,
                &settings.password,
            ] {
                role_allows(repo, role, Capability::PublicInfo)?;
            }
            let json =
                serde_json::to_string(&settings).map_err(|_| ManagementError::Unavailable)?;
            repo.connection().execute(
                "UPDATE access_role_sources SET settings_json = ? WHERE singleton = 1",
                [&json],
            )?;
            self.audit(
                repo,
                local_actor()?,
                "access.sources.set",
                "automatic-pack-roles",
            )?;
            Ok(settings)
        })
    }
    pub(super) fn create_user(
        &self,
        repo: &Repository<'_>,
        name: &DisplayName,
    ) -> Result<PrincipalId> {
        let principal = repo.insert_principal(name)?;
        let settings = sources(repo)?;
        repo.connection().execute(
            "INSERT INTO user_default_roles VALUES (?,?)",
            params![principal.as_str(), settings.new_user],
        )?;
        Ok(principal)
    }
    /// Anonymous key registration creates only the configured safe new-user role.
    /// Key possession is still required to sign in; registration grants no session.
    pub fn enroll_device(&self, device_name: &str, public_key: &str) -> Result<Enrolled> {
        self.enroll_device_for(device_name, public_key, None)
    }
    pub fn enroll_device_for(
        &self,
        device_name: &str,
        public_key: &str,
        token: Option<&str>,
    ) -> Result<Enrolled> {
        let authenticated = token.map(|t| self.authenticate(t)).transpose()?;
        let key = decode::<32>(public_key)?;
        let name = DisplayName::parse(device_name).map_err(|_| ManagementError::InvalidRequest)?;
        let label = DeviceLabel::parse(device_name).map_err(|_| ManagementError::InvalidRequest)?;
        let fingerprint = Fingerprint::parse(&hex::encode(digest::digest(&digest::SHA256, &key)))?;
        self.write(|repo| {
            let existing: Option<(String,String)> = repo.connection().query_row("SELECT principal_id,id FROM devices WHERE server_id = ? AND public_key_sha256 = ? AND revoked_at IS NULL", params![self.server_id(), fingerprint.as_str()], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some(subject)=&authenticated {self.subject_in(repo,&subject.device)?;}
            if let Some((principal,device)) = existing { if authenticated.as_ref().is_some_and(|a|a.principal.as_str()!=principal) {return Err(ManagementError::SessionInvalid);} self.subject_in(repo, &DeviceId::parse(&device)?)?; return Ok(Enrolled { server_id:self.server_id().into(),principal_id:principal,device_id:device,role:String::new() }); }
            let principal = if let Some(subject)=&authenticated {subject.principal.clone()} else {self.create_user(repo,&name)?};
            let owner_source=authenticated.as_ref().map(|subject|repo.policy(Some(subject)).map(|p|p.check(Capability::SecurityManage,None).allowed())).transpose()?.unwrap_or(false);
            self.check_device_capacity(repo,owner_source)?;
            let device = repo.insert_device(&principal,&label,&fingerprint)?;
            repo.connection().execute("INSERT INTO management_device_keys (device_id,server_id,public_key) VALUES (?,?,?)", params![device.as_str(),self.server_id(),key.as_slice()])?;
            let subject = Subject { principal:principal.clone(),device:device.clone() };
            self.audit(repo,device_actor(&subject)?,"identity.device.enroll",device.as_str())?;
            Ok(Enrolled { server_id:self.server_id().into(),principal_id:principal.to_string(),device_id:device.to_string(),role:String::new() })
        })
    }
    pub fn enroll_minecraft_for(
        &self,
        profile: VerifiedMinecraftProfile,
        device_name: &str,
        public_key: &str,
        session: Option<&str>,
    ) -> Result<Enrolled> {
        self.enroll_profile(
            profile,
            device_name,
            public_key,
            session,
            MinecraftPurpose::ManagementEnrollment,
        )
    }
    pub fn enroll_pack_profile(
        &self,
        profile: VerifiedMinecraftProfile,
        device_name: &str,
        public_key: &str,
        session: Option<&str>,
    ) -> Result<Enrolled> {
        self.enroll_profile(
            profile,
            device_name,
            public_key,
            session,
            MinecraftPurpose::Pack,
        )
    }
    fn enroll_profile(
        &self,
        profile: VerifiedMinecraftProfile,
        device_name: &str,
        public_key: &str,
        token: Option<&str>,
        purpose: MinecraftPurpose,
    ) -> Result<Enrolled> {
        if profile.purpose() != purpose
            || profile.server_uuid().as_uuid().to_string() != self.server_id()
        {
            return Err(ManagementError::SessionInvalid);
        }
        let authenticated = token.map(|t| self.authenticate(t)).transpose()?;
        let key = decode::<32>(public_key)?;
        let name = DisplayName::parse(device_name).map_err(|_| ManagementError::InvalidRequest)?;
        let label = DeviceLabel::parse(device_name).map_err(|_| ManagementError::InvalidRequest)?;
        let fingerprint = Fingerprint::parse(&hex::encode(digest::digest(&digest::SHA256, &key)))?;
        let uuid = profile.uuid().as_uuid().to_string();
        self.write(|repo| {
            let c = repo.connection();
            if let Some(subject) = &authenticated && !repo.policy(Some(subject))?.has_current_identity() { return Err(ManagementError::SessionInvalid); }
            let mapped: Option<String> = c.query_row("SELECT principal_id FROM user_minecraft_accounts WHERE server_id = ? AND minecraft_uuid = ?", params![self.server_id(),uuid], |r|r.get(0)).optional()?;
            let existing: Option<(String,String)> = c.query_row("SELECT principal_id,id FROM devices WHERE server_id = ? AND public_key_sha256 = ? AND revoked_at IS NULL", params![self.server_id(),fingerprint.as_str()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let principal = if let Some(id) = mapped {
                if authenticated.as_ref().is_some_and(|a|a.principal.as_str()!=id) { return Err(ManagementError::SessionInvalid); }
                PrincipalId::parse(&id)?
            } else if let Some(subject) = &authenticated { subject.principal.clone() }
            else { if existing.is_some() { return Err(ManagementError::SessionInvalid); } self.create_user(repo,&name)? };
            repo.active_principal(&principal)?;
            // UNIQUE principal binding prevents silent account merges. Existing-key
            // attachment needs a live session unless it already belongs to this UUID.
            let previously: Option<String> = c.query_row("SELECT minecraft_uuid FROM user_minecraft_accounts WHERE server_id = ? AND principal_id = ?",params![self.server_id(),principal.as_str()],|r|r.get(0)).optional()?;
            if previously.as_ref().is_some_and(|old|old != &uuid) { return Err(ManagementError::SessionInvalid); }
            c.execute("INSERT INTO user_minecraft_accounts VALUES (?,?,?) ON CONFLICT(server_id,minecraft_uuid) DO NOTHING",params![self.server_id(),uuid,principal.as_str()])?;
            let role: Option<String> = c.query_row("SELECT role_id FROM management_owner_links WHERE server_id = ? AND minecraft_uuid = ?",params![self.server_id(),uuid],|r|r.get(0)).optional()?;
            let device = if let Some((owner,id))=existing { if owner != principal.as_str() { return Err(ManagementError::SessionInvalid); } DeviceId::parse(&id)? }
                else { self.check_device_capacity(repo,role.is_some())?; let device=repo.insert_device(&principal,&label,&fingerprint)?; c.execute("INSERT INTO management_device_keys VALUES (?,?,?,?)",params![device.as_str(),self.server_id(),key.as_slice(),uuid])?; device };
            if let Some(role)=&role { repo.replace_role(&principal,&RoleId::parse(role)?,None,"local-owner-minecraft-link")?; }
            let subject=Subject { principal:principal.clone(),device:device.clone() };
            self.audit(repo,device_actor(&subject)?,"management.minecraft.enroll",device.as_str())?;
            Ok(Enrolled {server_id:self.server_id().into(),principal_id:principal.to_string(),device_id:device.to_string(),role:role.unwrap_or_default()})
        })
    }
    pub fn grant_password(
        &self,
        token: &str,
        verification: &PasswordVerification,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<()> {
        if verification.status() != PasswordVerificationStatus::Match
            || verification.server_id() != self.server_id()
            || policy.server.as_uuid().to_string() != self.server_id()
            || policy.mode != PackAccessMode::MinecraftOrPassword
            || policy.password != Some(verification.revision())
        {
            return Err(ManagementError::Forbidden);
        }
        let subject = self.authenticate(token)?;
        self.write(|repo| {
            if !repo.policy(Some(&subject))?.has_current_identity() { return Err(ManagementError::SessionInvalid); }
            let revision:Option<i64>=repo.connection().query_row("SELECT revision FROM pack_passwords WHERE server_id=? AND revoked_at IS NULL",[self.server_id()],|r|r.get(0)).optional()?;
            if revision!=i64::try_from(verification.revision().value()).ok() {return Err(ManagementError::Forbidden);}
            let settings=sources(repo)?;
            role_allows(repo,&settings.password,Capability::PackDownload)?;
            repo.connection().execute("INSERT INTO device_password_grants VALUES (?,?,?) ON CONFLICT(device_id) DO UPDATE SET role_id=excluded.role_id,password_revision=excluded.password_revision",params![subject.device.as_str(),settings.password,i64::try_from(verification.revision().value()).map_err(|_|ManagementError::Unavailable)?])?;
            self.audit(repo,device_actor(&subject)?,"access.password.grant",subject.device.as_str())
        })
    }
    pub fn revoke_session(&self, token: &str) -> Result<bool> {
        decode::<32>(token).map_err(|_| ManagementError::SessionInvalid)?;
        let key = bound_hash(b"session", self.server_id(), token);
        Ok(self
            .stores
            .lock()
            .map_err(|_| ManagementError::Unavailable)?
            .sessions
            .remove(&key)
            .is_some())
    }
    pub fn session_count(&self, now: Instant) -> Result<usize> {
        let mut stores = self
            .stores
            .lock()
            .map_err(|_| ManagementError::Unavailable)?;
        stores.prune(self.now());
        stores.sessions.retain(|_, s| now < s.monotonic_expires);
        Ok(stores.sessions.len())
    }
    pub fn pack_reference(&self, token: Option<&str>) -> Result<DeviceSessionReference> {
        let server =
            ServerUuid::parse(self.server_id()).map_err(|_| ManagementError::Unavailable)?;
        if let Some(token) = token {
            self.authenticate(token)?;
            Ok(DeviceSessionReference {
                key: bound_hash(b"session", self.server_id(), token),
                server,
                anonymous: false,
            })
        } else {
            Ok(DeviceSessionReference {
                key: bound_hash(b"anonymous", self.server_id(), "public"),
                server,
                anonymous: true,
            })
        }
    }
    pub fn authorize_pack(
        &self,
        reference: DeviceSessionReference,
        capability: Capability,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<PackSession> {
        if !safe(capability)
            || reference.server != policy.server
            || policy.server.as_uuid().to_string() != self.server_id()
        {
            return Err(ManagementError::SessionInvalid);
        }
        let observation = if reference.anonymous {
            None
        } else {
            let mut stores = self
                .stores
                .lock()
                .map_err(|_| ManagementError::Unavailable)?;
            stores.prune(self.now());
            let session = stores
                .sessions
                .get(&reference.key)
                .ok_or(ManagementError::SessionInvalid)?;
            if session.server != self.server_id() || now >= session.monotonic_expires {
                stores.sessions.remove(&reference.key);
                return Err(ManagementError::SessionInvalid);
            }
            Some((session.subject.clone(), session.monotonic_expires))
        };
        self.write(|repo| {
            let settings=sources(repo)?;
            let Some((subject,expires_at))=&observation else {
                if !role_allows(repo,&settings.anonymous,capability)? {return Err(ManagementError::Forbidden);}
                return Ok(Some(PackSession {grant:PackGrant::Role,expires_at:now+std::time::Duration::from_secs(60)}));
            };
            self.subject_in(repo,&subject.device)?;
            let current=repo.policy(Some(subject))?;
            let decision=current.check(capability,None);
            if matches!(capability,Capability::PackDownload|Capability::PackRealtime) && current.check(Capability::PackAccess,None)==Decision::Deny(DenyReason::ExplicitDeny) {return Err(ManagementError::Forbidden);}
            if decision==Decision::Deny(DenyReason::ExplicitDeny) {return Err(ManagementError::Forbidden);}
            let explicit=decision.allowed() || (matches!(capability,Capability::PackDownload|Capability::PackRealtime) && current.check(Capability::PackAccess,None).allowed());
            let default: Option<String>=repo.connection().query_row("SELECT role_id FROM user_default_roles WHERE principal_id=?",[subject.principal.as_str()],|r|r.get(0)).optional()?;
            let mut allowed=explicit;
            if let Some(role)=default {allowed|=role_allows(repo,&role,capability)?;}
            let uuid: Option<String>=repo.connection().query_row("SELECT minecraft_uuid FROM user_minecraft_accounts WHERE server_id=? AND principal_id=?",params![self.server_id(),subject.principal.as_str()],|r|r.get(0)).optional()?;
            let mut grant=PackGrant::Role;
            if let Some(uuid)=uuid {
                let player=MinecraftUuid::parse(&uuid).map_err(|_|ManagementError::Unavailable)?;
                if settings.whitelist_enabled && policy.properties.is_some_and(|p|p.online_mode()) && policy.whitelist.contains(player) && !policy.bans.contains(player) {
                    allowed|=role_allows(repo,&settings.whitelist,capability)?;grant=PackGrant::Minecraft(player);
                }
            }
            let password: Option<(String,i64)>=repo.connection().query_row("SELECT role_id,password_revision FROM device_password_grants WHERE device_id=?",[subject.device.as_str()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((role,revision))=password {
                let active:Option<i64>=repo.connection().query_row("SELECT revision FROM pack_passwords WHERE server_id=? AND revoked_at IS NULL",[self.server_id()],|r|r.get(0)).optional()?;
                if active==Some(revision) && policy.mode==PackAccessMode::MinecraftOrPassword && policy.password.is_some_and(|r|i64::try_from(r.value()).ok()==Some(revision)) {
                    allowed|=role_allows(repo,&role,capability)?;grant=PackGrant::Password(crate::identity::PasswordRevision::new(u64::try_from(revision).map_err(|_|ManagementError::Unavailable)?).map_err(|_|ManagementError::Unavailable)?);
                } else {repo.connection().execute("DELETE FROM device_password_grants WHERE device_id=?",[subject.device.as_str()])?;}
            }
            Ok(allowed.then_some(PackSession {grant,expires_at:*expires_at}))
        })?.ok_or(ManagementError::Forbidden)
    }
}

#[cfg(test)]
impl ManagementAccess {
    /// Exercise the REAL device/signature/session path with test-only credential
    /// observations. No fake session store or production issuer is introduced.
    pub(crate) fn test_pack_session(
        &self,
        grant: PackGrant,
        now: Instant,
    ) -> Result<(Secret, PackSession)> {
        self.session_count(now)?;
        use ring::signature::KeyPair;
        let key = ring::signature::Ed25519KeyPair::from_pkcs8(
            ring::signature::Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        let public = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
        let server = ServerUuid::parse(self.server_id()).unwrap();
        let device = if let PackGrant::Minecraft(uuid) = grant {
            self.enroll_minecraft(
                VerifiedMinecraftProfile::for_test_enrollment(server, uuid),
                "Fixture",
                &public,
            )?
        } else {
            self.enroll_device("Fixture", &public)?
        };
        let nonce = self.challenge(&device.device_id)?;
        let signature = URL_SAFE_NO_PAD.encode(
            key.sign(&session_message(self.server_id(), nonce.expose()))
                .as_ref(),
        );
        let token = self.session(&device.device_id, nonce.expose(), &signature)?;
        let expires_at = now + std::time::Duration::from_millis(self.session_ttl_ms as u64);
        self.stores
            .lock()
            .unwrap()
            .sessions
            .get_mut(&bound_hash(
                b"session",
                self.server_id(),
                token.secret.expose(),
            ))
            .unwrap()
            .monotonic_expires = expires_at;
        if let PackGrant::Password(revision) = grant {
            self.write(|repo| {repo.connection().execute("INSERT INTO pack_passwords VALUES (?,?,'test-only-observation',?,?,NULL) ON CONFLICT(server_id) DO UPDATE SET revision=excluded.revision",params![self.server_id(),revision.value() as i64,repo.now,repo.now])?;Ok(())})?;
            let whitelist = crate::whitelist::Whitelist::default();
            let bans = crate::minecraft_admission::BanList::default();
            let policy = PackAccessPolicy {
                server,
                mode: PackAccessMode::MinecraftOrPassword,
                properties: None,
                whitelist: &whitelist,
                bans: &bans,
                password: Some(revision),
            };
            let verification = crate::identity::PasswordVerification::for_test(
                self.server_id(),
                revision,
                PasswordVerificationStatus::Match,
            );
            self.grant_password(token.secret.expose(), &verification, &policy)?;
        }
        Ok((token.secret, PackSession { grant, expires_at }))
    }
}
