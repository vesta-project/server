//! P2 device enrollment and proof-of-possession sessions. Local owner methods are
//! trusted in-process seams used ONLY after native same-account IPC authentication.
//! They are never exposed to HTTP, pack identities or arbitrary bearer callers.
//! Codes are durable/hash-only; challenges and sessions end with the process.
use super::repository::Repository;
use super::{
    BuiltinRole, DeviceId, DeviceLabel, DisplayName, Fingerprint, PrincipalId, RoleId, Subject,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
    signature,
};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
};
use vesta_state::{
    StateDatabase, TransactionError,
    audit::{AuditActor, AuditLog, AuditRecord},
};
use zeroize::Zeroizing;

pub const DEFAULT_PAIR_MINUTES: u16 = 10;
pub const MAX_PAIR_MINUTES: u16 = 60;
pub const MAX_CODES: usize = 32;
pub const MAX_REGISTERED_DEVICES: usize = 1000;
pub const MAX_PUBLIC_DEVICE_KEYS: usize = MAX_REGISTERED_DEVICES - MAX_CODES;
pub const MAX_CHALLENGES: usize = 1024;
pub const MAX_SESSIONS: usize = 4096;
pub const CHALLENGE_TTL_MS: i64 = 60_000;
pub const SESSION_TTL_MS: i64 = 15 * 60_000;
pub const SESSION_PURPOSE: &str = "vesta.management.session.v1";
pub(crate) type Hash = [u8; 32];
type Result<T> = std::result::Result<T, ManagementError>;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementError {
    PairInvalid,
    SessionInvalid,
    InvalidRequest,
    Forbidden,
    Saturated,
    Unavailable,
}
impl ManagementError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::PairInvalid => "management_pair_invalid",
            Self::SessionInvalid => "management_session_invalid",
            Self::InvalidRequest => "invalid_request",
            Self::Forbidden => "management_forbidden",
            Self::Saturated => "management_saturated",
            Self::Unavailable => "management_unavailable",
        }
    }
}
impl From<vesta_state::StateError> for ManagementError {
    fn from(_: vesta_state::StateError) -> Self {
        Self::Unavailable
    }
}
impl From<rusqlite::Error> for ManagementError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Unavailable
    }
}
impl From<crate::ControlError> for ManagementError {
    fn from(_: crate::ControlError) -> Self {
        Self::Unavailable
    }
}

/// No serializer and redacted Debug: exposed only by explicit response builders.
pub struct Secret(Zeroizing<String>);
impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ManagementSecret[<redacted>]")
    }
}
pub struct PairCode {
    pub secret: Secret,
    pub expires_at_ms: i64,
}
pub struct SessionToken {
    pub secret: Secret,
    pub expires_at_ms: i64,
}
#[derive(Debug, Serialize)]
pub struct Enrolled {
    pub server_id: String,
    pub principal_id: String,
    pub device_id: String,
    pub role: String,
}
#[derive(Debug, Serialize)]
pub struct DeviceInfo {
    pub device_id: String,
    pub principal_id: String,
    pub device_name: String,
    pub revoked: bool,
}
struct Challenge {
    device: DeviceId,
    expires: i64,
    monotonic_expires: std::time::Instant,
}
struct Session {
    subject: Subject,
    server: String,
    expires: i64,
    monotonic_expires: std::time::Instant,
}
#[derive(Default)]
struct Stores {
    challenges: HashMap<Hash, Challenge>,
    sessions: HashMap<Hash, Session>,
}
impl Stores {
    fn prune(&mut self, now: i64) {
        self.challenges
            .retain(|_, v| v.expires > now && std::time::Instant::now() < v.monotonic_expires);
        self.sessions
            .retain(|_, v| v.expires > now && std::time::Instant::now() < v.monotonic_expires);
    }
}
#[derive(Clone)]
pub struct ManagementAccess {
    db: StateDatabase,
    session_limit: usize,
    session_ttl_ms: i64,
    audit: AuditLog,
    stores: Arc<Mutex<Stores>>,
}
impl ManagementAccess {
    pub fn new(db: StateDatabase) -> Result<Self> {
        Self::with_limits(db, MAX_SESSIONS, SESSION_TTL_MS)
    }
    pub(crate) fn with_limits(db: StateDatabase, capacity: usize, ttl_ms: i64) -> Result<Self> {
        if capacity == 0 || capacity > MAX_SESSIONS || ttl_ms <= 0 || ttl_ms > SESSION_TTL_MS {
            return Err(ManagementError::InvalidRequest);
        }
        Ok(Self {
            session_limit: capacity,
            session_ttl_ms: ttl_ms,
            audit: AuditLog::new(db.clone())?,
            db,
            stores: Arc::new(Mutex::new(Stores::default())),
        })
    }
    pub fn server_id(&self) -> &str {
        self.db.server_id()
    }
    fn now(&self) -> i64 {
        self.db.clock().millis()
    }
    fn write<T>(&self, work: impl FnOnce(&Repository<'_>) -> Result<T>) -> Result<T> {
        match self.db.write_with(|tx| {
            work(&Repository::new(
                tx.connection(),
                self.server_id(),
                self.now(),
            ))
        }) {
            Ok(v) => Ok(v),
            Err(TransactionError::Work(e)) => Err(e),
            Err(_) => Err(ManagementError::Unavailable),
        }
    }
    fn audit(
        &self,
        repo: &Repository<'_>,
        actor: AuditActor,
        action: &str,
        target: &str,
    ) -> Result<()> {
        let record = AuditRecord::new(
            actor,
            action.to_owned(),
            target.to_owned(),
            "success".to_owned(),
            None,
            std::iter::empty::<(String, Option<String>)>(),
        )?;
        self.audit.append(repo.connection(), &record)?;
        Ok(())
    }
    /// CLI OS account is the owner by explicit owner decision. Call this ONLY
    /// from the protected native local adapter, never an HTTP route.
    pub fn issue_local(&self, role: BuiltinRole, minutes: u16) -> Result<PairCode> {
        if !(1..=MAX_PAIR_MINUTES).contains(&minutes) {
            return Err(ManagementError::InvalidRequest);
        }
        let secret = random_secret()?;
        self.write(|repo| {
            let c = repo.connection();
            c.execute(
                "DELETE FROM management_pairing_codes WHERE expires_at <= ?",
                [repo.now],
            )?;
            let count: u32 =
                c.query_row("SELECT count(*) FROM management_pairing_codes", [], |r| {
                    r.get(0)
                })?;
            if count as usize >= MAX_CODES {
                return Err(ManagementError::Saturated);
            }
            let id: RoleId = role.into();
            repo.role(&id)?;
            let expires = deadline(repo.now, i64::from(minutes) * 60_000)?;
            let hash = bound_hash(b"pair", self.server_id(), secret.expose());
            c.execute(
                "INSERT INTO management_pairing_codes VALUES (?,?,?,?,?)",
                params![
                    hash.as_slice(),
                    self.server_id(),
                    id.as_str(),
                    repo.now,
                    expires
                ],
            )?;
            self.audit(repo, local_actor()?, "management.pair.issue", role.id())?;
            Ok(PairCode {
                secret,
                expires_at_ms: expires,
            })
        })
    }
    pub fn enroll(&self, code: &str, device_name: &str, public_key: &str) -> Result<Enrolled> {
        self.enroll_for(code, device_name, public_key, None)
    }
    pub fn enroll_for(
        &self,
        code: &str,
        device_name: &str,
        public_key: &str,
        token: Option<&str>,
    ) -> Result<Enrolled> {
        let authenticated = token.map(|t| self.authenticate(t)).transpose()?;
        let invalid = ManagementError::PairInvalid;
        decode::<32>(code).map_err(|_| invalid)?;
        let key = decode::<32>(public_key).map_err(|_| ManagementError::InvalidRequest)?;
        let name = DisplayName::parse(device_name).map_err(|_| ManagementError::InvalidRequest)?;
        let label = DeviceLabel::parse(device_name).map_err(|_| ManagementError::InvalidRequest)?;
        let fingerprint = Fingerprint::parse(&hex::encode(digest::digest(&digest::SHA256, &key)))?;
        self.write(|repo| {
            let c = repo.connection();
            let hash = bound_hash(b"pair", self.server_id(), code);
            let role: Option<String> = c.query_row("SELECT role_id FROM management_pairing_codes WHERE code_hash = ? AND server_id = ? AND expires_at > ?", params![hash.as_slice(), self.server_id(), repo.now], |r| r.get(0)).optional()?;
            let role = RoleId::parse(&role.ok_or(invalid)?)?;
            // BEGIN IMMEDIATE owns redemption. Removal, policy, enrollment and
            // audit commit together; failed transactions leave the code usable.
            c.execute("DELETE FROM management_pairing_codes WHERE code_hash = ?", [hash.as_slice()])?;
            repo.role(&role)?;
            if let Some(subject)=&authenticated {self.subject_in(repo,&subject.device)?;}
            let existing:Option<(String,String)>=c.query_row("SELECT principal_id,id FROM devices WHERE server_id=? AND public_key_sha256=? AND revoked_at IS NULL",params![self.server_id(),fingerprint.as_str()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let principal=if let Some(subject)=&authenticated {subject.principal.clone()} else {if existing.is_some() {return Err(ManagementError::SessionInvalid);}self.create_user(repo,&name)?};
            let device=if let Some((owner,id))=existing {if owner!=principal.as_str(){return Err(ManagementError::SessionInvalid);}DeviceId::parse(&id)?} else {
                self.check_device_capacity(repo,true)?;
                let device=repo.insert_device(&principal,&label,&fingerprint)?;
                c.execute("INSERT INTO management_device_keys (device_id, server_id, public_key) VALUES (?,?,?)", params![device.as_str(), self.server_id(), key.as_slice()])?;device
            };
            repo.replace_role(&principal, &role, None, "local-owner-pair")?;
            let subject = Subject { principal: principal.clone(), device: device.clone() };
            if !repo.policy(Some(&subject))?.has_current_identity() { return Err(ManagementError::Unavailable); }
            self.audit(repo, device_actor(&subject)?, "management.pair.enroll", device.as_str())?;
            Ok(Enrolled { server_id: self.server_id().to_owned(), principal_id: principal.to_string(), device_id: device.to_string(), role: role.to_string() })
        })
    }

    /// A fresh enrollment-purpose proof identifies the device owner. It grants
    /// a role ONLY when current server-local policy has an explicit owner link.
    pub fn enroll_minecraft(
        &self,
        profile: super::minecraft::VerifiedMinecraftProfile,
        device_name: &str,
        public_key: &str,
    ) -> Result<Enrolled> {
        self.enroll_minecraft_for(profile, device_name, public_key, None)
    }
    pub fn link_local(
        &self,
        uuid: super::minecraft::MinecraftUuid,
        role: BuiltinRole,
    ) -> Result<()> {
        let uuid = uuid.as_uuid().to_string();
        self.write(|repo| {
            let c = repo.connection();
            let exists: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM management_owner_links WHERE server_id = ? AND minecraft_uuid = ?)", params![self.server_id(), uuid], |r| r.get(0))?;
            let count: u32 = c.query_row("SELECT count(*) FROM management_owner_links", [], |r| r.get(0))?;
            if !exists && count >= 32 { return Err(ManagementError::Saturated); }
            repo.role(&role.into())?;
            c.execute("INSERT INTO management_owner_links VALUES (?,?,?,?) ON CONFLICT(server_id,minecraft_uuid) DO UPDATE SET role_id = excluded.role_id, created_at = excluded.created_at", params![self.server_id(), uuid, role.id(), repo.now])?;
            let principal:Option<String>=c.query_row("SELECT principal_id FROM user_minecraft_accounts WHERE server_id=? AND minecraft_uuid=?",params![self.server_id(),uuid],|r|r.get(0)).optional()?;
            if let Some(id)=principal {repo.replace_role(&PrincipalId::parse(&id)?,&role.into(),None,"local-owner-minecraft-link")?;}
            self.audit(repo, local_actor()?, "management.owner.link", &uuid)
        })
    }
    /// Existing devices remain by default. Explicit revocation also applies to
    /// descendants of older links for this UUID, even after a previous unlink.
    pub fn unlink_local(
        &self,
        uuid: super::minecraft::MinecraftUuid,
        revoke_devices: bool,
    ) -> Result<()> {
        let uuid = uuid.as_uuid().to_string();
        self.write(|repo| {
            let c = repo.connection();
            c.execute("DELETE FROM management_owner_links WHERE server_id = ? AND minecraft_uuid = ?", params![self.server_id(), uuid])?;
            if revoke_devices {
                c.execute("UPDATE devices SET revoked_at = ?, revoked_by = 'local-os-owner' WHERE server_id = ? AND revoked_at IS NULL AND principal_id IN (SELECT principal_id FROM user_minecraft_accounts WHERE server_id = ? AND minecraft_uuid = ?)", params![repo.now, self.server_id(), self.server_id(), uuid])?;
                c.execute("UPDATE role_assignments SET revoked_at=?,revoked_by='local-os-owner' WHERE granted_by='local-owner-minecraft-link' AND revoked_at IS NULL AND principal_id IN (SELECT principal_id FROM user_minecraft_accounts WHERE server_id=? AND minecraft_uuid=?)",params![repo.now,self.server_id(),uuid])?;
            }
            self.audit(repo, local_actor()?, if revoke_devices { "management.owner.unlink_revoke" } else { "management.owner.unlink" }, &uuid)
        })
    }
    pub fn links_local(&self) -> Result<Vec<serde_json::Value>> {
        self.write(|repo| {
            let mut s = repo.connection().prepare("SELECT minecraft_uuid, role_id FROM management_owner_links WHERE server_id = ? ORDER BY minecraft_uuid LIMIT 33")?;
            let rows = s.query_map([self.server_id()], |r| Ok(serde_json::json!({"minecraft_uuid": r.get::<_, String>(0)?, "role": r.get::<_, String>(1)?})))?;
            let links = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            if links.len() > 32 { return Err(ManagementError::Saturated); }
            Ok(links)
        })
    }
    /// Future handlers pass their exact capability and resource here and also
    /// reload policy in their mutation transaction; Subjects alone are not grants.
    pub fn authorize(
        &self,
        token: &str,
        capability: super::Capability,
        resource: Option<&super::Resource>,
    ) -> Result<Subject> {
        let subject = self.authenticate(token)?;
        self.write(|repo| {
            let policy = repo.policy(Some(&subject))?;
            if !policy.has_current_identity() {
                return Err(ManagementError::SessionInvalid);
            }
            if !policy.check(capability, resource).allowed() {
                return Err(ManagementError::Forbidden);
            }
            Ok(subject)
        })
    }
    fn subject_in(&self, repo: &Repository<'_>, device: &DeviceId) -> Result<(Subject, Vec<u8>)> {
        let row: Option<(String, Vec<u8>, String)> = repo.connection().query_row(
            "SELECT d.principal_id, k.public_key, d.public_key_sha256 FROM devices d JOIN management_device_keys k ON k.device_id = d.id WHERE d.id = ? AND d.server_id = ? AND k.server_id = ? AND d.revoked_at IS NULL",
            params![device.as_str(), self.server_id(), self.server_id()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
        let (principal, key, fingerprint) = row.ok_or(ManagementError::SessionInvalid)?;
        if key.len() != 32
            || hex::encode(digest::digest(&digest::SHA256, &key))
                != fingerprint.to_ascii_lowercase()
        {
            return Err(ManagementError::SessionInvalid);
        }
        let subject = Subject {
            principal: PrincipalId::parse(&principal)?,
            device: device.clone(),
        };
        let policy = repo.policy(Some(&subject))?;
        if !policy.has_current_identity() {
            return Err(ManagementError::SessionInvalid);
        }
        Ok((subject, key))
    }
    pub fn challenge(&self, device: &str) -> Result<Secret> {
        let device = DeviceId::parse(device).map_err(|_| ManagementError::SessionInvalid)?;
        let mut stores = self
            .stores
            .lock()
            .map_err(|_| ManagementError::Unavailable)?;
        stores.prune(self.now());
        if stores.challenges.len() >= MAX_CHALLENGES {
            return Err(ManagementError::Saturated);
        }
        self.write(|repo| self.subject_in(repo, &device).map(|_| ()))?;
        let nonce = random_secret()?;
        let expires = deadline(self.now(), CHALLENGE_TTL_MS)?;
        stores.challenges.insert(
            bound_hash(b"challenge", self.server_id(), nonce.expose()),
            Challenge {
                device,
                expires,
                monotonic_expires: std::time::Instant::now()
                    + std::time::Duration::from_millis(CHALLENGE_TTL_MS as u64),
            },
        );
        Ok(nonce)
    }
    pub fn session(&self, device: &str, nonce: &str, signature_text: &str) -> Result<SessionToken> {
        let invalid = ManagementError::SessionInvalid;
        let device = DeviceId::parse(device).map_err(|_| invalid)?;
        decode::<32>(nonce).map_err(|_| invalid)?;
        let mut stores = self
            .stores
            .lock()
            .map_err(|_| ManagementError::Unavailable)?;
        stores.prune(self.now());
        // Consume even failed signature/device attempts, including audit/capacity
        // failures. No challenge is ever restored after an attempted completion.
        let challenge = stores
            .challenges
            .remove(&bound_hash(b"challenge", self.server_id(), nonce))
            .ok_or(invalid)?;
        if challenge.device != device {
            return Err(invalid);
        }
        let signature_bytes = decode::<64>(signature_text).map_err(|_| invalid)?;
        if stores.sessions.len() >= self.session_limit {
            return Err(ManagementError::Saturated);
        }
        let secret = random_secret()?;
        let (subject, expires) = self.write(|repo| {
            if challenge.expires <= repo.now {
                return Err(invalid);
            }
            let (subject, key) = self.subject_in(repo, &device)?;
            signature::UnparsedPublicKey::new(&signature::ED25519, key)
                .verify(&session_message(self.server_id(), nonce), &signature_bytes)
                .map_err(|_| invalid)?;
            let expires = deadline(repo.now, self.session_ttl_ms)?;
            self.audit(
                repo,
                device_actor(&subject)?,
                "management.session.issue",
                device.as_str(),
            )?;
            Ok((subject, expires))
        })?;
        stores.sessions.insert(
            bound_hash(b"session", self.server_id(), secret.expose()),
            Session {
                subject,
                server: self.server_id().to_owned(),
                expires,
                monotonic_expires: std::time::Instant::now()
                    + std::time::Duration::from_millis(self.session_ttl_ms as u64),
            },
        );
        Ok(SessionToken {
            secret,
            expires_at_ms: expires,
        })
    }
    /// Authenticated lookup, not reusable policy authority. Future mutations must
    /// reload/authorize current policy inside their own write transaction.
    pub fn authenticate(&self, token: &str) -> Result<Subject> {
        decode::<32>(token).map_err(|_| ManagementError::SessionInvalid)?;
        let mut stores = self
            .stores
            .lock()
            .map_err(|_| ManagementError::Unavailable)?;
        stores.prune(self.now());
        let hash = bound_hash(b"session", self.server_id(), token);
        let session = stores
            .sessions
            .get(&hash)
            .ok_or(ManagementError::SessionInvalid)?;
        if session.server != self.server_id() {
            return Err(ManagementError::SessionInvalid);
        }
        let subject = session.subject.clone();
        let expires = session.expires;
        if std::time::Instant::now() >= session.monotonic_expires {
            stores.sessions.remove(&hash);
            return Err(ManagementError::SessionInvalid);
        }
        let result = self.write(|repo| {
            if expires <= repo.now {
                return Err(ManagementError::SessionInvalid);
            }
            let (current, _) = self.subject_in(repo, &subject.device)?;
            if current != subject {
                return Err(ManagementError::SessionInvalid);
            }
            Ok(current)
        });
        if result.is_err() {
            stores.sessions.remove(&hash);
        }
        result
    }
    pub fn list_local(&self) -> Result<Vec<DeviceInfo>> {
        self.write(|repo| {
            let mut s = repo.connection().prepare("SELECT id, principal_id, label, revoked_at IS NOT NULL FROM devices WHERE server_id = ? ORDER BY created_at, id LIMIT 1001")?;
            let rows = s.query_map([self.server_id()], |r| Ok(DeviceInfo { device_id: r.get(0)?, principal_id: r.get(1)?, device_name: r.get(2)?, revoked: r.get(3)? }))?;
            let result = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            if result.len() > 1000 { return Err(ManagementError::Saturated); }
            Ok(result)
        })
    }
    pub fn revoke_local(&self, device: &str) -> Result<()> {
        let device = DeviceId::parse(device).map_err(|_| ManagementError::InvalidRequest)?;
        self.write(|repo| {
            repo.device_principal(&device)?;
            repo.revoke_device(&device, "local-os-owner")?;
            self.audit(
                repo,
                local_actor()?,
                "management.device.revoke",
                device.as_str(),
            )
        })
    }
}
/// Exact signed bytes: prefix NUL server-id NUL canonical base64url nonce. Fixed
/// prefix is both domain separation and purpose; no caller-selected purpose.
pub fn session_message(server_id: &str, nonce: &str) -> Vec<u8> {
    format!("{SESSION_PURPOSE}\0{server_id}\0{nonce}").into_bytes()
}
fn deadline(now: i64, ttl: i64) -> Result<i64> {
    now.checked_add(ttl).ok_or(ManagementError::Unavailable)
}
fn random_secret() -> Result<Secret> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| ManagementError::Unavailable)?;
    Ok(Secret(Zeroizing::new(URL_SAFE_NO_PAD.encode(bytes))))
}
fn decode<const N: usize>(text: &str) -> Result<[u8; N]> {
    if text.len() != (N * 8).div_ceil(6) {
        return Err(ManagementError::InvalidRequest);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| ManagementError::InvalidRequest)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != text {
        return Err(ManagementError::InvalidRequest);
    }
    bytes
        .try_into()
        .map_err(|_| ManagementError::InvalidRequest)
}
fn bound_hash(purpose: &[u8], server: &str, secret: &str) -> Hash {
    let mut context = digest::Context::new(&digest::SHA256);
    context.update(b"vesta.management.credential.v1\0");
    context.update(purpose);
    context.update(b"\0");
    context.update(server.as_bytes());
    context.update(b"\0");
    context.update(secret.as_bytes());
    context.finish().as_ref().try_into().expect("SHA256 length")
}
fn local_actor() -> Result<AuditActor> {
    Ok(AuditActor::new(
        "local-os-owner".to_owned(),
        "local".to_owned(),
        "protected-local-control".to_owned(),
    )?)
}
fn device_actor(subject: &Subject) -> Result<AuditActor> {
    Ok(AuditActor::new(
        subject.principal.to_string(),
        subject.device.to_string(),
        "management-device".to_owned(),
    )?)
}

#[cfg(test)]
mod tests;

mod access;
pub use access::{AccessSettings, DeviceSessionReference};
