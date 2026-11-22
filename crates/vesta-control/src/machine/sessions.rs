//! Host bearer authority; share one instance for the lifetime of the host process.
use super::{HostState, owners::Device};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use uuid::Uuid;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdentityFailure(pub &'static str);
impl std::fmt::Display for IdentityFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for IdentityFailure {}
pub type Result<T> = std::result::Result<T, IdentityFailure>;
pub(crate) const INVALID: IdentityFailure = IdentityFailure("invalid_session");
pub(crate) const PAIRING: IdentityFailure = IdentityFailure("pairing_failed");
pub(crate) const UNAVAILABLE: IdentityFailure = IdentityFailure("identity_unavailable");
pub(crate) fn random() -> Result<String> {
    let mut b = [0; 32];
    getrandom::fill(&mut b).map_err(|_| UNAVAILABLE)?;
    Ok(B64.encode(b))
}
pub(crate) fn decode<const N: usize>(s: &str) -> Result<[u8; N]> {
    if s.len() != (N * 8).div_ceil(6) {
        return Err(PAIRING);
    }
    let b = B64.decode(s).map_err(|_| PAIRING)?;
    if B64.encode(&b) != s {
        return Err(PAIRING);
    }
    b.try_into().map_err(|_| PAIRING)
}
/// RFC 8785 serialization for the integer/string/object schemas in host identity.
pub fn canonical(value: &Value) -> Vec<u8> {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                out.push('{');
                let mut keys: Vec<_> = m.keys().collect();
                keys.sort_by_cached_key(|k| k.encode_utf16().collect::<Vec<_>>());
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',')
                    }
                    out.push_str(&serde_json::to_string(k).unwrap());
                    out.push(':');
                    write(&m[*k], out)
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',')
                    }
                    write(v, out)
                }
                out.push(']');
            }
            _ => out.push_str(&serde_json::to_string(v).unwrap()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out.into_bytes()
}
pub fn signed_bytes(domain: &str, value: &Value) -> Vec<u8> {
    let mut out = domain.as_bytes().to_vec();
    out.push(0);
    out.extend(canonical(value));
    out
}
pub(crate) fn verify(key: &str, signature: &str, domain: &str, value: &Value) -> Result<()> {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, decode::<32>(key)?)
        .verify(&signed_bytes(domain, value), &decode::<64>(signature)?)
        .map_err(|_| PAIRING)
}
pub(crate) fn timestamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let z = secs.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = y + i64::from(m <= 2);
    let s = secs.rem_euclid(86400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        (s / 60) % 60,
        s % 60
    )
}
pub(crate) struct Deadline {
    wall: i64,
    mono: Instant,
}
impl Deadline {
    pub(crate) fn until(now: i64, expires: i64) -> Self {
        Self {
            wall: expires,
            mono: Instant::now() + Duration::from_millis(expires.saturating_sub(now).max(0) as u64),
        }
    }
    #[cfg(test)]
    pub(crate) fn expire_monotonic_for_test(&mut self) {
        self.mono = Instant::now();
    }
    pub(crate) fn new(now: i64, seconds: u64) -> Self {
        Self {
            wall: now.saturating_add(seconds as i64 * 1000),
            mono: Instant::now() + Duration::from_secs(seconds),
        }
    }
    pub(crate) fn live(&self, now: i64) -> bool {
        now < self.wall && Instant::now() < self.mono
    }
    pub(crate) fn text(&self) -> String {
        timestamp(self.wall)
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionChallengeRequest {
    pub device_id: Uuid,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionKeyChallengeRequest {
    pub device_key: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionKeyChallenge {
    pub device_id: Uuid,
    #[serde(flatten)]
    pub challenge: SessionChallenge,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionChallenge {
    pub challenge_id: String,
    pub nonce: String,
    pub expires_at: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRequest {
    pub device_id: Uuid,
    pub challenge_id: String,
    pub signature: String,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResponse {
    pub session: String,
    pub expires_at: String,
    pub auth_generation: String,
    pub host_id: Uuid,
    pub device_id: Uuid,
    pub account_id: Uuid,
}
struct Challenge {
    device: Uuid,
    response: SessionChallenge,
    deadline: Deadline,
}
struct Bearer {
    device: Uuid,
    account: Uuid,
    deadline: Deadline,
}
pub struct HostSessions {
    state: HostState,
    host_id: Uuid,
    challenges: HashMap<String, Challenge>,
    bearers: HashMap<[u8; 32], Bearer>,
    attempts: u32,
    window: Instant,
}
impl HostSessions {
    /// Lost enrollment replies may hide the newly allocated device id. Look it
    /// up by public key, then require the same fresh Ed25519 session proof. An
    /// unknown/revoked key receives an indistinguishable challenge for a random id.
    pub fn challenge_key(
        &mut self,
        request: SessionKeyChallengeRequest,
    ) -> Result<SessionKeyChallenge> {
        use rusqlite::OptionalExtension;
        let key = decode::<32>(&request.device_key)?;
        let id=self.state.read(|c|{let id:Option<String>=c.query_row("SELECT d.device_id FROM machine_devices d JOIN machine_accounts a USING(account_id) WHERE d.public_key=?1 AND d.revoked_at_ms IS NULL AND a.disabled=0",[key.as_slice()],|r|r.get(0)).optional()?;id.map(super::state::uuid).transpose()}).map_err(|_|UNAVAILABLE)?.unwrap_or_else(Uuid::new_v4);
        let challenge = self.challenge(SessionChallengeRequest { device_id: id })?;
        Ok(SessionKeyChallenge {
            device_id: id,
            challenge,
        })
    }
    /// Only a committed claim callback can issue its initial enrollment bearer.
    pub fn claimed(&mut self, commit: &super::claim::ClaimCommit<'_>) -> Result<SessionResponse> {
        self.issue(&commit.device()?)
    }
    /// Only a committed account sign-in can issue its initial bearer.
    pub fn signed_in(&mut self, enrollment: &super::signin::SignedIn) -> Result<SessionResponse> {
        self.issue(enrollment.device())
    }
    pub fn new(state: HostState) -> Result<Self> {
        let host_id = state.snapshot().map_err(|_| UNAVAILABLE)?.host_id;
        Ok(Self {
            state,
            host_id,
            challenges: HashMap::new(),
            bearers: HashMap::new(),
            attempts: 0,
            window: Instant::now(),
        })
    }
    fn reap(&mut self) {
        let now = self.state.now_ms();
        self.challenges.retain(|_, v| v.deadline.live(now));
        self.bearers.retain(|_, v| v.deadline.live(now));
    }
    fn charge(&mut self) -> Result<()> {
        if self.window.elapsed() >= Duration::from_secs(60) {
            self.window = Instant::now();
            self.attempts = 0;
        }
        if self.attempts >= 120 {
            return Err(IdentityFailure("rate_limited"));
        }
        self.attempts += 1;
        Ok(())
    }
    pub fn retry_after(&self) -> Duration {
        Duration::from_secs(60).saturating_sub(self.window.elapsed())
    }
    pub fn challenge(&mut self, request: SessionChallengeRequest) -> Result<SessionChallenge> {
        self.charge()?;
        self.reap();
        if self.challenges.len() >= 1024 {
            return Err(UNAVAILABLE);
        }
        // Unknown/revoked ids have the same challenge shape and verification error.
        let deadline = Deadline::new(self.state.now_ms(), 60);
        let response = SessionChallenge {
            challenge_id: random()?,
            nonce: random()?,
            expires_at: deadline.text(),
        };
        self.challenges.insert(
            response.challenge_id.clone(),
            Challenge {
                device: request.device_id,
                response: response.clone(),
                deadline,
            },
        );
        Ok(response)
    }
    pub fn complete(&mut self, request: SessionRequest) -> Result<SessionResponse> {
        let pending = self.challenges.remove(&request.challenge_id);
        self.charge()?;
        let pending = pending.ok_or(INVALID)?;
        if pending.device != request.device_id || !pending.deadline.live(self.state.now_ms()) {
            return Err(INVALID);
        }
        let device = self
            .state
            .active_device(request.device_id)
            .map_err(|_| UNAVAILABLE)?
            .ok_or(INVALID)?;
        let host = self.host_id;
        verify(&B64.encode(device.public_key),&request.signature,"vesta.host.session.v1",&json!({"hostId":host,"deviceId":request.device_id,"challengeId":request.challenge_id,"nonce":pending.response.nonce,"expiresAt":pending.response.expires_at})).map_err(|_|INVALID)?;
        self.issue(&device)
    }
    pub(crate) fn issue(&mut self, device: &Device) -> Result<SessionResponse> {
        self.reap();
        if self.bearers.len() >= 4096 {
            return Err(UNAVAILABLE);
        }
        let current = self
            .state
            .active_device(device.device_id)
            .map_err(|_| UNAVAILABLE)?
            .ok_or(INVALID)?;
        if current.account_id != device.account_id {
            return Err(INVALID);
        }
        let snapshot = self.state.snapshot().map_err(|_| UNAVAILABLE)?;
        let token = random()?;
        let deadline = Deadline::new(self.state.now_ms(), 900);
        let expires_at = deadline.text();
        self.bearers.insert(
            self.hash(&token)?,
            Bearer {
                device: device.device_id,
                account: device.account_id,
                deadline,
            },
        );
        Ok(SessionResponse {
            session: token,
            expires_at,
            auth_generation: snapshot.auth_generation.to_string(),
            host_id: snapshot.host_id,
            device_id: device.device_id,
            account_id: device.account_id,
        })
    }
    fn hash(&self, token: &str) -> Result<[u8; 32]> {
        let mut h = Sha256::new();
        h.update(b"vesta.host.session.v1\0");
        h.update(self.host_id.as_bytes());
        h.update(decode::<32>(token).map_err(|_| INVALID)?);
        Ok(h.finalize().into())
    }
    pub fn authenticate(&self, token: &str) -> Result<Device> {
        let b = self.bearers.get(&self.hash(token)?)
            .filter(|b| b.deadline.live(self.state.now_ms())).ok_or(INVALID)?;
        let d = self
            .state
            .active_device(b.device)
            .map_err(|_| UNAVAILABLE)?
            .ok_or(INVALID)?;
        if d.account_id != b.account {
            return Err(INVALID);
        }
        Ok(d)
    }
    pub fn logout(&mut self, token: &str) -> Result<()> {
        self.bearers.remove(&self.hash(token)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::sync::Arc;
    fn fixture() -> (
        tempfile::TempDir,
        HostState,
        Ed25519KeyPair,
        Device,
        HostSessions,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let state = HostState::open(dir.path(), Arc::new(vesta_state::SystemClock)).unwrap();
        let key = Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap();
        let device = state
            .claim(
                Uuid::new_v4(),
                "Laptop",
                key.public_key().as_ref().try_into().unwrap(),
            )
            .unwrap();
        let sessions = HostSessions::new(state.clone()).unwrap();
        (dir, state, key, device, sessions)
    }
    fn request(
        state: &HostState,
        key: &Ed25519KeyPair,
        device: Uuid,
        c: &SessionChallenge,
    ) -> SessionRequest {
        let bytes = signed_bytes(
            "vesta.host.session.v1",
            &json!({"hostId":state.snapshot().unwrap().host_id,"deviceId":device,"challengeId":c.challenge_id,"nonce":c.nonce,"expiresAt":c.expires_at}),
        );
        SessionRequest {
            device_id: device,
            challenge_id: c.challenge_id.clone(),
            signature: B64.encode(key.sign(&bytes).as_ref()),
        }
    }
    #[test]
    fn host_session_replay_restart_logout_and_revocation() {
        let (_dir, state, key, device, mut sessions) = fixture();
        let c = sessions
            .challenge(SessionChallengeRequest {
                device_id: device.device_id,
            })
            .unwrap();
        let response = sessions
            .complete(request(&state, &key, device.device_id, &c))
            .unwrap();
        assert_eq!(
            response.auth_generation,
            state.snapshot().unwrap().auth_generation.to_string()
        );
        assert_eq!(
            sessions.authenticate(&response.session).unwrap().device_id,
            device.device_id
        );
        assert!(
            sessions
                .complete(request(&state, &key, device.device_id, &c))
                .is_err()
        );
        let mut restarted = HostSessions::new(state.clone()).unwrap();
        assert!(restarted.authenticate(&response.session).is_err());
        assert!(
            restarted
                .complete(request(&state, &key, device.device_id, &c))
                .is_err()
        );
        sessions.logout(&response.session).unwrap();
        assert!(sessions.authenticate(&response.session).is_err());
        let response = sessions.issue(&device).unwrap();
        state
            .revoke_device(super::super::owners::Actor::LocalRecovery, device.device_id)
            .unwrap();
        assert!(sessions.authenticate(&response.session).is_err());
    }
    #[test]
    fn host_session_wrong_key_device_host_and_expiry_consume_challenge() {
        let (_dir, state, key, device, mut sessions) = fixture();
        let wrong = Ed25519KeyPair::from_seed_unchecked(&[43; 32]).unwrap();
        for mode in 0..4 {
            let c = sessions
                .challenge(SessionChallengeRequest {
                    device_id: device.device_id,
                })
                .unwrap();
            let mut r = request(
                &state,
                if mode == 0 { &wrong } else { &key },
                device.device_id,
                &c,
            );
            if mode == 1 {
                r.device_id = Uuid::new_v4()
            }
            if mode == 2 {
                r.signature=B64.encode(key.sign(&signed_bytes("vesta.host.session.v1",&json!({"hostId":Uuid::new_v4(),"deviceId":device.device_id,"challengeId":c.challenge_id,"nonce":c.nonce,"expiresAt":c.expires_at}))).as_ref())
            }
            if mode == 3 {
                sessions
                    .challenges
                    .get_mut(&c.challenge_id)
                    .unwrap()
                    .deadline
                    .wall = 0;
            }
            assert!(sessions.complete(r).is_err());
            assert!(
                sessions
                    .complete(request(&state, &key, device.device_id, &c))
                    .is_err()
            );
        }
        let response = sessions.issue(&device).unwrap();
        let hash = sessions.hash(&response.session).unwrap();
        sessions.bearers.get_mut(&hash).unwrap().deadline.mono = Instant::now();
        assert!(sessions.authenticate(&response.session).is_err());
    }
}
