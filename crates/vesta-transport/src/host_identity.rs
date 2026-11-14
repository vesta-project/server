//! Host identity orchestration without HTTP routes. H08 supplies the live TLS
//! Connection and authorization header. Never hold the PAKE lock over Mojang I/O.
use crate::host_pake::{Capability, ClaimAuthority, Connection, HostPake};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use vesta_control::machine::{
    HostState,
    accounts::*,
    claim::{ClaimCommit, ClaimRequest},
    link::{
        HostLinks, LinkApprovalRequest, LinkApprover, LinkCodeRequest, LinkPollResponse,
        LinkRequest, LinkStatus, LinkSubmitted,
    },
    recovery::{LocalPrivilege, RecoveryRequest, recover},
    sessions::*,
    signin::{SignInRequest, sign_in},
};
use vesta_host_client::{Finish, Finished, PairingFailed, Purpose, Start, Started};
const FAILED: IdentityFailure = IdentityFailure("pairing_failed");
fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| IdentityFailure("identity_unavailable"))
}
impl ClaimAuthority for ClaimCommit<'_> {
    fn is_unclaimed(&self) -> bool {
        ClaimCommit::is_unclaimed(self)
    }
    fn consume_claim(&self, cap: &Capability) -> vesta_host_client::Result<()> {
        self.commit_checked(
            &cap.exchange_id,
            &cap.device_key,
            cap.purpose == Purpose::Claim,
            || std::time::Instant::now() < cap.valid_until,
        )
        .map_err(|_| PairingFailed)
    }
}
struct Advisory<'a>(&'a HostState);
impl ClaimAuthority for Advisory<'_> {
    fn is_unclaimed(&self) -> bool {
        self.0
            .snapshot()
            .is_ok_and(|s| s.primary_account_id.is_none())
    }
    fn consume_claim(&self, _: &Capability) -> vesta_host_client::Result<()> {
        Err(PairingFailed)
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimResponse {
    pub host_id: Uuid,
    pub fingerprint: String,
    pub account_id: Uuid,
    pub device_id: Uuid,
    pub session: String,
    pub expires_at: String,
    pub auth_generation: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignInResponse {
    pub state: &'static str,
    pub host_id: Uuid,
    pub fingerprint: String,
    pub account_id: Uuid,
    pub device_id: Uuid,
    pub device_name: String,
    pub session: String,
    pub expires_at: String,
    pub auth_generation: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkCodeResponse {
    pub link_id: Uuid,
    pub code: String,
    pub expires_at: String,
}
/// Anonymous sign-in challenges spend the shared Mojang challenge budget, so
/// they get their own small per-address and host-wide allowance.
struct SignInLimits {
    window: std::time::Instant,
    host: u32,
    peers: std::collections::HashMap<std::net::IpAddr, u8>,
}
impl SignInLimits {
    const PER_PEER: u8 = 5;
    const PER_HOST: u32 = 30;
    fn admit(&mut self, peer: std::net::IpAddr) -> Result<()> {
        let now = std::time::Instant::now();
        if now.duration_since(self.window) >= std::time::Duration::from_secs(60) {
            self.window = now;
            self.host = 0;
            self.peers.clear();
        }
        let used = self.peers.get(&peer).copied();
        if self.host >= Self::PER_HOST
            || used.unwrap_or(0) >= Self::PER_PEER
            || (used.is_none() && self.peers.len() >= 1024)
        {
            return Err(IdentityFailure("rate_limited"));
        }
        self.host += 1;
        *self.peers.entry(peer).or_default() += 1;
        Ok(())
    }
}
pub struct HostIdentity {
    pub state: HostState,
    pake: Mutex<HostPake>,
    accounts: Mutex<HostAccounts>,
    sessions: Mutex<HostSessions>,
    links: Mutex<HostLinks>,
    link_cap: Mutex<Option<Capability>>,
    sign_in_limits: Mutex<SignInLimits>,
}
impl HostIdentity {
    pub fn new(state: HostState) -> Result<Self> {
        let host = state.snapshot().map_err(|_| FAILED)?.host_id.to_string();
        Ok(Self {
            pake: Mutex::new(HostPake::new(host)),
            accounts: Mutex::new(HostAccounts::new(state.clone())?),
            sessions: Mutex::new(HostSessions::new(state.clone())),
            links: Mutex::new(HostLinks::new(state.clone())),
            link_cap: Mutex::new(None),
            sign_in_limits: Mutex::new(SignInLimits {
                window: std::time::Instant::now(),
                host: 0,
                peers: Default::default(),
            }),
            state,
        })
    }
    pub fn pake_start(
        &self,
        connection: &Arc<Connection>,
        peer: std::net::IpAddr,
        request: Start,
    ) -> Result<Started> {
        let mut pake = lock(&self.pake)?;
        // Setup codes live only in process memory. After a restart or expiry there
        // is no active code; tell the launcher distinctly rather than pairing_failed.
        if request.purpose == Purpose::Claim && !pake.has_active_claim() {
            return Err(IdentityFailure("setup_code_expired_or_replaced"));
        }
        pake.start(&Advisory(&self.state), connection, peer, request)
            .map_err(|_| FAILED)
    }
    pub fn pake_finish(&self, connection: &Arc<Connection>, request: Finish) -> Result<Finished> {
        lock(&self.pake)?
            .finish(connection, request)
            .map_err(|_| FAILED)
    }
    fn scope(
        &self,
        connection: &Arc<Connection>,
        token: &str,
        key: &str,
        purpose: AccountPurpose,
        binding: &str,
    ) -> Result<AccountScope> {
        let purpose_pake = match purpose {
            AccountPurpose::Claim => Purpose::Claim,
            AccountPurpose::Link => Purpose::Link,
            AccountPurpose::Invite | AccountPurpose::Signin => return Err(FAILED),
        };
        let cap = lock(&self.pake)?
            .authorize(connection, token, purpose_pake, key)
            .map_err(|_| FAILED)?;
        if cap.exchange_id != binding {
            return Err(FAILED);
        }
        Ok(AccountScope {
            connection_id: connection.identity_id,
            device_key: key.into(),
            purpose,
            binding: binding.into(),
        })
    }
    pub fn account_challenge(
        &self,
        connection: &Arc<Connection>,
        token: &str,
        request: AccountChallengeRequest,
    ) -> Result<AccountChallengeResponse> {
        let scope = self.scope(
            connection,
            token,
            &request.device_key,
            request.purpose,
            &request.binding,
        )?;
        lock(&self.accounts)?.challenge(&scope, request)
    }
    /// Purpose/key/exchange come from the currently authorized capability.
    pub fn account_complete(
        &self,
        connection: &Arc<Connection>,
        token: &str,
        request: AccountCompleteRequest,
    ) -> Result<AccountProofResponse> {
        let cap = lock(&self.pake)?
            .account_capability(connection, token)
            .map_err(|_| FAILED)?;
        let purpose = match cap.purpose {
            Purpose::Claim => AccountPurpose::Claim,
            Purpose::Link => AccountPurpose::Link,
        };
        let scope = self.scope(
            connection,
            token,
            &cap.device_key,
            purpose,
            &cap.exchange_id,
        )?;
        let proof = lock(&self.accounts)?.complete(&scope, request)?;
        self.scope(
            connection,
            token,
            &cap.device_key,
            purpose,
            &cap.exchange_id,
        )?;
        Ok(proof)
    }
    pub fn claim(
        &self,
        connection: &Arc<Connection>,
        token: &str,
        request: ClaimRequest,
    ) -> Result<ClaimResponse> {
        // Account completion may be doing provider I/O. Wait for that store
        // before acquiring PAKE, so setup rotation remains responsive.
        let mut accounts = lock(&self.accounts)?;
        let mut pake = lock(&self.pake)?;
        let (cap, retry) = match pake.account_capability(connection, token) {
            Ok(cap) => (cap, false),
            Err(_) => (
                pake.claim_result_capability(connection, token)
                    .map_err(|_| FAILED)?,
                true,
            ),
        };
        if cap.purpose != Purpose::Claim {
            return Err(FAILED);
        }
        let key = cap.device_key.clone();
        let scope = AccountScope {
            connection_id: connection.identity_id,
            device_key: key.clone(),
            purpose: AccountPurpose::Claim,
            binding: cap.exchange_id.clone(),
        };
        let commit = ClaimCommit::new(&self.state, &mut accounts, scope, request);
        if retry {
            ClaimAuthority::consume_claim(&commit, &cap).map_err(|_| FAILED)?;
        } else {
            pake.consume_claim(&commit, connection, token, &key)
                .map_err(|_| FAILED)?;
        }
        if std::time::Instant::now() >= cap.valid_until {
            return Err(FAILED);
        }
        let s = lock(&self.sessions)?.claimed(&commit)?;
        Ok(ClaimResponse {
            host_id: s.host_id,
            fingerprint: connection.fingerprint.clone(),
            account_id: s.account_id,
            device_id: s.device_id,
            session: s.session,
            expires_at: s.expires_at,
            auth_generation: s.auth_generation,
        })
    }
    /// No code: the binding must be this connection's TLS certificate
    /// fingerprint, so the Mojang join covers the certificate the launcher saw.
    pub fn sign_in_challenge(
        &self,
        connection: &Arc<Connection>,
        peer: std::net::IpAddr,
        request: AccountChallengeRequest,
    ) -> Result<AccountChallengeResponse> {
        if !matches!(request.purpose, AccountPurpose::Signin | AccountPurpose::Invite) || (request.purpose == AccountPurpose::Signin && request.binding != connection.fingerprint) || (request.purpose == AccountPurpose::Invite && (request.binding.len()!=64 || !request.binding.bytes().all(|b|b.is_ascii_hexdigit())))
        {
            return Err(FAILED);
        }
        lock(&self.sign_in_limits)?.admit(peer)?;
        let scope = AccountScope {
            connection_id: connection.identity_id,
            device_key: request.device_key.clone(),
            purpose: request.purpose,
            binding: request.binding.clone(),
        };
        lock(&self.accounts)?.challenge(&scope, request)
    }
    pub fn sign_in_complete(
        &self,
        connection: &Arc<Connection>,
        request: AccountCompleteRequest,
    ) -> Result<AccountProofResponse> {
        let mut accounts = lock(&self.accounts)?;
        let scope = accounts
            .pending_sign_in_scope(&request.challenge_id)
            .ok_or(FAILED)?;
        if scope.connection_id != connection.identity_id || (scope.purpose == AccountPurpose::Signin && scope.binding != connection.fingerprint)
        {
            return Err(FAILED);
        }
        accounts.complete(&scope, request)
    }
    pub fn redeem_invite(&self, connection: &Arc<Connection>, peer: std::net::IpAddr, request: vesta_control::machine::invites::RedeemInvite) -> Result<serde_json::Value> {
        lock(&self.sign_in_limits)?.admit(peer)?;
        let (_, binding) = vesta_control::machine::invites::binding(&request.invite)?;
        let scope=AccountScope { connection_id:connection.identity_id,device_key:request.device_key.clone(),purpose:AccountPurpose::Invite,binding };
        let (enrollment,server,role)=vesta_control::machine::invites::redeem(&self.state,&mut *lock(&self.accounts)?,&scope,&connection.fingerprint,&request)?;
        let session=lock(&self.sessions)?.signed_in(&enrollment)?;
        Ok(serde_json::json!({"hostId":session.host_id,"serverId":server,"role":role,"session":session.session,"expiresAt":session.expires_at,"authGeneration":session.auth_generation}))
    }
    /// Enrolls (or re-confirms) this device for the verified account when the
    /// account already holds a role here, then issues its first session.
    pub fn sign_in(
        &self,
        connection: &Arc<Connection>,
        request: SignInRequest,
    ) -> Result<SignInResponse> {
        let mut accounts = lock(&self.accounts)?;
        let scope = AccountScope {
            connection_id: connection.identity_id,
            device_key: request.device_key.clone(),
            purpose: AccountPurpose::Signin,
            binding: connection.fingerprint.clone(),
        };
        let signed = sign_in(
            &self.state,
            &mut accounts,
            &scope,
            &connection.fingerprint,
            &request,
        )?;
        drop(accounts);
        let s = lock(&self.sessions)?.signed_in(&signed)?;
        Ok(SignInResponse {
            state: "enrolled",
            host_id: s.host_id,
            fingerprint: connection.fingerprint.clone(),
            account_id: s.account_id,
            device_id: s.device_id,
            device_name: signed.device().name.clone(),
            session: s.session,
            expires_at: s.expires_at,
            auth_generation: s.auth_generation,
        })
    }
    pub fn session_challenge(&self, request: SessionChallengeRequest) -> Result<SessionChallenge> {
        lock(&self.sessions)?.challenge(request)
    }
    pub fn session_key_challenge(
        &self,
        request: SessionKeyChallengeRequest,
    ) -> Result<SessionKeyChallenge> {
        lock(&self.sessions)?.challenge_key(request)
    }
    pub fn session_complete(&self, request: SessionRequest) -> Result<SessionResponse> {
        lock(&self.sessions)?.complete(request)
    }
    pub fn authenticate(&self, token: &str) -> Result<vesta_control::machine::owners::Device> {
        lock(&self.sessions)?.authenticate(token)
    }
    pub fn logout(&self, token: &str) -> Result<()> {
        lock(&self.sessions)?.logout(token)
    }
    /// Health/claim state for the privileged OS-local installer, without secrets.
    pub fn local_status(&self, _local: LocalPrivilege) -> Result<serde_json::Value> {
        let state = self.state.snapshot().map_err(|_| FAILED)?;
        Ok(serde_json::json!({"hostId":state.host_id,"claimed":state.primary_account_id.is_some()}))
    }
    pub fn setup_code(&self, _local: LocalPrivilege) -> Result<serde_json::Value> {
        if self
            .state
            .snapshot()
            .map_err(|_| FAILED)?
            .primary_account_id
            .is_some()
        {
            return Err(IdentityFailure("already_claimed"));
        }
        let (code, expires_at) = lock(&self.pake)?
            .rotate_setup(&Advisory(&self.state))
            .map_err(|_| FAILED)?;
        self.state
            .write(|c| {
                vesta_control::machine::recovery::audit_setup_rotation(
                    c,
                    _local,
                    self.state.now_ms(),
                )
            })
            .map_err(|_| FAILED)?;
        Ok(serde_json::json!({"code": code.display(), "expiresAt": expires_at}))
    }
    fn create_link(&self, actor: LinkApprover, target: Option<Uuid>) -> Result<LinkCodeResponse> {
        let mut pake = lock(&self.pake)?;
        let mut links = lock(&self.links)?;
        let id = links.create(actor, target)?;
        let (code, _) = pake.rotate_link(id.to_string()).map_err(|_| FAILED)?;
        *lock(&self.link_cap)? = None;
        let expires_at = links.status(id, true)?.expires_at;
        Ok(LinkCodeResponse {
            link_id: id,
            code: code.display(),
            expires_at,
        })
    }
    pub fn owner_link_code(
        &self,
        session: &str,
        request: LinkCodeRequest,
    ) -> Result<LinkCodeResponse> {
        let d = self.authenticate(session)?;
        self.create_link(
            LinkApprover::Device(d.device_id),
            Some(request.target_account_id),
        )
    }
    pub fn local_link_code(&self, local: LocalPrivilege) -> Result<LinkCodeResponse> {
        self.create_link(LinkApprover::Local(local), None)
    }
    pub fn submit_link(
        &self,
        connection: &Arc<Connection>,
        token: &str,
        request: LinkRequest,
    ) -> Result<LinkSubmitted> {
        let mut accounts = lock(&self.accounts)?;
        let mut pake = lock(&self.pake)?;
        let cap = pake
            .authorize(
                connection,
                token,
                Purpose::Link,
                &request.request.device_key,
            )
            .map_err(|_| FAILED)?;
        if cap.link_id.as_deref() != Some(request.request.link_id.to_string().as_str()) {
            return Err(FAILED);
        }
        let scope = AccountScope {
            connection_id: connection.identity_id,
            device_key: cap.device_key.clone(),
            purpose: AccountPurpose::Link,
            binding: cap.exchange_id.clone(),
        };
        let result = lock(&self.links)?.submit(&mut accounts, &scope, request)?;
        *lock(&self.link_cap)? = Some(cap);
        Ok(LinkSubmitted {
            link_id: result.link_id,
            state: result.state,
            request_hash: result.request_hash.ok_or(FAILED)?,
        })
    }
    pub fn link_status(
        &self,
        connection: &Arc<Connection>,
        token: &str,
        id: Uuid,
    ) -> Result<LinkPollResponse> {
        let cap = lock(&self.pake)?
            .link_poll_capability(connection, token)
            .map_err(|_| FAILED)?;
        if cap.link_id.as_deref() != Some(id.to_string().as_str()) {
            return Err(FAILED);
        }
        let status = lock(&self.links)?.status(id, false)?;
        if let Some(device_id) = status.device_id {
            Ok(LinkPollResponse::Approved {
                state: "approved".into(),
                device_id,
            })
        } else if status.state == "awaiting_approval" {
            Ok(LinkPollResponse::Pending(status))
        } else {
            Err(FAILED)
        }
    }
    pub fn owner_link_status(&self, session: &str, id: Uuid) -> Result<LinkStatus> {
        let device = self.authenticate(session)?;
        if self
            .state
            .host_role(device.account_id)
            .map_err(|_| FAILED)?
            .is_none()
        {
            return Err(FAILED);
        }
        lock(&self.links)?.status(id, true)
    }
    fn approve(&self, actor: LinkApprover, id: Uuid, hash: &str) -> Result<Uuid> {
        let mut pake = lock(&self.pake)?;
        let mut links = lock(&self.links)?;
        if links
            .status(id, true)
            .is_ok_and(|s| s.state == "approved" && s.request_hash.as_deref() == Some(hash))
        {
            return Ok(links.approve(actor, id, hash)?.device_id);
        }
        let cap = lock(&self.link_cap)?.clone().ok_or(FAILED)?;
        if cap.link_id.as_deref() != Some(id.to_string().as_str()) {
            return Err(FAILED);
        }
        pake.validate_link_capability(&cap).map_err(|_| FAILED)?;
        let device = links.approve_checked(actor, id, hash, || {
            std::time::Instant::now() < cap.valid_until
        })?;
        let _ = pake.consume_link(&cap);
        Ok(device.device_id)
    }
    pub fn owner_approve_link(
        &self,
        session: &str,
        id: Uuid,
        request: LinkApprovalRequest,
    ) -> Result<Uuid> {
        let device = self.authenticate(session)?;
        self.approve(
            LinkApprover::Device(device.device_id),
            id,
            &request.request_hash,
        )
    }
    pub fn local_approve_link(&self, local: LocalPrivilege, id: Uuid, hash: &str) -> Result<Uuid> {
        self.approve(LinkApprover::Local(local), id, hash)
    }
    pub fn local_link_status(&self, _local: LocalPrivilege, id: Uuid) -> Result<LinkStatus> {
        lock(&self.links)?.status(id, true)
    }
    pub fn local_pending_link(&self, _local: LocalPrivilege) -> Result<LinkStatus> {
        lock(&self.links)?.current_status()
    }
    pub fn local_recover(&self, local: LocalPrivilege, request: &RecoveryRequest) -> Result<Uuid> {
        let mut pake = lock(&self.pake)?;
        let device = recover(&self.state, local, request)?;
        *pake = HostPake::new(
            self.state
                .snapshot()
                .map_err(|_| FAILED)?
                .host_id
                .to_string(),
        );
        lock(&self.links)?.invalidate();
        *lock(&self.link_cap)? = None;
        Ok(device.device_id)
    }
}

#[cfg(test)]
impl HostIdentity {
    pub(crate) fn test_account(&self,account:Uuid,name:&str){*self.accounts.lock().unwrap()=HostAccounts::test_profile(self.state.clone(),account,name);}
}
