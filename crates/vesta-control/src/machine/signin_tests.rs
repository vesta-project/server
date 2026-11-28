use super::{
    HostState,
    accounts::*,
    owners::{Actor, HostRole, ServerRole},
    registry::{ServerEntry, ServerState, Visibility},
    sessions::*,
    settings::*,
    signin::*,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

const FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
struct Fixture {
    _dir: tempfile::TempDir,
    state: HostState,
    accounts: HostAccounts,
    owner: Uuid,
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let state = HostState::open(dir.path(), Arc::new(vesta_state::SystemClock)).unwrap();
    let owner = Uuid::new_v4();
    state
        .claim(
            owner,
            "Desk",
            key(1).public_key().as_ref().try_into().unwrap(),
        )
        .unwrap();
    let accounts = HostAccounts::new(state.clone()).unwrap();
    Fixture {
        _dir: dir,
        state,
        accounts,
        owner,
    }
}
fn key(n: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[n; 32]).unwrap()
}
fn scope(key: &Ed25519KeyPair, fingerprint: &str) -> AccountScope {
    AccountScope {
        connection_id: Uuid::new_v4(),
        device_key: B64.encode(key.public_key().as_ref()),
        purpose: AccountPurpose::Signin,
        binding: fingerprint.into(),
    }
}
fn request(
    key: &Ed25519KeyPair,
    proof: &str,
    name: &str,
    host: Uuid,
    fingerprint: &str,
) -> SignInRequest {
    let device_key = B64.encode(key.public_key().as_ref());
    let signature = B64.encode(
        key.sign(&signed_bytes(
            "vesta.host.signin.v1",
            &json!({"hostId":host,"fingerprint":fingerprint,"accountProofId":proof,"deviceKey":device_key,"deviceName":name}),
        ))
        .as_ref(),
    );
    SignInRequest {
        account_proof_id: proof.into(),
        device_key,
        device_name: name.into(),
        signature,
    }
}
impl Fixture {
    fn host(&self) -> Uuid {
        self.state.snapshot().unwrap().host_id
    }
    fn attempt(
        &mut self,
        key: &Ed25519KeyPair,
        account: Uuid,
        name: &str,
    ) -> (AccountScope, SignInRequest, Result<SignedIn>) {
        let scope = scope(key, FP);
        let proof = Uuid::new_v4().to_string();
        self.accounts.seed_test_proof(&proof, &scope, account);
        let r = request(key, &proof, name, self.host(), FP);
        let result = sign_in(&self.state, &mut self.accounts, &scope, FP, &r);
        (scope, r, result)
    }
    fn device_count(&self) -> usize {
        self.state.devices(Actor::LocalRecovery).unwrap().len()
    }
    fn audits(&self, action: &str) -> i64 {
        self.state
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM machine_audit WHERE action=?1",
                    [action],
                    |r| r.get(0),
                )?)
            })
            .unwrap()
    }
}

#[test]
fn machine_sign_in_enrolls_role_holder_without_code_and_session_follows_revocation() {
    let mut f = fixture();
    let generation = f.state.snapshot().unwrap().auth_generation;
    let laptop = key(2);
    let (scope, r, result) = f.attempt(&laptop, f.owner, "  Work\u{202E}  laptop\n ");
    let signed = result.unwrap();
    assert!(signed.created);
    assert_eq!(signed.device().account_id, f.owner);
    assert_eq!(signed.device().name, "Work laptop");
    assert_eq!(f.state.snapshot().unwrap().auth_generation, generation + 1);
    assert_eq!(f.audits("device_sign_in"), 1);
    // The same proof cannot enroll again.
    assert_eq!(
        sign_in(&f.state, &mut f.accounts, &scope, FP, &r).err(),
        Some(IdentityFailure("pairing_failed"))
    );
    let mut sessions = HostSessions::new(f.state.clone()).unwrap();
    let session = sessions.signed_in(&signed).unwrap();
    assert_eq!(session.account_id, f.owner);
    assert_eq!(
        sessions.authenticate(&session.session).unwrap().device_id,
        signed.device().device_id
    );
    f.state
        .revoke_device(Actor::Account(f.owner), signed.device().device_id)
        .unwrap();
    assert_eq!(f.state.snapshot().unwrap().auth_generation, generation + 2);
    assert!(sessions.authenticate(&session.session).is_err());
    // A revoked key stays revoked; the account may sign in again with a new key.
    assert_eq!(
        f.attempt(&laptop, f.owner, "Laptop").2.err(),
        Some(KEY_UNUSABLE)
    );
    assert!(f.attempt(&key(3), f.owner, "Laptop").2.unwrap().created);
}

#[test]
fn machine_sign_in_server_role_enrolls_and_account_without_role_leaves_no_device() {
    let mut f = fixture();
    let player = Uuid::new_v4();
    let stranger = Uuid::new_v4();
    let server = Uuid::new_v4();
    f.state
        .write(|c| {
            c.execute(
                "INSERT INTO machine_accounts(account_id) VALUES(?1),(?2)",
                [player.to_string(), stranger.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    f.state
        .register_server(
            Actor::Account(f.owner),
            &ServerEntry {
                server_id: server,
                name: "Survival".into(),
                game_port: 25565,
                state: ServerState::Stopped,
                visibility: Visibility::Private,
            },
            f.state.snapshot().unwrap().revision,
        )
        .unwrap();
    f.state
        .set_server_role(
            Actor::Account(f.owner),
            server,
            player,
            Some(ServerRole::PackUser),
            f.state.snapshot().unwrap().revision,
        )
        .unwrap();
    let before = f.device_count();
    let generation = f.state.snapshot().unwrap().auth_generation;
    for account in [stranger, Uuid::new_v4()] {
        let (scope, r, result) = f.attempt(&key(4), account, "Phone");
        assert_eq!(result.err(), Some(NO_ACCESS));
        // The refusal consumed the proof.
        assert_eq!(
            sign_in(&f.state, &mut f.accounts, &scope, FP, &r).err(),
            Some(IdentityFailure("pairing_failed"))
        );
    }
    assert_eq!(f.device_count(), before);
    assert_eq!(f.state.snapshot().unwrap().auth_generation, generation);
    let signed = f.attempt(&key(4), player, "Phone").2.unwrap();
    assert_eq!(signed.device().account_id, player);
    assert_eq!(f.device_count(), before + 1);
    // An active key of the same account signs in again; another account cannot take it.
    assert!(!f.attempt(&key(4), player, "Phone").2.unwrap().created);
    assert_eq!(
        f.attempt(&key(4), f.owner, "Phone").2.err(),
        Some(KEY_UNUSABLE)
    );
}

#[test]
fn machine_sign_in_rejects_other_fingerprint_host_and_scope() {
    let mut f = fixture();
    let k = key(5);
    let host = f.host();
    let other = "b".repeat(64);
    let cases: Vec<(AccountScope, &str, Uuid, &str)> = vec![
        // Proof bound to a different certificate fingerprint.
        (scope(&k, &other), FP, host, FP),
        // Request signed for a different fingerprint or host id.
        (scope(&k, FP), FP, host, &other),
        (scope(&k, FP), FP, Uuid::new_v4(), FP),
        // Arriving on a connection with a different certificate.
        (scope(&k, FP), &other, host, FP),
    ];
    for (proof_scope, connection_fp, signed_host, signed_fp) in cases {
        let proof = Uuid::new_v4().to_string();
        f.accounts.seed_test_proof(&proof, &proof_scope, f.owner);
        let r = request(&k, &proof, "Laptop", signed_host, signed_fp);
        let mut used = scope(&k, connection_fp);
        used.connection_id = proof_scope.connection_id;
        assert_eq!(
            sign_in(&f.state, &mut f.accounts, &used, connection_fp, &r).err(),
            Some(IdentityFailure("pairing_failed"))
        );
    }
    // A proof from another connection, or for another purpose, does not apply.
    let proof = "other-connection";
    let mut claim_scope = scope(&k, FP);
    claim_scope.purpose = AccountPurpose::Claim;
    f.accounts.seed_test_proof(proof, &claim_scope, f.owner);
    f.accounts.seed_test_proof("other", &scope(&k, FP), f.owner);
    for id in [proof, "other"] {
        let r = request(&k, id, "Laptop", host, FP);
        assert!(sign_in(&f.state, &mut f.accounts, &scope(&k, FP), FP, &r).is_err());
    }
    assert_eq!(f.device_count(), 1);
}

#[test]
fn machine_sign_in_approval_mode_requires_link_approval() {
    let mut f = fixture();
    let player = Uuid::new_v4();
    f.state
        .write(|c| {
            c.execute(
                "INSERT INTO machine_accounts(account_id) VALUES(?1)",
                [player.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        f.state
            .update_settings(
                Actor::Account(player),
                HostSettingsPatch {
                    name: None,
                    new_device_sign_in: Some(NewDeviceSignIn::Approval),
                    removed_server_retention_days: None,
                    default_on_agent_start: None,
                }
            )
            .unwrap_err()
            .code(),
        "forbidden"
    );
    let settings = f
        .state
        .update_settings(
            Actor::Account(f.owner),
            HostSettingsPatch {
                name: None,
                new_device_sign_in: Some(NewDeviceSignIn::Approval),
                removed_server_retention_days: None,
                default_on_agent_start: None,
            },
        )
        .unwrap();
    assert_eq!(settings.new_device_sign_in, NewDeviceSignIn::Approval);
    assert_eq!(f.audits("new_device_sign_in_change"), 1);
    assert_eq!(
        f.attempt(&key(6), f.owner, "Laptop").2.err(),
        Some(APPROVAL_REQUIRED)
    );
    assert_eq!(f.device_count(), 1);
    // Already-enrolled keys are unaffected by the mode.
    assert!(!f.attempt(&key(1), f.owner, "Desk").2.unwrap().created);
    f.state
        .update_settings(
            Actor::Account(f.owner),
            HostSettingsPatch {
                name: None,
                new_device_sign_in: Some(NewDeviceSignIn::Account),
                removed_server_retention_days: None,
                default_on_agent_start: None,
            },
        )
        .unwrap();
    assert!(f.attempt(&key(6), f.owner, "Laptop").2.unwrap().created);
}

#[test]
fn machine_owners_list_names_and_rename_authorization() {
    let f = fixture();
    let second = Uuid::new_v4();
    f.state
        .write(|c| {
            HostState::enroll_device_in(
                c,
                second,
                "Other",
                key(7).public_key().as_ref().try_into().unwrap(),
                f.state.now_ms(),
            )
        })
        .unwrap();
    assert_eq!(
        f.state.owners(Actor::Account(second)).unwrap_err().code(),
        "forbidden"
    );
    f.state
        .set_owner(
            Actor::Account(f.owner),
            second,
            true,
            f.state.snapshot().unwrap().revision,
        )
        .unwrap();
    f.state
        .write(|c| super::owners::record_account_name_in(c, f.owner, "Steve", 1))
        .unwrap();
    let owners = f.state.owners(Actor::Account(second)).unwrap();
    assert_eq!(owners.len(), 2);
    assert_eq!(
        (
            owners[0].account_id,
            owners[0].role,
            owners[0].name.as_deref()
        ),
        (f.owner, HostRole::PrimaryOwner, Some("Steve"))
    );
    assert_eq!(
        (owners[1].role, owners[1].name.as_deref()),
        (HostRole::Owner, None)
    );

    let rename = |actor, name: &str| {
        f.state.update_settings(
            actor,
            HostSettingsPatch {
                name: Some(name.into()),
                new_device_sign_in: None,
                removed_server_retention_days: None,
                default_on_agent_start: None,
            },
        )
    };
    assert_eq!(f.state.settings().unwrap().name, None);
    assert_eq!(
        rename(Actor::Account(Uuid::new_v4()), "Nope")
            .unwrap_err()
            .code(),
        "forbidden"
    );
    for bad in ["", " \u{202E} ", &"x".repeat(64)] {
        assert_eq!(
            rename(Actor::Account(second), bad).unwrap_err().code(),
            "invalid_request"
        );
    }
    let revision = f.state.snapshot().unwrap().revision;
    let settings = rename(Actor::Account(second), "  Family\tserver ").unwrap();
    assert_eq!(settings.name.as_deref(), Some("Family server"));
    assert_eq!(f.state.snapshot().unwrap().revision, revision + 1);
    assert_eq!(f.audits("host_rename"), 1);
}

#[test]
fn machine_device_names_are_bounded_and_sanitised() {
    assert_eq!(device_name("\u{0007}\u{200B}").as_deref(), None);
    assert_eq!(device_name("a\u{0000}b  c").as_deref(), Some("ab c"));
    assert_eq!(
        device_name(&"\u{e9}".repeat(100)).unwrap().chars().count(),
        64
    );
    assert_eq!(device_name(&"x".repeat(257)), None);
    assert_eq!(machine_name(&"\u{e9}".repeat(31)).unwrap().len(), 62);
    assert_eq!(machine_name(&"\u{e9}".repeat(32)), None);
}

#[test]
fn machine_invite_redeems_only_on_its_server_and_replay_does_not_spend_a_use() {
    use super::invites::*;
    let mut f = fixture();
    let mut servers = Vec::new();
    for port in [32001, 32002] {
        let id = Uuid::new_v4();
        f.state
            .register_server(
                Actor::Account(f.owner),
                &ServerEntry {
                    server_id: id,
                    name: format!("S {port}"),
                    game_port: port,
                    state: ServerState::Stopped,
                    visibility: Visibility::Private,
                },
                f.state.snapshot().unwrap().revision,
            )
            .unwrap();
        servers.push(id);
    }
    let issued = issue(
        &f.state,
        Actor::Account(f.owner),
        servers[0],
        FP,
        CreateInvite {
            role: "moderator".into(),
            expires_minutes: 60,
            multi_use: false,
            max_uses: None,
        },
    )
    .unwrap();
    let invite = issued["invite"].as_str().unwrap();
    let (_, hash) = binding(invite).unwrap();
    let key = key(11);
    let account = Uuid::new_v4();
    let scope = AccountScope {
        connection_id: Uuid::new_v4(),
        device_key: B64.encode(key.public_key().as_ref()),
        purpose: AccountPurpose::Invite,
        binding: hash.clone(),
    };
    for proof in ["proof1", "proof2"] {
        f.accounts.seed_test_proof(proof, &scope, account);
        let signature=B64.encode(key.sign(&signed_bytes("vesta.host.invite.v1",&json!({"hostId":f.host(),"serverId":servers[0],"inviteHash":hash,"accountProofId":proof,"deviceKey":scope.device_key}))).as_ref());
        let request = RedeemInvite {
            invite: invite.into(),
            account_proof_id: proof.into(),
            device_key: scope.device_key.clone(),
            device_name: "Guest".into(),
            signature,
        };
        redeem(&f.state, &mut f.accounts, &scope, FP, &request).unwrap();
    }
    assert_eq!(
        f.state.effective_role(account, servers[0]).unwrap(),
        Some(super::owners::EffectiveRole::Server(ServerRole::Moderator))
    );
    assert_eq!(f.state.effective_role(account, servers[1]).unwrap(), None);
    assert!(f.state.host_role(account).unwrap().is_none());
    revoke(
        &f.state,
        Actor::Account(f.owner),
        servers[1],
        Uuid::parse_str(issued["id"].as_str().unwrap()).unwrap(),
    )
    .unwrap_err();
    revoke(
        &f.state,
        Actor::Account(f.owner),
        servers[0],
        Uuid::parse_str(issued["id"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert!(
        list(&f.state, Actor::Account(f.owner), servers[0]).unwrap()["invitations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
