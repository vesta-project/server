use super::{HostState, accounts::*, claim::*, link::*, recovery::*, sessions::*};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::json;
use std::sync::{Arc, Barrier};
use uuid::Uuid;
fn state() -> (tempfile::TempDir, HostState) {
    let dir = tempfile::tempdir().unwrap();
    let state = HostState::open(dir.path(), Arc::new(vesta_state::SystemClock)).unwrap();
    (dir, state)
}
fn key(n: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[n; 32]).unwrap()
}
fn scope(key: &Ed25519KeyPair, purpose: AccountPurpose) -> AccountScope {
    AccountScope {
        connection_id: Uuid::new_v4(),
        device_key: B64.encode(key.public_key().as_ref()),
        purpose,
        binding: Uuid::new_v4().to_string(),
    }
}
fn claim_request(state: &HostState, key: &Ed25519KeyPair, scope: &AccountScope) -> ClaimRequest {
    let mut r = ClaimRequest {
        exchange_id: scope.binding.clone(),
        account_proof_id: "verified-proof".into(),
        device_name: "Laptop".into(),
        signature: String::new(),
    };
    r.signature=B64.encode(key.sign(&signed_bytes("vesta.host.claim.v1",&json!({"hostId":state.snapshot().unwrap().host_id,"exchangeId":r.exchange_id,"accountProofId":r.account_proof_id,"deviceKey":scope.device_key,"deviceName":r.device_name}))).as_ref());
    r
}
#[test]
fn machine_claim_lost_reply_is_durable_and_wrong_signature_keeps_proof() {
    let (dir, state) = state();
    let key = key(50);
    let scope = scope(&key, AccountPurpose::Claim);
    let account = Uuid::new_v4();
    let mut accounts = HostAccounts::new(state.clone()).unwrap();
    accounts.seed_test_proof("verified-proof", &scope, account);
    let r = claim_request(&state, &key, &scope);
    let mut bad = r.clone();
    bad.device_name = "Substituted".into();
    assert!(
        ClaimCommit::new(&state, &mut accounts, scope.clone(), bad)
            .commit(&scope.binding, &scope.device_key, true)
            .is_err()
    );
    let commit = ClaimCommit::new(&state, &mut accounts, scope.clone(), r.clone());
    assert!(
        commit
            .commit_checked(&scope.binding, &scope.device_key, true, || false)
            .is_err()
    );
    assert!(state.snapshot().unwrap().primary_account_id.is_none());
    commit
        .commit(&scope.binding, &scope.device_key, true)
        .unwrap();
    let device = commit.device().unwrap();
    drop(commit);
    let generation = state.snapshot().unwrap().auth_generation;
    // Reopen both DB and ephemeral proof store, simulating commit followed by a lost reply/crash.
    let reopened = HostState::open(dir.path(), Arc::new(vesta_state::SystemClock)).unwrap();
    let mut empty = HostAccounts::new(reopened.clone()).unwrap();
    let retry = ClaimCommit::new(&reopened, &mut empty, scope.clone(), r.clone());
    retry
        .commit(&scope.binding, &scope.device_key, true)
        .unwrap();
    assert_eq!(retry.device().unwrap().device_id, device.device_id);
    assert_eq!(reopened.snapshot().unwrap().auth_generation, generation);
    drop(retry);
    let mut changed = r;
    changed.account_proof_id = "different".into();
    assert!(
        ClaimCommit::new(&reopened, &mut empty, scope.clone(), changed)
            .commit(&scope.binding, &scope.device_key, true)
            .is_err()
    );
    assert_eq!(
        reopened.snapshot().unwrap().primary_account_id,
        Some(account)
    );
}
#[test]
fn machine_claim_concurrent_account_proofs_install_one_primary() {
    let (_dir, state) = state();
    let gate = Arc::new(Barrier::new(2));
    let threads = (0..2)
        .map(|i| {
            let state = state.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                let key = key(51 + i);
                let scope = scope(&key, AccountPurpose::Claim);
                let mut accounts = HostAccounts::new(state.clone()).unwrap();
                accounts.seed_test_proof("verified-proof", &scope, Uuid::new_v4());
                let request = claim_request(&state, &key, &scope);
                gate.wait();
                ClaimCommit::new(&state, &mut accounts, scope.clone(), request)
                    .commit(&scope.binding, &scope.device_key, true)
                    .is_ok()
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .filter(|ok| *ok)
            .count(),
        1
    );
}
fn signed_link(
    state: &HostState,
    key: &Ed25519KeyPair,
    id: Uuid,
    account: Uuid,
    scope: &AccountScope,
) -> LinkRequest {
    let r = SignedLinkRequest {
        host_id: state.snapshot().unwrap().host_id,
        link_id: id,
        device_key: scope.device_key.clone(),
        account_id: account,
        requested_role: "linked_device".into(),
        device_name: "New laptop".into(),
        nonce: B64.encode([1; 32]),
        expires_at: timestamp(state.now_ms() + 119000),
    };
    let signature = B64.encode(
        key.sign(&signed_bytes(
            "vesta.host.link.v1",
            &serde_json::to_value(&r).unwrap(),
        ))
        .as_ref(),
    );
    LinkRequest {
        exchange_id: scope.binding.clone(),
        account_proof_id: "verified-proof".into(),
        request: r,
        signature,
    }
}
#[test]
fn machine_link_requires_exact_hash_current_owner_and_online_proof() {
    let (_dir, state) = state();
    let owner = state
        .claim(
            Uuid::new_v4(),
            "Owner",
            key(53).public_key().as_ref().try_into().unwrap(),
        )
        .unwrap();
    let local = LocalPrivilege::from_privileged_local_channel();
    let mut links = HostLinks::new(state.clone());
    let id = links
        .create(
            LinkApprover::Device(owner.device_id),
            Some(owner.account_id),
        )
        .unwrap();
    let key = key(54);
    let scope = scope(&key, AccountPurpose::Link);
    let mut accounts = HostAccounts::new(state.clone()).unwrap();
    let request = signed_link(&state, &key, id, owner.account_id, &scope);
    assert!(
        links
            .submit(&mut accounts, &scope, request.clone())
            .is_err()
    );
    accounts.seed_test_proof("verified-proof", &scope, owner.account_id);
    let mut wrong = request.clone();
    wrong.request.device_name = "Substituted".into();
    assert!(links.submit(&mut accounts, &scope, wrong).is_err());
    let result = links
        .submit(&mut accounts, &scope, request.clone())
        .unwrap();
    assert_eq!(result.state, "awaiting_approval");
    assert!(links.status(id, false).unwrap().request.is_none());
    assert!(links.status(id, true).unwrap().request.is_some());
    assert_eq!(
        links
            .submit(&mut accounts, &scope, request)
            .unwrap()
            .request_hash,
        result.request_hash
    );
    assert!(
        links
            .approve(LinkApprover::Local(local), id, &"0".repeat(64))
            .is_err()
    );
    let hash = result.request_hash.unwrap();
    assert!(
        links
            .approve_checked(LinkApprover::Device(owner.device_id), id, &hash, || false)
            .is_err()
    );
    let device = links
        .approve(LinkApprover::Device(owner.device_id), id, &hash)
        .unwrap();
    assert_eq!(device.account_id, owner.account_id);
    let generation = state.snapshot().unwrap().auth_generation;
    assert_eq!(
        links
            .approve(LinkApprover::Local(local), id, &hash)
            .unwrap()
            .device_id,
        device.device_id
    );
    assert_eq!(state.snapshot().unwrap().auth_generation, generation);
    links.create(LinkApprover::Local(local), None).unwrap();
    assert!(links.status(id, true).is_err());
    assert!(
        links
            .approve(LinkApprover::Local(local), id, &hash)
            .is_err()
    );
}
#[test]
fn machine_link_rejects_wrong_account_expired_request_and_revoked_approver() {
    let (_dir, state) = state();
    let owner = state
        .claim(
            Uuid::new_v4(),
            "Owner",
            key(55).public_key().as_ref().try_into().unwrap(),
        )
        .unwrap();
    let local = LocalPrivilege::from_privileged_local_channel();
    let mut links = HostLinks::new(state.clone());
    let id = links
        .create(LinkApprover::Local(local), Some(owner.account_id))
        .unwrap();
    let key = key(56);
    let scope = scope(&key, AccountPurpose::Link);
    let mut accounts = HostAccounts::new(state.clone()).unwrap();
    accounts.seed_test_proof("verified-proof", &scope, owner.account_id);
    let wrong = signed_link(&state, &key, id, Uuid::new_v4(), &scope);
    assert!(links.submit(&mut accounts, &scope, wrong).is_err());
    let mut expired = signed_link(&state, &key, id, owner.account_id, &scope);
    expired.request.expires_at = timestamp(state.now_ms() - 1000);
    assert!(links.submit(&mut accounts, &scope, expired).is_err());
    let r = signed_link(&state, &key, id, owner.account_id, &scope);
    let hash = links
        .submit(&mut accounts, &scope, r)
        .unwrap()
        .request_hash
        .unwrap();
    state
        .revoke_device(super::owners::Actor::LocalRecovery, owner.device_id)
        .unwrap();
    assert!(
        links
            .approve(LinkApprover::Device(owner.device_id), id, &hash)
            .is_err()
    );
    assert!(links.approve(LinkApprover::Local(local), id, &hash).is_ok());
}
#[test]
fn machine_local_recovery_revokes_old_sessions_and_retries_without_new_device() {
    let (_dir, state) = state();
    let old = state
        .claim(
            Uuid::new_v4(),
            "Old",
            key(57).public_key().as_ref().try_into().unwrap(),
        )
        .unwrap();
    let mut sessions = HostSessions::new(state.clone()).unwrap();
    let token = sessions.issue(&old).unwrap().session;
    let local = LocalPrivilege::from_privileged_local_channel();
    let request = RecoveryRequest {
        request_id: Uuid::new_v4(),
        account_id: Uuid::new_v4(),
        device_key: B64.encode(key(58).public_key().as_ref()),
        device_name: "Recovered".into(),
        revoke_all: true,
    };
    assert!(recover(&state, local, &request).is_err());
    state
        .write(|c| {
            HostState::enroll_device_in(
                c,
                request.account_id,
                "Previously verified",
                key(61).public_key().as_ref().try_into().unwrap(),
                state.now_ms(),
            )
        })
        .unwrap();
    let device = recover(&state, local, &request).unwrap();
    assert!(sessions.authenticate(&token).is_err());
    assert_eq!(
        state.snapshot().unwrap().primary_account_id,
        Some(request.account_id)
    );
    assert_eq!(state.host_role(old.account_id).unwrap(), None);
    let generation = state.snapshot().unwrap().auth_generation;
    assert_eq!(
        recover(&state, local, &request).unwrap().device_id,
        device.device_id
    );
    assert_eq!(state.snapshot().unwrap().auth_generation, generation);
}

#[test]
fn machine_lost_claim_reply_recovers_device_id_by_fresh_key_possession() {
    let (_dir, state) = state();
    let key = key(59);
    let scope = scope(&key, AccountPurpose::Claim);
    let mut accounts = HostAccounts::new(state.clone()).unwrap();
    accounts.seed_test_proof("verified-proof", &scope, Uuid::new_v4());
    let request = claim_request(&state, &key, &scope);
    let commit = ClaimCommit::new(&state, &mut accounts, scope.clone(), request);
    commit
        .commit(&scope.binding, &scope.device_key, true)
        .unwrap();
    let device = commit.device().unwrap();
    drop(commit);
    let mut sessions = HostSessions::new(state.clone()).unwrap();
    let recovered = sessions
        .challenge_key(SessionKeyChallengeRequest {
            device_key: scope.device_key.clone(),
        })
        .unwrap();
    assert_eq!(recovered.device_id, device.device_id);
    let c = recovered.challenge;
    let signature=B64.encode(key.sign(&signed_bytes("vesta.host.session.v1",&json!({"hostId":state.snapshot().unwrap().host_id,"deviceId":device.device_id,"challengeId":c.challenge_id,"nonce":c.nonce,"expiresAt":c.expires_at}))).as_ref());
    let session = sessions
        .complete(SessionRequest {
            device_id: device.device_id,
            challenge_id: c.challenge_id,
            signature,
        })
        .unwrap();
    assert_eq!(session.account_id, device.account_id);
    assert_eq!(
        sessions.authenticate(&session.session).unwrap().device_id,
        device.device_id
    );
    let unknown = sessions
        .challenge_key(SessionKeyChallengeRequest {
            device_key: B64.encode(self::key(60).public_key().as_ref()),
        })
        .unwrap();
    assert_ne!(unknown.device_id, device.device_id);
}
