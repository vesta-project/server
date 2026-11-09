use super::*;
use crate::identity::{
    BuiltinRole,
    management::{Enrolled, session_message},
    minecraft::{MinecraftUuid, ServerUuid, VerifiedMinecraftProfile},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use std::sync::Arc;
use vesta_state::{StateDirectory, SystemClock};

const INSTALLATION: &str = "0123456789abcdef0123456789abcdef";
struct Fixture {
    root: tempfile::TempDir,
    db: StateDatabase,
    setup: FirstServerSetup,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let db = StateDatabase::open(
            StateDirectory::new(root.path().join("state"), None).unwrap(),
            Arc::new(SystemClock),
        )
        .unwrap();
        let setup = FirstServerSetup::reserve_local(db.clone(), INSTALLATION).unwrap();
        Self { root, db, setup }
    }
    fn enroll(&self, role: BuiltinRole) -> (Enrolled, Ed25519KeyPair, String) {
        let key = Ed25519KeyPair::from_pkcs8(
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        let m = self.setup.management();
        let code = m.issue_local(role, 10).unwrap();
        let profile = VerifiedMinecraftProfile::for_test_enrollment(
            ServerUuid::parse(self.db.server_id()).unwrap(),
            MinecraftUuid::parse(&uuid::Uuid::new_v4().to_string()).unwrap(),
        );
        let device = m
            .enroll_minecraft_with_invitation(
                profile,
                "Owned setup fixture",
                &URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
                Some(code.secret.expose()),
            )
            .unwrap();
        let session = session(m, &device, &key);
        (device, key, session)
    }
}
fn session(m: &ManagementAccess, device: &Enrolled, key: &Ed25519KeyPair) -> String {
    let nonce = m.challenge(&device.device_id).unwrap();
    let signed = key.sign(&session_message(m.server_id(), nonce.expose()));
    m.session(
        &device.device_id,
        nonce.expose(),
        &URL_SAFE_NO_PAD.encode(signed.as_ref()),
    )
    .unwrap()
    .secret
    .expose()
    .to_owned()
}

#[test]
fn first_account_claim_is_idempotent_and_has_no_machine_authority() {
    let f = Fixture::new();
    let (_, _, token) = f.enroll(BuiltinRole::Owner);
    let before = f.setup.audit.page(0, 100).unwrap().len();
    let result = f.setup.claim(&token).unwrap();
    assert!(result.claimed);
    assert_eq!(result.server_id, f.db.server_id());
    assert_eq!(f.setup.claim(&token).unwrap(), result);
    assert_eq!(f.setup.claimed_status(&token).unwrap(), result);
    assert_eq!(f.setup.audit.page(0, 100).unwrap().len(), before + 1);
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 3);
    assert!(f.setup.with_owner(&token, |_, _, _| Ok(())).is_ok());
    assert!(
        FirstServerSetup::reserve_local(f.db.clone(), "fedcba9876543210fedcba9876543210").is_err()
    );
    assert!(f.setup.audit.verify_chain().unwrap().valid);
}

#[test]
fn other_accounts_lesser_roles_and_foreign_sessions_cannot_take_over() {
    let f = Fixture::new();
    let (_, _, first) = f.enroll(BuiltinRole::Owner);
    let (_, _, admin) = f.enroll(BuiltinRole::Administrator);
    assert_eq!(f.setup.claim(&admin), Err(PermissionError::Forbidden));
    f.setup.claim(&first).unwrap();
    let (_, _, other) = f.enroll(BuiltinRole::Owner);
    assert_eq!(f.setup.claim(&other), Err(PermissionError::Conflict));
    assert_eq!(
        f.setup.with_owner(&other, |_, _, _| Ok(())),
        Err(PermissionError::Forbidden)
    );
    let foreign = Fixture::new();
    let (_, _, token) = foreign.enroll(BuiltinRole::Owner);
    assert_eq!(f.setup.claim(&token), Err(PermissionError::SessionInvalid));
}

#[test]
fn revocation_or_account_unlink_refuses_execution_before_effect() {
    let f = Fixture::new();
    let (device, _, token) = f.enroll(BuiltinRole::Owner);
    f.setup.claim(&token).unwrap();
    f.db.write(|c| {
        c.execute(
            "UPDATE management_device_keys SET linked_minecraft_uuid=NULL WHERE device_id=?",
            [&device.device_id],
        )?;
        Ok(())
    })
    .unwrap();
    let effects = std::cell::Cell::new(0);
    assert_eq!(
        f.setup.with_owner(&token, |_, _, _| {
            effects.set(1);
            Ok(())
        }),
        Err(PermissionError::SessionInvalid)
    );
    assert_eq!(effects.get(), 0);
}

#[test]
fn claim_audit_failure_rolls_back_owner_binding() {
    let f = Fixture::new();
    let (_, _, token) = f.enroll(BuiltinRole::Owner);
    f.db.write(|c|{c.execute_batch("CREATE TRIGGER setup_audit_failure BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'owned injected audit failure'); END")?;Ok(())}).unwrap();
    assert_eq!(f.setup.claim(&token), Err(PermissionError::Unavailable));
    assert!(
        f.db.read(|c| Ok(c.query_row(
            "SELECT owner_principal IS NULL FROM first_server_setup",
            [],
            |r| r.get::<_, bool>(0)
        )?))
        .unwrap()
    );
    f.db.write(|c| {
        c.execute_batch("DROP TRIGGER setup_audit_failure")?;
        Ok(())
    })
    .unwrap();
    assert!(f.setup.claim(&token).unwrap().claimed);
}

#[test]
fn real_database_reopen_retains_claim_but_requires_new_session() {
    let Fixture { root, db, setup } = Fixture::new();
    let f = Fixture { root, db, setup };
    let (device, key, token) = f.enroll(BuiltinRole::Owner);
    let expected = f.setup.claim(&token).unwrap();
    let Fixture { root, db, setup } = f;
    drop(setup);
    db.close().unwrap();
    drop(db);
    let db = StateDatabase::open(
        StateDirectory::new(root.path().join("state"), None).unwrap(),
        Arc::new(SystemClock),
    )
    .unwrap();
    let setup = FirstServerSetup::reserve_local(db.clone(), INSTALLATION).unwrap();
    assert_eq!(setup.claim(&token), Err(PermissionError::SessionInvalid));
    let renewed = session(setup.management(), &device, &key);
    assert_eq!(setup.claim(&renewed).unwrap(), expected);
}

#[test]
fn concurrent_account_claims_choose_one_owner_and_replay_cannot_replace_it() {
    let f = Fixture::new();
    let (_, _, a) = f.enroll(BuiltinRole::Owner);
    let (_, _, b) = f.enroll(BuiltinRole::Owner);
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut threads = Vec::new();
    for token in [a.clone(), b.clone()] {
        let setup = f.setup.clone();
        let gate = barrier.clone();
        threads.push(std::thread::spawn(move || {
            gate.wait();
            setup.claim(&token)
        }));
    }
    barrier.wait();
    let results = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == Err(PermissionError::Conflict))
            .count(),
        1
    );
    assert_eq!(
        f.setup
            .audit
            .page(0, 100)
            .unwrap()
            .iter()
            .filter(|r| r.action == "setup.first_server.claim")
            .count(),
        1
    );
}

#[test]
fn revoked_session_and_removed_owner_role_do_not_admit_setup_mutations() {
    let f = Fixture::new();
    let (device, key, token) = f.enroll(BuiltinRole::Owner);
    f.setup.claim(&token).unwrap();
    assert!(f.setup.management().revoke_session(&token).unwrap());
    assert_eq!(
        f.setup.claimed_status(&token),
        Err(PermissionError::SessionInvalid)
    );
    let next = session(f.setup.management(), &device, &key);
    f.db.write(|c| {
        c.execute(
            "UPDATE role_assignments SET revoked_at=1 WHERE principal_id=? AND role_id='owner'",
            [&device.principal_id],
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        f.setup.claimed_status(&next),
        Err(PermissionError::Forbidden)
    );
}
