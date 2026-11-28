//! Transport-only PAKE admission and capability state. H04 owns durable enrollment.
//! Put this state behind one mutex; never hold it across account-provider I/O.
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Weak},
    time::{Duration, Instant, SystemTime},
};
use vesta_host_client::{
    rand_core::{CryptoRng, OsRng, RngCore},
    spake2::{Ed25519Group, Password, Spake2},
    *,
};
use zeroize::Zeroizing;
/// One accepted TLS connection. Drop every strong reference on disconnect.
/// Request extensions carry a clone; stored exchanges hold only Weak references.
pub struct Connection {
    pub(crate) identity_id: uuid::Uuid,
    exporter: Zeroizing<[u8; 32]>,
    pub fingerprint: String,
}
impl Connection {
    pub(crate) fn new(exporter: [u8; 32], fingerprint: String) -> Arc<Self> {
        Arc::new(Self {
            identity_id: uuid::Uuid::new_v4(),
            exporter: Zeroizing::new(exporter),
            fingerprint,
        })
    }
    pub(crate) fn from_tls(
        connection: &rustls::ServerConnection,
        fingerprint: String,
    ) -> Result<Option<Arc<Self>>> {
        if connection.protocol_version() != Some(rustls::ProtocolVersion::TLSv1_3)
            || connection.handshake_kind() != Some(rustls::HandshakeKind::Full)
        {
            return Ok(None);
        }
        let exporter = connection
            .export_keying_material([0; 32], EXPORTER_LABEL, Some(b""))
            .map_err(|_| PairingFailed)?;
        Ok(Some(Self::new(exporter, fingerprint)))
    }
    pub fn exporter(&self) -> &[u8; 32] {
        &self.exporter
    }
}
/// H04 must atomically test unclaimed and install the verified owner/device in
/// consume_claim. A successful return commits enrollment; failure consumes nothing.
pub trait ClaimAuthority {
    fn is_unclaimed(&self) -> bool;
    fn consume_claim(&self, capability: &Capability) -> Result<()>;
}
#[derive(Clone)]
pub struct Capability {
    pub exchange_id: String,
    pub device_key: String,
    pub purpose: Purpose,
    pub link_id: Option<String>,
    pub code_epoch: u64,
    pub valid_until: Instant,
}
struct Code {
    password: SetupCode,
    deadline: Instant,
    epoch: u64,
    link_id: Option<String>,
}
struct Exchange {
    confirmation: Confirmation,
    connection: Weak<Connection>,
    deadline: Instant,
    capability: Capability,
    peer: IpAddr,
}
struct Token {
    connection: Weak<Connection>,
    deadline: Instant,
    capability: Capability,
}
pub struct HostPake {
    host_id: String,
    epoch: u64,
    claim: Option<Code>,
    link: Option<Code>,
    exchanges: HashMap<String, Exchange>,
    tokens: HashMap<[u8; 32], Token>,
    completed_links: HashMap<[u8; 32], Token>,
    completed_claims: HashMap<[u8; 32], Token>,
    buckets: HashMap<IpAddr, u8>,
    window: Instant,
    starts: u8,
    failures: u8,
    backoff: Instant,
}
fn token_hash(token: &str) -> Result<[u8; 32]> {
    use sha2::Digest;
    Ok(sha2::Sha256::digest(decode::<32>(token)?).into())
}
fn random<const N: usize>(rng: &mut (impl CryptoRng + RngCore)) -> [u8; N] {
    let mut out = [0; N];
    rng.fill_bytes(&mut out);
    out
}
fn timestamp(ttl: Duration) -> String {
    let time: chrono::DateTime<chrono::Utc> = (SystemTime::now() + ttl).into();
    time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
impl HostPake {
    pub fn new(host_id: String) -> Self {
        let now = Instant::now();
        Self {
            host_id,
            epoch: 0,
            claim: None,
            link: None,
            exchanges: HashMap::new(),
            tokens: HashMap::new(),
            completed_links: HashMap::new(),
            completed_claims: HashMap::new(),
            buckets: HashMap::new(),
            window: now,
            starts: 0,
            failures: 0,
            backoff: now,
        }
    }
    pub fn rotate_setup(&mut self, authority: &impl ClaimAuthority) -> Result<(SetupCode, String)> {
        if !authority.is_unclaimed() {
            return Err(PairingFailed);
        }
        self.install_code(SetupCode::generate(), Purpose::Claim, None, Instant::now())
    }
    /// True when an unexpired claim setup code is loaded in this process.
    pub fn has_active_claim(&mut self) -> bool {
        self.reap(Instant::now());
        self.claim.is_some()
    }
    pub fn rotate_link(&mut self, link_id: String) -> Result<(SetupCode, String)> {
        self.install_code(
            SetupCode::generate(),
            Purpose::Link,
            Some(link_id),
            Instant::now(),
        )
    }
    fn install_code(
        &mut self,
        password: SetupCode,
        purpose: Purpose,
        link_id: Option<String>,
        now: Instant,
    ) -> Result<(SetupCode, String)> {
        self.epoch = self.epoch.checked_add(1).ok_or(PairingFailed)?;
        self.exchanges
            .retain(|_, x| x.capability.purpose != purpose);
        self.tokens.retain(|_, x| x.capability.purpose != purpose);
        if purpose == Purpose::Link {
            self.completed_links.clear();
        } else {
            self.completed_claims.clear();
        }
        let shown = SetupCode::parse(password.canonical())?;
        let ttl = Duration::from_secs(if purpose == Purpose::Claim { 1800 } else { 600 });
        let expires_at = timestamp(ttl);
        let code = Some(Code {
            password,
            deadline: now + ttl,
            epoch: self.epoch,
            link_id,
        });
        if purpose == Purpose::Claim {
            self.claim = code
        } else {
            self.link = code
        }
        Ok((shown, expires_at))
    }
    fn reap(&mut self, now: Instant) {
        self.exchanges
            .retain(|_, x| x.deadline > now && x.connection.strong_count() > 0);
        self.tokens
            .retain(|_, x| x.deadline > now && x.connection.strong_count() > 0);
        self.completed_links
            .retain(|_, x| x.deadline > now && x.connection.strong_count() > 0);
        self.completed_claims
            .retain(|_, x| x.deadline > now && x.connection.strong_count() > 0);
        if self.claim.as_ref().is_some_and(|x| x.deadline <= now) {
            self.claim = None;
        }
        if self.link.as_ref().is_some_and(|x| x.deadline <= now) {
            self.link = None;
        }
        if now.duration_since(self.window) >= Duration::from_secs(60) {
            self.window = now;
            self.starts = 0;
            self.buckets.clear();
        }
    }
    pub fn start(
        &mut self,
        authority: &impl ClaimAuthority,
        connection: &Arc<Connection>,
        peer: IpAddr,
        request: Start,
    ) -> Result<Started> {
        self.start_inner(authority, connection, peer, request, Instant::now(), OsRng)
    }
    fn start_inner(
        &mut self,
        authority: &impl ClaimAuthority,
        connection: &Arc<Connection>,
        peer: IpAddr,
        request: Start,
        now: Instant,
        mut rng: impl CryptoRng + RngCore,
    ) -> Result<Started> {
        self.reap(now);
        if now < self.backoff
            || self.starts >= 20
            || self.buckets.get(&peer).copied().unwrap_or(0) >= 5
            || (!self.buckets.contains_key(&peer) && self.buckets.len() >= 1024)
        {
            return Err(PairingFailed);
        }
        self.starts += 1;
        *self.buckets.entry(peer).or_default() += 1;
        if request.host_id.len() > 36
            || request.device_key.len() > 43
            || request.client_nonce.len() > 43
            || request.message_a.len() > 44
            || request.link_id.as_ref().is_some_and(|id| id.len() > 36)
        {
            return Err(PairingFailed);
        }
        if self.exchanges.len() >= 16
            || self.tokens.len() >= 32
            || self.exchanges.values().filter(|x| x.peer == peer).count() >= 2
        {
            return Err(PairingFailed);
        }
        // Perform the same bounded SPAKE work on invalid state, identity or messages.
        let code = if request.purpose == Purpose::Claim {
            self.claim.as_ref()
        } else {
            self.link.as_ref()
        };
        let valid = code.is_some_and(|c| request.link_id.is_none() || request.link_id == c.link_id)
            && request.host_id == self.host_id
            && (request.purpose != Purpose::Claim
                || (authority.is_unclaimed() && request.link_id.is_none()));
        let dummy = SetupCode::generate();
        let password = code.map_or(&dummy, |c| &c.password);
        let (a, b) = identities(&request);
        let (state, message) = Spake2::<Ed25519Group>::start_b_with_rng(
            &Password::new(password.canonical().as_bytes()),
            &a,
            &b,
            &mut rng,
        );
        let peer_message = decode::<33>(&request.message_a).unwrap_or([0; 33]);
        let shared = state.finish(&peer_message).map(Zeroizing::new);
        if !valid
            || decode::<32>(&request.device_key).is_err()
            || decode::<32>(&request.client_nonce).is_err()
        {
            return Err(PairingFailed);
        }
        let shared = shared.map_err(|_| PairingFailed)?;
        let code = code.ok_or(PairingFailed)?;
        let ttl = Duration::from_secs(120).min(code.deadline.saturating_duration_since(now));
        let response = Started {
            exchange_id: uuid::Builder::from_random_bytes(random(&mut rng))
                .into_uuid()
                .to_string(),
            link_id: code.link_id.clone(),
            host_id: self.host_id.clone(),
            fingerprint: connection.fingerprint.clone(),
            server_nonce: encode(&random::<32>(&mut rng)),
            message_b: encode(&message),
            expires_at: timestamp(ttl),
        };
        let confirmation = Confirmation::new(
            &shared,
            connection.exporter(),
            &transcript(&request, &response)?,
        );
        let capability = Capability {
            exchange_id: response.exchange_id.clone(),
            device_key: request.device_key,
            purpose: request.purpose,
            link_id: code.link_id.clone(),
            code_epoch: code.epoch,
            valid_until: code.deadline,
        };
        self.exchanges.insert(
            response.exchange_id.clone(),
            Exchange {
                confirmation,
                connection: Arc::downgrade(connection),
                deadline: now + ttl,
                capability,
                peer,
            },
        );
        Ok(response)
    }
    pub fn finish(&mut self, connection: &Arc<Connection>, request: Finish) -> Result<Finished> {
        let now = Instant::now();
        self.reap(now);
        // Remove before verification: finish is single-use, including failed proofs.
        let exchange = self
            .exchanges
            .remove(&request.exchange_id)
            .ok_or(PairingFailed)?;
        if !exchange.connection.ptr_eq(&Arc::downgrade(connection))
            || exchange
                .confirmation
                .verify_client(&request.client_confirm)
                .is_err()
        {
            self.failures = self.failures.saturating_add(1);
            if self.failures >= 5 {
                self.backoff = now + Duration::from_secs(30);
                self.failures = 0;
            }
            return Err(PairingFailed);
        }
        let code = self.code(&exchange.capability)?;
        let ttl = Duration::from_secs(600).min(code.deadline.saturating_duration_since(now));
        if self.tokens.len() >= 32 {
            return Err(PairingFailed);
        }
        let token = encode(&random::<32>(&mut OsRng));
        let hash = token_hash(&token)?;
        let response = Finished {
            server_confirm: exchange.confirmation.server(),
            pake_token: token,
            expires_at: timestamp(ttl),
        };
        let mut capability = exchange.capability;
        capability.valid_until = now + ttl;
        self.tokens.insert(
            hash,
            Token {
                connection: exchange.connection,
                deadline: now + ttl,
                capability,
            },
        );
        Ok(response)
    }
    fn code(&self, cap: &Capability) -> Result<&Code> {
        let code = if cap.purpose == Purpose::Claim {
            self.claim.as_ref()
        } else {
            self.link.as_ref()
        };
        code.filter(|c| c.epoch == cap.code_epoch && c.deadline > Instant::now())
            .ok_or(PairingFailed)
    }
    /// Authorizes account challenge/completion and the matching claim/link only.
    /// Never translate this capability into an ordinary host bearer.
    pub fn account_capability(
        &mut self,
        connection: &Arc<Connection>,
        token: &str,
    ) -> Result<Capability> {
        self.reap(Instant::now());
        let t = self.tokens.get(&token_hash(token)?).ok_or(PairingFailed)?;
        if !t.connection.ptr_eq(&Arc::downgrade(connection)) {
            return Err(PairingFailed);
        }
        self.code(&t.capability)?;
        Ok(t.capability.clone())
    }
    pub fn authorize(
        &mut self,
        connection: &Arc<Connection>,
        token: &str,
        purpose: Purpose,
        device_key: &str,
    ) -> Result<Capability> {
        self.reap(Instant::now());
        let t = self.tokens.get(&token_hash(token)?).ok_or(PairingFailed)?;
        if !t.connection.ptr_eq(&Arc::downgrade(connection))
            || t.capability.purpose != purpose
            || t.capability.device_key != device_key
        {
            return Err(PairingFailed);
        }
        self.code(&t.capability)?;
        Ok(t.capability.clone())
    }
    pub fn consume_claim(
        &mut self,
        authority: &impl ClaimAuthority,
        connection: &Arc<Connection>,
        token: &str,
        device_key: &str,
    ) -> Result<()> {
        let cap = self.authorize(connection, token, Purpose::Claim, device_key)?;
        authority.consume_claim(&cap)?;
        self.claim = None;
        let completed: Vec<_> = self
            .tokens
            .iter()
            .filter(|(_, t)| t.capability.exchange_id == cap.exchange_id)
            .map(|(key, _)| *key)
            .collect();
        for key in completed {
            if let Some(token) = self.tokens.remove(&key) {
                self.completed_claims.insert(key, token);
            }
        }
        self.tokens
            .retain(|_, x| x.capability.purpose != Purpose::Claim);
        self.exchanges
            .retain(|_, x| x.capability.purpose != Purpose::Claim);
        Ok(())
    }
    /// Authorizes only an exact retry of the durable claim receipt, never a new
    /// account proof or claim. Reconnection uses fresh device session proof.
    pub fn claim_result_capability(
        &mut self,
        connection: &Arc<Connection>,
        token: &str,
    ) -> Result<Capability> {
        self.reap(Instant::now());
        let t = self
            .completed_claims
            .get(&token_hash(token)?)
            .ok_or(PairingFailed)?;
        if !t.connection.ptr_eq(&Arc::downgrade(connection)) {
            return Err(PairingFailed);
        }
        Ok(t.capability.clone())
    }
    /// H04 calls only after committing the exact approved link request.
    pub fn validate_link_capability(&mut self, cap: &Capability) -> Result<()> {
        self.reap(Instant::now());
        self.code(cap)?;
        if cap.purpose != Purpose::Link
            || !self.tokens.values().any(|t| {
                t.capability.exchange_id == cap.exchange_id
                    && t.capability.code_epoch == cap.code_epoch
            })
        {
            return Err(PairingFailed);
        }
        Ok(())
    }
    pub fn consume_link(&mut self, cap: &Capability) -> Result<()> {
        self.code(cap)?;
        if cap.purpose != Purpose::Link {
            return Err(PairingFailed);
        }
        self.link = None;
        let completed: Vec<_> = self
            .tokens
            .iter()
            .filter(|(_, x)| x.capability.exchange_id == cap.exchange_id)
            .map(|(k, _)| *k)
            .collect();
        for key in completed {
            if let Some(token) = self.tokens.remove(&key) {
                self.completed_links.insert(key, token);
            }
        }
        self.tokens
            .retain(|_, x| x.capability.purpose != Purpose::Link);
        self.exchanges
            .retain(|_, x| x.capability.purpose != Purpose::Link);
        Ok(())
    }
    /// Completed link tokens allow only polling the committed device id. They
    /// cannot authorize account proof, enrollment, or ordinary host operations.
    pub fn authorize_link_status(
        &mut self,
        connection: &Arc<Connection>,
        token: &str,
        device_key: &str,
    ) -> Result<Capability> {
        if let Ok(cap) = self.authorize(connection, token, Purpose::Link, device_key) {
            return Ok(cap);
        }
        self.reap(Instant::now());
        let t = self
            .completed_links
            .get(&token_hash(token)?)
            .ok_or(PairingFailed)?;
        if !t.connection.ptr_eq(&Arc::downgrade(connection))
            || t.capability.device_key != device_key
        {
            return Err(PairingFailed);
        }
        Ok(t.capability.clone())
    }
    pub fn link_poll_capability(
        &mut self,
        connection: &Arc<Connection>,
        token: &str,
    ) -> Result<Capability> {
        if let Ok(cap) = self.account_capability(connection, token) {
            if cap.purpose == Purpose::Link {
                return Ok(cap);
            }
            return Err(PairingFailed);
        }
        self.reap(Instant::now());
        let t = self
            .completed_links
            .get(&token_hash(token)?)
            .ok_or(PairingFailed)?;
        if !t.connection.ptr_eq(&Arc::downgrade(connection)) {
            return Err(PairingFailed);
        }
        Ok(t.capability.clone())
    }
}
#[cfg(test)]
#[path = "host_pake_tests.rs"]
mod tests;
