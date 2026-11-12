//! Purpose-bound action tickets over the sole canonical user/device session store.
//! Authentication identifies a device; every download/realtime use resolves live
//! public/pack capabilities and safe automatic role sources. Passwords and UUID
//! proofs cannot grant management. No independent pack session authority is cached.
use crate::config::PackAccessMode;
use crate::identity::minecraft::{MinecraftUuid, ServerUuid, VerifiedMinecraftProfile};
use crate::identity::{PasswordRevision, PasswordVerification, PasswordVerificationStatus};
use crate::minecraft_admission::{BanList, ServerProperties};
use crate::whitelist::Whitelist;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::sync::Arc;
use std::{
    collections::HashMap,
    fmt,
    sync::Mutex,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

pub const MAX_SESSION_TTL: Duration = Duration::from_secs(15 * 60);
pub const MAX_TICKET_TTL: Duration = Duration::from_secs(60);
pub const MAX_SESSIONS: usize = 4_096;
pub const MAX_TICKETS: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackAccessError {
    InvalidLimits,
    /// Unknown, malformed, expired, used or no-longer-permitted secrets are indistinguishable.
    Invalid,
    Saturated,
    Unavailable,
}
impl PackAccessError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "pack_session_limits_invalid",
            Self::Invalid => "pack_session_invalid",
            Self::Saturated => "pack_session_saturated",
            Self::Unavailable => "pack_session_unavailable",
        }
    }
}
impl fmt::Display for PackAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for PackAccessError {}

/// What a pack client may access, and why. Never management authority.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum PackGrant {
    Role,
    Minecraft(MinecraftUuid),
    Password(PasswordRevision),
}
impl fmt::Debug for PackGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Role => "PackGrant::Role",
            Self::Minecraft(_) => "PackGrant::Minecraft[<redacted>]",
            Self::Password(_) => "PackGrant::Password",
        })
    }
}

/// The current pack-access policy, assembled by trusted host code from configuration,
/// the server's own admission files and the persisted password state.
pub struct PackAccessPolicy<'a> {
    pub server: ServerUuid,
    pub mode: PackAccessMode,
    /// `None` when `server.properties` is unavailable: Minecraft policy then fails closed.
    pub properties: Option<ServerProperties>,
    pub whitelist: &'a Whitelist,
    pub bans: &'a BanList,
    /// The active password revision, or `None` when no password is set or it is revoked.
    pub password: Option<PasswordRevision>,
}
impl PackAccessPolicy<'_> {
    /// Minecraft admission uses the server's real data. Offline-mode servers cannot
    /// prove a joining player's identity, so Minecraft policy is unavailable there.
    pub fn admits(&self, player: MinecraftUuid) -> bool {
        self.properties.is_some_and(|properties| {
            properties.online_mode()
                && !self.bans.contains(player)
                && (!properties.whitelist_enabled() || self.whitelist.contains(player))
        })
    }

    pub fn grant_minecraft(&self, profile: &VerifiedMinecraftProfile) -> Option<PackGrant> {
        (profile.purpose() == crate::identity::minecraft::MinecraftPurpose::Pack
            && profile.server_uuid() == self.server
            && self.admits(profile.uuid()))
        .then_some(PackGrant::Minecraft(profile.uuid()))
    }

    /// Only a match against the currently active password revision grants access.
    pub fn grant_password(&self, verification: &PasswordVerification) -> Option<PackGrant> {
        (self.mode == PackAccessMode::MinecraftOrPassword
            && verification.status() == PasswordVerificationStatus::Match
            && verification.server_id() == self.server.as_uuid().to_string()
            && self.password == Some(verification.revision()))
        .then_some(PackGrant::Password(verification.revision()))
    }

    pub fn allows(&self, grant: PackGrant) -> bool {
        match grant {
            PackGrant::Role => false,
            PackGrant::Minecraft(player) => self.admits(player),
            PackGrant::Password(revision) => {
                self.mode == PackAccessMode::MinecraftOrPassword && self.password == Some(revision)
            }
        }
    }
}

/// A bearer secret shown to the client once. Debug never prints it.
pub struct PackSecret(Zeroizing<String>);
impl PackSecret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for PackSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PackSecret[<redacted>]")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackSession {
    pub(crate) grant: PackGrant,
    pub(crate) expires_at: Instant,
}
impl PackSession {
    pub const fn grant(&self) -> PackGrant {
        self.grant
    }
    pub const fn expires_at(&self) -> Instant {
        self.expires_at
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionLimits {
    session_ttl: Duration,
    ticket_ttl: Duration,
    sessions: usize,
    tickets: usize,
}
impl SessionLimits {
    pub fn new(
        session_ttl: Duration,
        ticket_ttl: Duration,
        sessions: usize,
        tickets: usize,
    ) -> Result<Self, PackAccessError> {
        if session_ttl.is_zero()
            || session_ttl > MAX_SESSION_TTL
            || ticket_ttl.is_zero()
            || ticket_ttl > MAX_TICKET_TTL
            || sessions == 0
            || sessions > MAX_SESSIONS
            || tickets == 0
            || tickets > MAX_TICKETS
        {
            return Err(PackAccessError::InvalidLimits);
        }
        Ok(Self {
            session_ttl,
            ticket_ttl,
            sessions,
            tickets,
        })
    }
}
impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            session_ttl: Duration::from_secs(15 * 60),
            ticket_ttl: Duration::from_secs(30),
            sessions: 4_096,
            tickets: 1_024,
        }
    }
}

type Key = [u8; 32];
pub type RealtimeSession = crate::identity::management::DeviceSessionReference;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TicketPurpose {
    Download,
    Realtime,
}
struct Ticket {
    session: RealtimeSession,
    expires_at: Instant,
    purpose: TicketPurpose,
}
#[derive(Default)]
struct State {
    tickets: HashMap<Key, Ticket>,
}
/// Ticket facade over the sole canonical device session store. No pack sessions
/// or cached pack authority are retained here; every use resolves live user roles.
pub struct PackSessions {
    management: crate::identity::management::ManagementAccess,
    server: ServerUuid,
    limits: SessionLimits,
    state: Mutex<State>,
    #[cfg(test)]
    fixture: Option<Arc<tempfile::TempDir>>,
}
impl fmt::Debug for PackSessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceTickets")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}
impl PackSessions {
    pub fn new(
        management: crate::identity::management::ManagementAccess,
        limits: SessionLimits,
    ) -> Self {
        let server = ServerUuid::parse(management.server_id()).expect("canonical server UUID");
        Self {
            management,
            server,
            limits,
            state: Mutex::new(State::default()),
            #[cfg(test)]
            fixture: None,
        }
    }
    pub fn authenticate(
        &self,
        secret: &str,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<PackSession, PackAccessError> {
        self.authenticate_reference(secret, now, policy)
            .map(|(_, s)| s)
    }
    pub fn authenticate_reference(
        &self,
        secret: &str,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<(RealtimeSession, PackSession), PackAccessError> {
        let reference = self
            .management
            .pack_reference(if secret.is_empty() {
                None
            } else {
                Some(secret)
            })
            .map_err(pack_error)?;
        let session = self
            .management
            .authorize_pack(
                reference,
                crate::identity::Capability::PackDownload,
                now,
                policy,
            )
            .map_err(pack_error)?;
        Ok((reference, session))
    }
    pub fn authorize_info(
        &self,
        token: Option<&str>,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<(), PackAccessError> {
        let reference = self.management.pack_reference(token).map_err(pack_error)?;
        self.management
            .authorize_pack(
                reference,
                crate::identity::Capability::PublicInfo,
                now,
                policy,
            )
            .map(|_| ())
            .map_err(pack_error)
    }
    pub fn revoke(&self, secret: &str) -> Result<bool, PackAccessError> {
        let reference = self.management.pack_reference(Some(secret));
        if let Ok(reference) = reference {
            self.lock()?.tickets.retain(|_, t| t.session != reference);
        }
        self.management
            .revoke_session(secret)
            .or_else(|e| {
                if e == crate::identity::management::ManagementError::SessionInvalid {
                    Ok(false)
                } else {
                    Err(e)
                }
            })
            .map_err(pack_error)
    }
    pub fn issue_ticket(
        &self,
        secret: &str,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<PackSecret, PackAccessError> {
        self.issue_ticket_for(secret, TicketPurpose::Download, now, policy)
    }
    pub fn issue_ticket_for(
        &self,
        secret: &str,
        purpose: TicketPurpose,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<PackSecret, PackAccessError> {
        let reference = self
            .management
            .pack_reference(if secret.is_empty() {
                None
            } else {
                Some(secret)
            })
            .map_err(pack_error)?;
        let capability = match purpose {
            TicketPurpose::Download => crate::identity::Capability::PackDownload,
            TicketPurpose::Realtime => crate::identity::Capability::PackRealtime,
        };
        let session = self
            .management
            .authorize_pack(reference, capability, now, policy)
            .map_err(pack_error)?;
        let mut state = self.lock()?;
        state.tickets.retain(|_, t| now < t.expires_at);
        if state.tickets.len() >= self.limits.tickets {
            return Err(PackAccessError::Saturated);
        }
        let expires_at = (now + self.limits.ticket_ttl).min(session.expires_at());
        let (secret, key) = self.mint()?;
        state.tickets.insert(
            key,
            Ticket {
                session: reference,
                expires_at,
                purpose,
            },
        );
        Ok(secret)
    }
    pub fn redeem_ticket(
        &self,
        ticket: &str,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<PackSession, PackAccessError> {
        self.redeem_for(ticket, TicketPurpose::Download, now, policy)
            .map(|(_, s)| s)
    }
    pub fn redeem_download(
        &self,
        ticket: &str,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<(RealtimeSession, PackSession), PackAccessError> {
        self.redeem_for(ticket, TicketPurpose::Download, now, policy)
    }
    pub fn redeem_realtime(
        &self,
        ticket: &str,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<(RealtimeSession, PackSession), PackAccessError> {
        self.redeem_for(ticket, TicketPurpose::Realtime, now, policy)
    }
    fn redeem_for(
        &self,
        secret: &str,
        purpose: TicketPurpose,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<(RealtimeSession, PackSession), PackAccessError> {
        let key = self.key(secret)?;
        let ticket = self
            .lock()?
            .tickets
            .remove(&key)
            .ok_or(PackAccessError::Invalid)?;
        if now >= ticket.expires_at || ticket.purpose != purpose {
            return Err(PackAccessError::Invalid);
        }
        let capability = match purpose {
            TicketPurpose::Download => crate::identity::Capability::PackDownload,
            TicketPurpose::Realtime => crate::identity::Capability::PackRealtime,
        };
        let session = self
            .management
            .authorize_pack(ticket.session, capability, now, policy)
            .map_err(pack_error)?;
        Ok((ticket.session, session))
    }
    pub fn recheck_realtime(
        &self,
        reference: RealtimeSession,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<PackSession, PackAccessError> {
        self.management
            .authorize_pack(
                reference,
                crate::identity::Capability::PackRealtime,
                now,
                policy,
            )
            .map_err(pack_error)
    }
    pub fn len(&self) -> Result<(usize, usize), PackAccessError> {
        Ok((
            self.management
                .session_count(Instant::now())
                .map_err(pack_error)?,
            self.lock()?.tickets.len(),
        ))
    }
    fn mint(&self) -> Result<(PackSecret, Key), PackAccessError> {
        use ring::rand::SecureRandom as _;
        let mut bytes = Zeroizing::new([0; 32]);
        ring::rand::SystemRandom::new()
            .fill(bytes.as_mut())
            .map_err(|_| PackAccessError::Unavailable)?;
        let secret = PackSecret(Zeroizing::new(URL_SAFE_NO_PAD.encode(bytes.as_ref())));
        let key = self.key(secret.expose())?;
        Ok((secret, key))
    }
    fn key(&self, secret: &str) -> Result<Key, PackAccessError> {
        if secret.len() != 43 {
            return Err(PackAccessError::Invalid);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(secret)
            .map_err(|_| PackAccessError::Invalid)?;
        if bytes.len() != 32 || URL_SAFE_NO_PAD.encode(bytes) != secret {
            return Err(PackAccessError::Invalid);
        }
        let mut hash = Sha256::new();
        hash.update(b"vesta-device-action-ticket-v1\0");
        hash.update(self.server.as_uuid().as_bytes());
        hash.update(secret.as_bytes());
        Ok(hash.finalize().into())
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, PackAccessError> {
        self.state.lock().map_err(|_| PackAccessError::Unavailable)
    }
}
fn pack_error(error: crate::identity::management::ManagementError) -> PackAccessError {
    use crate::identity::management::ManagementError;
    match error {
        ManagementError::Forbidden
        | ManagementError::SessionInvalid
        | ManagementError::PairInvalid
        | ManagementError::InvalidRequest => PackAccessError::Invalid,
        ManagementError::Saturated => PackAccessError::Saturated,
        ManagementError::Unavailable => PackAccessError::Unavailable,
    }
}
#[cfg(test)]
#[path = "pack_access_tests.rs"]
mod tests;

#[cfg(test)]
impl PackSessions {
    fn fixture(server: ServerUuid, limits: SessionLimits) -> Self {
        let temp = Arc::new(tempfile::tempdir().unwrap());
        let directory = vesta_state::StateDirectory::new(temp.path().join("state"), None).unwrap();
        let db =
            vesta_state::StateDatabase::open(directory.clone(), Arc::new(vesta_state::SystemClock))
                .unwrap();
        db.write(|c| {
            c.execute(
                "UPDATE server_identity SET server_id=?",
                [server.as_uuid().to_string()],
            )?;
            Ok(())
        })
        .unwrap();
        db.close().unwrap();
        let db = vesta_state::StateDatabase::open(directory, Arc::new(vesta_state::SystemClock))
            .unwrap();
        let management = crate::identity::management::ManagementAccess::with_limits(
            db,
            limits.sessions,
            limits.session_ttl.as_millis() as i64,
        )
        .unwrap();
        let mut sessions = Self::new(management, limits);
        sessions.fixture = Some(temp);
        sessions
    }
    pub(crate) fn issue(
        &self,
        grant: PackGrant,
        now: Instant,
    ) -> Result<(PackSecret, PackSession), PackAccessError> {
        self.lock()?.tickets.retain(|_, t| now < t.expires_at);
        let (secret, session) = self
            .management
            .test_pack_session(grant, now)
            .map_err(pack_error)?;
        Ok((PackSecret(Zeroizing::new(secret.expose().into())), session))
    }
}
