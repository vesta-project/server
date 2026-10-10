//! Pack-only access: grants from Minecraft proofs or pack passwords, and the short-lived
//! sessions and single-use tickets that carry them.
//!
//! This domain is separate from management. A pack grant never becomes a `Subject`,
//! role or capability, and it only describes access to hosted pack resources. Secrets
//! are random, returned once and retained only as a server-bound SHA-256. Every use
//! rechecks the grant against the current policy, so a ban, whitelist removal, mode
//! change or password rotation ends access without waiting for expiry. Sessions live
//! in memory and end with the process.
use crate::config::PackAccessMode;
use crate::identity::minecraft::{MinecraftUuid, ServerUuid, VerifiedMinecraftProfile};
use crate::identity::{PasswordRevision, PasswordVerification, PasswordVerificationStatus};
use crate::minecraft_admission::{BanList, ServerProperties};
use crate::whitelist::Whitelist;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt,
    sync::Mutex,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

pub const MAX_SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub const MAX_TICKET_TTL: Duration = Duration::from_secs(60);
pub const MAX_SESSIONS: usize = 16_384;
pub const MAX_TICKETS: usize = 4_096;
const SECRET_BYTES: usize = 32;
const SECRET_TEXT_LEN: usize = 43;

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
    Minecraft(MinecraftUuid),
    Password(PasswordRevision),
}
impl fmt::Debug for PackGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
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
        (profile.server_uuid() == self.server && self.admits(profile.uuid()))
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
    grant: PackGrant,
    expires_at: Instant,
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
            session_ttl: Duration::from_secs(60 * 60),
            ticket_ttl: Duration::from_secs(30),
            sessions: 4_096,
            tickets: 1_024,
        }
    }
}

type Key = [u8; 32];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TicketPurpose {
    Download,
    Realtime,
}

/// Opaque server-local session reference. Not a client bearer credential or authority.
#[derive(Clone, Copy, Eq, PartialEq, Hash)]
pub struct RealtimeSession {
    key: Key,
    server: ServerUuid,
}
impl fmt::Debug for RealtimeSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RealtimeSession[<redacted>]")
    }
}
struct Ticket {
    session: Key,
    expires_at: Instant,
    purpose: TicketPurpose,
}
#[derive(Default)]
struct State {
    sessions: HashMap<Key, PackSession>,
    tickets: HashMap<Key, Ticket>,
}

/// In-memory store for one canonical server. Callers pass a monotonic `now`.
pub struct PackSessions {
    server: ServerUuid,
    limits: SessionLimits,
    state: Mutex<State>,
}
impl fmt::Debug for PackSessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PackSessions")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}
impl PackSessions {
    pub fn new(server: ServerUuid, limits: SessionLimits) -> Self {
        Self {
            server,
            limits,
            state: Mutex::new(State::default()),
        }
    }

    /// The grant must come from `PackAccessPolicy::grant_*` against the current policy.
    pub fn issue(
        &self,
        grant: PackGrant,
        now: Instant,
    ) -> Result<(PackSecret, PackSession), PackAccessError> {
        let mut state = self.lock()?;
        purge(&mut state, now);
        if state.sessions.len() >= self.limits.sessions {
            return Err(PackAccessError::Saturated);
        }
        let expires_at = now
            .checked_add(self.limits.session_ttl)
            .ok_or(PackAccessError::Unavailable)?;
        let (secret, key) = self.mint()?;
        let session = PackSession { grant, expires_at };
        if state.sessions.insert(key, session).is_some() {
            return Err(PackAccessError::Unavailable);
        }
        Ok((secret, session))
    }

    /// A session whose grant the current policy no longer allows is removed.
    pub fn authenticate(
        &self,
        secret: &str,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<PackSession, PackAccessError> {
        let key = self.key(secret)?;
        let mut state = self.lock()?;
        live_session(&mut state, &key, now, policy)
    }

    /// Like `authenticate`, plus the opaque reference used for per-session admission
    /// (for example, concurrent download caps). The reference is not a credential.
    pub fn authenticate_reference(
        &self,
        secret: &str,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<(RealtimeSession, PackSession), PackAccessError> {
        let key = self.key(secret)?;
        let mut state = self.lock()?;
        let session = live_session(&mut state, &key, now, policy)?;
        Ok((
            RealtimeSession {
                key,
                server: self.server,
            },
            session,
        ))
    }

    pub fn revoke(&self, secret: &str) -> Result<bool, PackAccessError> {
        let Ok(key) = self.key(secret) else {
            return Ok(false);
        };
        let mut state = self.lock()?;
        state.tickets.retain(|_, ticket| ticket.session != key);
        Ok(state.sessions.remove(&key).is_some())
    }

    /// Compatibility helper: issue a download ticket, never a realtime ticket.
    pub fn issue_ticket(
        &self,
        secret: &str,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<PackSecret, PackAccessError> {
        self.issue_ticket_for(secret, TicketPurpose::Download, now, policy)
    }

    /// Purpose-bound single-use ticket. It never outlives its session.
    pub fn issue_ticket_for(
        &self,
        secret: &str,
        purpose: TicketPurpose,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<PackSecret, PackAccessError> {
        let session_key = self.key(secret)?;
        let mut state = self.lock()?;
        let session = live_session(&mut state, &session_key, now, policy)?;
        purge(&mut state, now);
        if state.tickets.len() >= self.limits.tickets {
            return Err(PackAccessError::Saturated);
        }
        let expires_at = now
            .checked_add(self.limits.ticket_ttl)
            .ok_or(PackAccessError::Unavailable)?
            .min(session.expires_at);
        let (ticket, key) = self.mint()?;
        state.tickets.insert(
            key,
            Ticket {
                session: session_key,
                expires_at,
                purpose,
            },
        );
        Ok(ticket)
    }

    /// Consumes the ticket whether or not redemption succeeds.
    pub fn redeem_ticket(
        &self,
        ticket: &str,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<PackSession, PackAccessError> {
        self.redeem_for(ticket, TicketPurpose::Download, now, policy)
            .map(|(_, session)| session)
    }

    /// `redeem_ticket` returning the owning session's reference, so a download
    /// ticket and its session bearer share one per-session admission slot.
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
        ticket: &str,
        purpose: TicketPurpose,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<(RealtimeSession, PackSession), PackAccessError> {
        let key = self.key(ticket)?;
        let mut state = self.lock()?;
        let ticket = state.tickets.remove(&key).ok_or(PackAccessError::Invalid)?;
        if now >= ticket.expires_at || ticket.purpose != purpose {
            return Err(PackAccessError::Invalid);
        }
        let session = live_session(&mut state, &ticket.session, now, policy)?;
        Ok((
            RealtimeSession {
                key: ticket.session,
                server: self.server,
            },
            session,
        ))
    }

    /// Every realtime push must consult the live store and current policy, not a
    /// cached grant: revocation must also terminate already connected sockets.
    pub fn recheck_realtime(
        &self,
        reference: RealtimeSession,
        now: Instant,
        policy: &PackAccessPolicy<'_>,
    ) -> Result<PackSession, PackAccessError> {
        if reference.server != self.server {
            return Err(PackAccessError::Invalid);
        }
        let mut state = self.lock()?;
        live_session(&mut state, &reference.key, now, policy)
    }

    pub fn len(&self) -> Result<(usize, usize), PackAccessError> {
        let state = self.lock()?;
        Ok((state.sessions.len(), state.tickets.len()))
    }

    fn mint(&self) -> Result<(PackSecret, Key), PackAccessError> {
        let mut random = Zeroizing::new([0_u8; SECRET_BYTES]);
        getrandom::fill(random.as_mut()).map_err(|_| PackAccessError::Unavailable)?;
        let secret = PackSecret(Zeroizing::new(URL_SAFE_NO_PAD.encode(random.as_ref())));
        let key = self.key(secret.expose())?;
        Ok((secret, key))
    }

    /// Strict spelling first, so arbitrary input is never hashed into a lookup.
    fn key(&self, secret: &str) -> Result<Key, PackAccessError> {
        if secret.len() != SECRET_TEXT_LEN
            || !secret
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(PackAccessError::Invalid);
        }
        let mut hasher = Sha256::new();
        hasher.update(b"vesta-pack-session\0");
        hasher.update(self.server.as_uuid().as_bytes());
        hasher.update(secret.as_bytes());
        Ok(hasher.finalize().into())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, PackAccessError> {
        self.state.lock().map_err(|_| PackAccessError::Unavailable)
    }
}

fn live_session(
    state: &mut State,
    key: &Key,
    now: Instant,
    policy: &PackAccessPolicy<'_>,
) -> Result<PackSession, PackAccessError> {
    let session = *state.sessions.get(key).ok_or(PackAccessError::Invalid)?;
    if now >= session.expires_at || !policy.allows(session.grant) {
        state.sessions.remove(key);
        state.tickets.retain(|_, ticket| &ticket.session != key);
        return Err(PackAccessError::Invalid);
    }
    Ok(session)
}

fn purge(state: &mut State, now: Instant) {
    state.sessions.retain(|_, session| now < session.expires_at);
    let sessions = &state.sessions;
    state
        .tickets
        .retain(|_, ticket| now < ticket.expires_at && sessions.contains_key(&ticket.session));
}

#[cfg(test)]
#[path = "pack_access_tests.rs"]
mod tests;
