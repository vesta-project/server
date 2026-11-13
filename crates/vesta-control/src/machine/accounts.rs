//! Fresh Mojang-backed host proofs. No access-token field exists on this API.
use super::{HostState, sessions::*};
use crate::identity::minecraft::{
    ChallengeId, MinecraftChallenges, MinecraftName, MinecraftUuid, ServerUuid,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use uuid::Uuid;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccountPurpose {
    Claim,
    Link,
    Invite,
}
/// Trusted transport adapter input, constructed only after authorizing the current
/// PAKE/invite capability on this live connection. Re-authorize at every call.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountScope {
    pub connection_id: Uuid,
    pub device_key: String,
    pub purpose: AccountPurpose,
    pub binding: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountChallengeRequest {
    pub account_id: Uuid,
    pub name: String,
    pub device_key: String,
    pub purpose: AccountPurpose,
    pub binding: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountChallengeResponse {
    pub challenge_id: String,
    pub server_hash: String,
    pub expires_at: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountCompleteRequest {
    pub challenge_id: String,
    pub signature: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProofResponse {
    pub account_proof_id: String,
    pub account_id: Uuid,
    pub expires_at: String,
}
struct Pending {
    scope: AccountScope,
    account: Uuid,
    hash: String,
    deadline: Deadline,
}
struct Proof {
    scope: AccountScope,
    account: Uuid,
    deadline: Deadline,
}
pub struct HostAccounts {
    state: HostState,
    verifier: MinecraftChallenges,
    pending: HashMap<String, Pending>,
    proofs: HashMap<String, Proof>,
}
impl HostAccounts {
    #[cfg(test)]
    pub(crate) fn seed_test_proof(&mut self, id: &str, scope: &AccountScope, account: Uuid) {
        self.proofs.insert(
            id.into(),
            Proof {
                scope: scope.clone(),
                account,
                deadline: Deadline::new(self.state.now_ms(), 120),
            },
        );
    }
    pub fn new(state: HostState) -> Result<Self> {
        let host = state.snapshot().map_err(|_| UNAVAILABLE)?.host_id;
        let verifier =
            MinecraftChallenges::new(ServerUuid::from_uuid(host).map_err(|_| UNAVAILABLE)?)
                .map_err(provider_error)?;
        Ok(Self {
            state,
            verifier,
            pending: HashMap::new(),
            proofs: HashMap::new(),
        })
    }
    pub fn challenge(
        &mut self,
        scope: &AccountScope,
        request: AccountChallengeRequest,
    ) -> Result<AccountChallengeResponse> {
        let now = self.state.now_ms();
        self.pending.retain(|_, p| p.deadline.live(now));
        self.proofs.retain(|_, p| p.deadline.live(now));
        if self.pending.len() + self.proofs.len() >= 1024 {
            return Err(UNAVAILABLE);
        }
        decode::<32>(&request.device_key)?;
        if scope.device_key != request.device_key
            || scope.purpose != request.purpose
            || scope.binding != request.binding
            || request.binding.len() > 256
        {
            return Err(PAIRING);
        }
        let account = MinecraftUuid::from_uuid(request.account_id).map_err(|_| PAIRING)?;
        let name = MinecraftName::parse(&request.name).map_err(|_| PAIRING)?;
        let host = self.state.snapshot().map_err(|_| UNAVAILABLE)?.host_id;
        let issued=self.verifier.issue_host(account,name,|nonce|canonical(&json!({"hostId":host,"deviceKey":request.device_key,"purpose":request.purpose,"binding":request.binding,"accountId":request.account_id,"nonce":nonce}))).map_err(provider_error)?;
        let response = AccountChallengeResponse {
            challenge_id: issued.challenge_id().as_str().into(),
            server_hash: issued.server_hash().as_str().into(),
            expires_at: Deadline::new(now, 60).text(),
        };
        self.pending.insert(
            response.challenge_id.clone(),
            Pending {
                scope: scope.clone(),
                account: request.account_id,
                hash: response.server_hash.clone(),
                deadline: Deadline::new(now, 60),
            },
        );
        Ok(response)
    }
    pub fn complete(
        &mut self,
        scope: &AccountScope,
        request: AccountCompleteRequest,
    ) -> Result<AccountProofResponse> {
        // Consume before signature checks and remote I/O; provider outage requires a fresh challenge.
        let p = self.pending.remove(&request.challenge_id).ok_or(PAIRING)?;
        if &p.scope != scope || !p.deadline.live(self.state.now_ms()) {
            return Err(PAIRING);
        }
        let host = self.state.snapshot().map_err(|_| UNAVAILABLE)?.host_id;
        verify(
            &scope.device_key,
            &request.signature,
            "vesta.host.account.v1",
            &json!({"hostId":host,"deviceKey":scope.device_key,"challengeId":request.challenge_id,"serverHash":p.hash,"purpose":scope.purpose,"binding":scope.binding}),
        )?;
        let id = ChallengeId::parse(&request.challenge_id).map_err(|_| PAIRING)?;
        let profile = self
            .verifier
            .complete_management(&id)
            .map_err(provider_error)?
            .ok_or(PAIRING)?;
        if profile.uuid().as_uuid() != p.account
            || profile.server_uuid().as_uuid() != host
            || !p.deadline.live(self.state.now_ms())
        {
            return Err(PAIRING);
        }
        let proof_id = random()?;
        let deadline = Deadline::new(self.state.now_ms(), 120);
        let expires_at = deadline.text();
        self.proofs.insert(
            proof_id.clone(),
            Proof {
                scope: scope.clone(),
                account: p.account,
                deadline,
            },
        );
        Ok(AccountProofResponse {
            account_proof_id: proof_id,
            account_id: p.account,
            expires_at,
        })
    }
    /// Keeps the proof on transaction failure; consumes only after durable success.
    /// Caller serializes this store with enrollment and rechecks PAKE immediately
    /// before this call. A proof never authorizes a different connection or key.
    pub fn consume<T>(
        &mut self,
        id: &str,
        scope: &AccountScope,
        commit: impl FnOnce(Uuid) -> Result<T>,
    ) -> Result<T> {
        self.consume_checked(id, scope, |account, _| commit(account))
    }
    /// The callback must recheck this lease inside its enrollment transaction,
    /// after waiting for database admission and immediately before mutation.
    pub fn consume_checked<T>(
        &mut self,
        id: &str,
        scope: &AccountScope,
        commit: impl FnOnce(Uuid, &dyn Fn() -> bool) -> Result<T>,
    ) -> Result<T> {
        let proof = self.proofs.get(id).ok_or(PAIRING)?;
        if &proof.scope != scope || !proof.deadline.live(self.state.now_ms()) {
            return Err(PAIRING);
        }
        let result = commit(proof.account, &|| proof.deadline.live(self.state.now_ms()))?;
        self.proofs.remove(id);
        Ok(result)
    }
}
fn provider_error(error: crate::identity::minecraft::MinecraftError) -> IdentityFailure {
    match error.code() {
        "minecraft_unavailable" | "minecraft_busy" => {
            IdentityFailure("account_verification_unavailable")
        }
        "minecraft_rate_limited" => IdentityFailure("rate_limited"),
        _ => PAIRING,
    }
}

#[cfg(test)]
impl HostAccounts {
    pub(crate) fn with_test_verifier(state: HostState, verifier: MinecraftChallenges) -> Self {
        Self {
            state,
            verifier,
            pending: HashMap::new(),
            proofs: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn host_account_proof_deadlines_and_restart_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let state = HostState::open(dir.path(), Arc::new(vesta_state::SystemClock)).unwrap();
        let mut accounts = HostAccounts::new(state.clone()).unwrap();
        let scope = AccountScope {
            connection_id: Uuid::new_v4(),
            device_key: random().unwrap(),
            purpose: AccountPurpose::Invite,
            binding: "invite-digest".into(),
        };
        for wall in [false, true] {
            let mut deadline = Deadline::new(0, 120);
            if !wall {
                deadline = Deadline::new(state.now_ms(), 120);
                deadline.expire_monotonic_for_test();
            }
            accounts.proofs.insert(
                "expired".into(),
                Proof {
                    scope: scope.clone(),
                    account: Uuid::new_v4(),
                    deadline,
                },
            );
            assert!(accounts.consume("expired", &scope, Ok).is_err());
        }
        accounts.proofs.insert(
            "live".into(),
            Proof {
                scope: scope.clone(),
                account: Uuid::new_v4(),
                deadline: Deadline::new(state.now_ms(), 120),
            },
        );
        assert!(
            HostAccounts::new(state)
                .unwrap()
                .consume("live", &scope, Ok)
                .is_err()
        );
    }
}
