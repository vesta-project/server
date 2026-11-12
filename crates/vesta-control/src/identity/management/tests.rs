use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::sync::{
    Barrier,
    atomic::{AtomicI64, Ordering},
};
use vesta_state::{Clock, StateDirectory};
struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    db: StateDatabase,
    service: ManagementAccess,
    clock: Arc<TestClock>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(1_791_115_200_000)));
        let db = StateDatabase::open(
            StateDirectory::new(temp.path().join("state"), None).unwrap(),
            clock.clone(),
        )
        .unwrap();
        let service = ManagementAccess::new(db.clone()).unwrap();
        Self {
            _temp: temp,
            db,
            service,
            clock,
        }
    }
    fn enroll(&self, role: BuiltinRole) -> (Enrolled, Ed25519KeyPair) {
        let key = key();
        let code = self.service.issue_local(role, 10).unwrap();
        let device = self
            .service
            .enroll_minecraft_with_invitation(
                profile(self, &uuid::Uuid::new_v4().to_string()),
                "Test device",
                &URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
                Some(code.secret.expose()),
            )
            .unwrap();
        (device, key)
    }
    fn session(&self, device: &Enrolled, key: &Ed25519KeyPair) -> SessionToken {
        let nonce = self.service.challenge(&device.device_id).unwrap();
        let sig = key.sign(&session_message(self.service.server_id(), nonce.expose()));
        self.service
            .session(
                &device.device_id,
                nonce.expose(),
                &URL_SAFE_NO_PAD.encode(sig.as_ref()),
            )
            .unwrap()
    }
}
fn key() -> Ed25519KeyPair {
    Ed25519KeyPair::from_pkcs8(
        Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .unwrap()
            .as_ref(),
    )
    .unwrap()
}
#[test]
fn pair_signature_session_revoke_and_success_audit() {
    let f = Fixture::new();
    let (device, key) = f.enroll(BuiltinRole::Owner);
    let token = f.session(&device, &key);
    assert_eq!(
        f.service
            .authorize(
                token.secret.expose(),
                super::super::Capability::StatusView,
                None
            )
            .unwrap()
            .device
            .as_str(),
        device.device_id
    );
    assert_eq!(f.service.list_local().unwrap().len(), 1);
    f.service.revoke_local(&device.device_id).unwrap();
    assert_eq!(
        f.service.authenticate(token.secret.expose()),
        Err(ManagementError::SessionInvalid)
    );
    assert!(f.service.list_local().unwrap()[0].revoked);
    let audit = f.service.audit.page(0, 100).unwrap();
    let actions: Vec<_> = audit.iter().map(|a| a.action.as_str()).collect();
    for action in [
        "management.pair.issue",
        "management.minecraft.enroll",
        "management.session.issue",
        "management.device.revoke",
    ] {
        assert!(actions.contains(&action));
    }
    assert!(f.service.audit.verify_chain().unwrap().valid);
}

#[test]
fn console_fragment_index_follows_live_credentials_and_releases_memory() {
    let f = Fixture::new();
    let (device, key) = f.enroll(BuiltinRole::Owner);
    let nonce = f.service.challenge(&device.device_id).unwrap();
    assert_eq!(
        f.service
            .redact_console_secret_edges(&format!("nonce: {}", &nonce.expose()[..21]))
            .unwrap(),
        "nonce: [redacted]"
    );
    let signature = key.sign(&session_message(f.service.server_id(), nonce.expose()));
    let token = f
        .service
        .session(
            &device.device_id,
            nonce.expose(),
            &URL_SAFE_NO_PAD.encode(signature.as_ref()),
        )
        .unwrap();
    assert_eq!(
        f.service
            .redact_console_secret_edges(&format!("token: {}", &token.secret.expose()[..21]))
            .unwrap(),
        "token: [redacted]"
    );
    assert!(f.service.revoke_session(token.secret.expose()).unwrap());
    let stores = f.service.stores.lock().unwrap();
    assert!(stores.sessions.is_empty());
    assert!(stores.challenges.is_empty());
    // No credential history accumulates when sessions are consumed/revoked.
    assert_eq!(
        stores
            .console_credentials
            .redact(&token.secret.expose()[..21]),
        &token.secret.expose()[..21]
    );
}
#[test]
fn concurrent_pair_redemption_is_single_use() {
    let f = Fixture::new();
    let code = f
        .service
        .issue_local(BuiltinRole::Administrator, 10)
        .unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let service = f.service.clone();
        let code = code.secret.expose().to_owned();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            let key = key();
            barrier.wait();
            service.enroll(
                &code,
                "Concurrent",
                &URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
            )
        }));
    }
    barrier.wait();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(ManagementError::PairInvalid)))
            .count(),
        1
    );
    assert_eq!(f.service.list_local().unwrap().len(), 1);
}
#[test]
fn wrong_expired_used_and_server_bound_codes_are_fixed_refusals() {
    let f = Fixture::new();
    let key = URL_SAFE_NO_PAD.encode(key().public_key().as_ref());
    let wrong = random_secret().unwrap();
    assert!(matches!(
        f.service.enroll(wrong.expose(), "Wrong", &key),
        Err(ManagementError::PairInvalid)
    ));
    assert!(matches!(
        f.service.enroll("malformed", "Wrong", &key),
        Err(ManagementError::PairInvalid)
    ));
    let code = f.service.issue_local(BuiltinRole::Owner, 1).unwrap();
    f.clock.0.fetch_add(60_000, Ordering::SeqCst);
    assert!(matches!(
        f.service.enroll(code.secret.expose(), "Expired", &key),
        Err(ManagementError::PairInvalid)
    ));
    let code = f.service.issue_local(BuiltinRole::Owner, 1).unwrap();
    let other = Fixture::new();
    assert!(matches!(
        other.service.enroll(code.secret.expose(), "Foreign", &key),
        Err(ManagementError::PairInvalid)
    ));
    f.service
        .enroll(code.secret.expose(), "Used", &key)
        .unwrap();
    assert!(matches!(
        f.service.enroll(code.secret.expose(), "Used", &key),
        Err(ManagementError::PairInvalid)
    ));
}
#[test]
fn wrong_server_purpose_key_and_replayed_or_expired_nonce_refuse() {
    let f = Fixture::new();
    let (device, key) = f.enroll(BuiltinRole::Owner);
    for kind in ["server", "purpose", "key"] {
        let nonce = f.service.challenge(&device.device_id).unwrap();
        let message = match kind {
            "server" => session_message("wrong-server", nonce.expose()),
            "purpose" => format!(
                "pack.download\0{}\0{}",
                f.service.server_id(),
                nonce.expose()
            )
            .into_bytes(),
            _ => session_message(f.service.server_id(), nonce.expose()),
        };
        let wrong_key = super::tests::key();
        let sig = if kind == "key" {
            wrong_key.sign(&message)
        } else {
            key.sign(&message)
        };
        assert!(matches!(
            f.service.session(
                &device.device_id,
                nonce.expose(),
                &URL_SAFE_NO_PAD.encode(sig.as_ref())
            ),
            Err(ManagementError::SessionInvalid)
        ));
        let good = key.sign(&session_message(f.service.server_id(), nonce.expose()));
        assert!(matches!(
            f.service.session(
                &device.device_id,
                nonce.expose(),
                &URL_SAFE_NO_PAD.encode(good.as_ref())
            ),
            Err(ManagementError::SessionInvalid)
        ));
    }
    let nonce = f.service.challenge(&device.device_id).unwrap();
    let signature = URL_SAFE_NO_PAD.encode(
        key.sign(&session_message(f.service.server_id(), nonce.expose()))
            .as_ref(),
    );
    f.service
        .session(&device.device_id, nonce.expose(), &signature)
        .unwrap();
    assert!(matches!(
        f.service
            .session(&device.device_id, nonce.expose(), &signature),
        Err(ManagementError::SessionInvalid)
    ));
    let nonce = f.service.challenge(&device.device_id).unwrap();
    f.clock.0.fetch_add(CHALLENGE_TTL_MS, Ordering::SeqCst);
    assert!(matches!(
        f.service
            .session(&device.device_id, nonce.expose(), &signature),
        Err(ManagementError::SessionInvalid)
    ));
}
#[test]
fn role_expiry_revocation_and_server_mismatch_rechecked() {
    let f = Fixture::new();
    let (device, key) = f.enroll(BuiltinRole::Owner);
    let token = f.session(&device, &key);
    f.db.write(|c| {
        c.execute(
            "UPDATE role_assignments SET expires_at = ? WHERE principal_id = ?",
            params![f.clock.millis() + 1, device.principal_id],
        )?;
        Ok(())
    })
    .unwrap();
    f.clock.0.fetch_add(1, Ordering::SeqCst);
    assert!(matches!(
        f.service.authorize(
            token.secret.expose(),
            super::super::Capability::StatusView,
            None
        ),
        Err(ManagementError::Forbidden)
    ));
    // Roleless principals still authenticate for the eventual unified pack domain.
    assert!(f.service.authenticate(token.secret.expose()).is_ok());
    f.db.write(|c| {
        c.execute("UPDATE role_assignments SET expires_at = NULL", [])?;
        c.execute(
            "UPDATE management_device_keys SET server_id = ?",
            [f.service.server_id()],
        )?;
        Ok(())
    })
    .unwrap();
    let hash = bound_hash(b"session", f.service.server_id(), token.secret.expose());
    f.service
        .stores
        .lock()
        .unwrap()
        .sessions
        .get_mut(&hash)
        .unwrap()
        .server = "foreign".into();
    assert!(matches!(
        f.service.authenticate(token.secret.expose()),
        Err(ManagementError::SessionInvalid)
    ));
}
#[test]
fn stores_are_bounded_expiry_frees_capacity_and_secrets_are_hash_only() {
    let f = Fixture::new();
    for minutes in [0, 61, u16::MAX] {
        assert!(matches!(
            f.service.issue_local(BuiltinRole::Owner, minutes),
            Err(ManagementError::InvalidRequest)
        ));
    }
    let (device, key) = f.enroll(BuiltinRole::Owner);
    for _ in 0..MAX_CODES {
        f.service.issue_local(BuiltinRole::Viewer, 1).unwrap();
    }
    assert!(matches!(
        f.service.issue_local(BuiltinRole::Owner, 1),
        Err(ManagementError::Saturated)
    ));
    f.clock.0.fetch_add(60_000, Ordering::SeqCst);
    let code = f.service.issue_local(BuiltinRole::Owner, 1).unwrap();
    f.db.read(|c| {
        let (hash, count): (Vec<u8>, u32) = c.query_row("SELECT code_hash, (SELECT count(*) FROM management_pairing_codes) FROM management_pairing_codes", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        assert_eq!(hash.len(), 32); assert_eq!(count, 1); assert_ne!(hash, code.secret.expose().as_bytes()); Ok(())
    }).unwrap();
    let subject = Subject {
        principal: PrincipalId::parse(&device.principal_id).unwrap(),
        device: DeviceId::parse(&device.device_id).unwrap(),
    };
    {
        let mut stores = f.service.stores.lock().unwrap();
        for n in 0..MAX_CHALLENGES {
            stores.challenges.insert(
                bound_hash(b"fixture", f.service.server_id(), &n.to_string()),
                Challenge {
                    console_edges: Box::new([]),
                    device: subject.device.clone(),
                    expires: f.clock.millis() + 1,
                    monotonic_expires: std::time::Instant::now()
                        + std::time::Duration::from_secs(60),
                },
            );
        }
    }
    assert!(matches!(
        f.service.challenge(&device.device_id),
        Err(ManagementError::Saturated)
    ));
    f.clock.0.fetch_add(1, Ordering::SeqCst);
    let nonce = f.service.challenge(&device.device_id).unwrap();
    {
        let mut stores = f.service.stores.lock().unwrap();
        for n in 0..MAX_SESSIONS {
            stores.sessions.insert(
                bound_hash(b"fixture", f.service.server_id(), &n.to_string()),
                Session {
                    console_edges: Box::new([]),
                    subject: subject.clone(),
                    server: f.service.server_id().into(),
                    expires: f.clock.millis() + 1,
                    monotonic_expires: std::time::Instant::now()
                        + std::time::Duration::from_secs(60),
                },
            );
        }
    }
    let signature = URL_SAFE_NO_PAD.encode(
        key.sign(&session_message(f.service.server_id(), nonce.expose()))
            .as_ref(),
    );
    assert!(matches!(
        f.service
            .session(&device.device_id, nonce.expose(), &signature),
        Err(ManagementError::Saturated)
    ));
    f.clock.0.fetch_add(1, Ordering::SeqCst);
    let token = f.session(&device, &key);
    assert_eq!(f.service.stores.lock().unwrap().sessions.len(), 1);
    f.clock.0.fetch_add(SESSION_TTL_MS, Ordering::SeqCst);
    assert!(matches!(
        f.service.authenticate(token.secret.expose()),
        Err(ManagementError::SessionInvalid)
    ));
}
#[test]
fn enrollment_and_revocation_roll_back_with_failed_audit() {
    let f = Fixture::new();
    let code = f.service.issue_local(BuiltinRole::Owner, 10).unwrap();
    let key = URL_SAFE_NO_PAD.encode(key().public_key().as_ref());
    f.db.write(|c| { c.execute_batch("CREATE TRIGGER fail_management_audit BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'fixture'); END;")?; Ok(()) }).unwrap();
    assert!(matches!(
        f.service.enroll(code.secret.expose(), "Failed", &key),
        Err(ManagementError::Unavailable)
    ));
    assert!(f.service.list_local().unwrap().is_empty());
    f.db.write(|c| {
        c.execute_batch("DROP TRIGGER fail_management_audit;")?;
        Ok(())
    })
    .unwrap();
    let device = f
        .service
        .enroll(code.secret.expose(), "Retried", &key)
        .unwrap();
    f.db.write(|c| { c.execute_batch("CREATE TRIGGER fail_management_audit BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'fixture'); END;")?; Ok(()) }).unwrap();
    assert!(matches!(
        f.service.revoke_local(&device.device_id),
        Err(ManagementError::Unavailable)
    ));
    assert!(!f.service.list_local().unwrap()[0].revoked);
}

use crate::{
    config::PackAccessMode,
    identity::{
        Capability,
        minecraft::{MinecraftUuid, ServerUuid, VerifiedMinecraftProfile},
    },
    minecraft_admission::{BanList, ServerProperties},
    pack_access::{PackAccessPolicy, PackGrant},
    whitelist::Whitelist,
};
fn profile(f: &Fixture, uuid: &str) -> VerifiedMinecraftProfile {
    VerifiedMinecraftProfile::for_test_enrollment(
        ServerUuid::parse(f.service.server_id()).unwrap(),
        MinecraftUuid::parse(uuid).unwrap(),
    )
}
const USER_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";
fn normal(f: &Fixture) -> (Enrolled, Ed25519KeyPair) {
    let key = key();
    let enrolled = f
        .service
        .enroll_minecraft(
            profile(f, &uuid::Uuid::new_v4().to_string()),
            "Ordinary user",
            &URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
        )
        .unwrap();
    (enrolled, key)
}
fn policy<'a>(f: &Fixture, whitelist: &'a Whitelist, bans: &'a BanList) -> PackAccessPolicy<'a> {
    PackAccessPolicy {
        server: ServerUuid::parse(f.service.server_id()).unwrap(),
        mode: PackAccessMode::MinecraftOrPassword,
        properties: Some(ServerProperties::parse(b"online-mode=true\nwhite-list=false\n").unwrap()),
        whitelist,
        bans,
        password: None,
    }
}
#[test]
fn canonical_uuid_devices_pairing_and_authenticated_method_attachment() {
    let f = Fixture::new();
    let k = key();
    let public = URL_SAFE_NO_PAD.encode(k.public_key().as_ref());
    let first = f
        .service
        .enroll_minecraft(profile(&f, USER_UUID), "User", &public)
        .unwrap();
    let token = f.session(&first, &k);
    let linked = f
        .service
        .enroll_minecraft_for(
            profile(&f, USER_UUID),
            "User",
            &public,
            Some(token.secret.expose()),
        )
        .unwrap();
    assert_eq!(linked.principal_id, first.principal_id);
    assert_eq!(linked.device_id, first.device_id);
    let second_key = key();
    let second = f
        .service
        .enroll_minecraft(
            profile(&f, USER_UUID),
            "Second",
            &URL_SAFE_NO_PAD.encode(second_key.public_key().as_ref()),
        )
        .unwrap();
    assert_eq!(second.principal_id, first.principal_id);
    assert_ne!(second.device_id, first.device_id);
    let second_token = f.session(&second, &second_key);
    let code = f.service.issue_local(BuiltinRole::Owner, 10).unwrap();
    assert!(matches!(
        f.service.enroll(
            code.secret.expose(),
            "Second",
            &URL_SAFE_NO_PAD.encode(second_key.public_key().as_ref())
        ),
        Err(ManagementError::SessionInvalid)
    ));
    let paired = f
        .service
        .enroll_for(
            code.secret.expose(),
            "Second",
            &URL_SAFE_NO_PAD.encode(second_key.public_key().as_ref()),
            Some(second_token.secret.expose()),
        )
        .unwrap();
    assert_eq!(paired.principal_id, first.principal_id);
    assert!(
        f.service
            .authorize(token.secret.expose(), Capability::SecurityManage, None)
            .is_ok()
    );
    let (stranger, sk) = normal(&f);
    let stranger_token = f.session(&stranger, &sk);
    assert!(matches!(
        f.service.enroll_minecraft_for(
            profile(&f, USER_UUID),
            "Other",
            &URL_SAFE_NO_PAD.encode(sk.public_key().as_ref()),
            Some(stranger_token.secret.expose())
        ),
        Err(ManagementError::SessionInvalid)
    ));
    assert!(matches!(
        f.service.enroll_minecraft_for(
            profile(&f, "123e4567-e89b-42d3-a456-426614174000"),
            "Other",
            &public,
            Some(token.secret.expose())
        ),
        Err(ManagementError::SessionInvalid)
    ));
    f.db.read(|c| {
        let count: u32 = c.query_row("SELECT count(*) FROM principals", [], |r| r.get(0))?;
        assert_eq!(count, 2);
        Ok(())
    })
    .unwrap();
}
#[test]
fn all_automatic_sources_refuse_management_roles_and_audit_rolls_back() {
    let f = Fixture::new();
    let before = f.service.access_local().unwrap();
    for field in ["anonymous", "new_user", "whitelist", "password"] {
        assert!(matches!(
            f.service
                .patch_access_local(&[(field.into(), "owner".into())]),
            Err(ManagementError::InvalidRequest)
        ));
        assert!(matches!(
            f.service
                .patch_access_local(&[(field.into(), "pack-user".into())]),
            Err(ManagementError::InvalidRequest)
        ));
        assert_eq!(f.service.access_local().unwrap(), before);
    }
    f.db.write(|c| {c.execute_batch("INSERT INTO roles VALUES ('unsafe-auto','Unsafe',5,0,'fixture',0); INSERT INTO role_capabilities VALUES ('unsafe-auto','status.view','*');")?;Ok(())}).unwrap();
    assert!(matches!(
        f.service
            .patch_access_local(&[("anonymous".into(), "unsafe-auto".into())]),
        Err(ManagementError::InvalidRequest)
    ));
    f.db.write(|c| {c.execute_batch("CREATE TRIGGER access_audit_failure BEFORE INSERT ON audit_events WHEN NEW.action='access.sources.set' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;Ok(())}).unwrap();
    assert!(
        f.service
            .patch_access_local(&[("anonymous".into(), "player".into())])
            .is_err()
    );
    assert_eq!(f.service.access_local().unwrap(), before);
}
#[test]
fn anonymous_new_user_live_whitelist_and_explicit_roles_are_separate_sources() {
    let f = Fixture::new();
    let whitelist = Whitelist::parse(format!("[{{\"uuid\":\"{USER_UUID}\"}}]").as_bytes()).unwrap();
    let empty = Whitelist::default();
    let bans = BanList::default();
    let p = policy(&f, &whitelist, &bans);
    let now = std::time::Instant::now();
    let anonymous = f.service.pack_reference(None).unwrap();
    assert!(
        f.service
            .authorize_pack(anonymous, Capability::PublicInfo, now, &p)
            .is_ok()
    );
    assert!(matches!(
        f.service
            .authorize_pack(anonymous, Capability::PackDownload, now, &p),
        Err(ManagementError::Forbidden)
    ));
    let key = key();
    let user = f
        .service
        .enroll_minecraft(
            profile(&f, USER_UUID),
            "User",
            &URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
        )
        .unwrap();
    let token = f.session(&user, &key);
    let reference = f
        .service
        .pack_reference(Some(token.secret.expose()))
        .unwrap();
    assert!(
        f.service
            .authorize_pack(reference, Capability::PublicInfo, now, &p)
            .is_ok()
    );
    assert!(matches!(
        f.service.authorize_pack(
            reference,
            Capability::PackDownload,
            now,
            &policy(&f, &empty, &bans)
        ),
        Err(ManagementError::Forbidden)
    ));
    f.service
        .enroll_minecraft_for(
            profile(&f, USER_UUID),
            "User",
            &URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
            Some(token.secret.expose()),
        )
        .unwrap();
    assert!(
        f.service
            .authorize_pack(reference, Capability::PackDownload, now, &p)
            .is_ok()
    );
    assert!(
        f.service
            .authorize_pack(reference, Capability::PackRealtime, now, &p)
            .is_ok()
    );
    assert!(matches!(
        f.service.authorize_pack(
            reference,
            Capability::PackDownload,
            now,
            &policy(&f, &empty, &bans)
        ),
        Err(ManagementError::Forbidden)
    ));
    let banned = BanList::parse(
        format!("[{{\"uuid\":\"{USER_UUID}\",\"expires\":\"forever\"}}]").as_bytes(),
    )
    .unwrap();
    assert!(matches!(
        f.service.authorize_pack(
            reference,
            Capability::PackDownload,
            now,
            &policy(&f, &whitelist, &banned)
        ),
        Err(ManagementError::Forbidden)
    ));
    f.service
        .patch_access_local(&[("whitelist_enabled".into(), "off".into())])
        .unwrap();
    assert!(matches!(
        f.service
            .authorize_pack(reference, Capability::PackRealtime, now, &p),
        Err(ManagementError::Forbidden)
    ));
    f.service
        .patch_access_local(&[
            ("anonymous".into(), "player".into()),
            ("new_user".into(), "player".into()),
        ])
        .unwrap();
    assert!(
        f.service
            .authorize_pack(anonymous, Capability::PackDownload, now, &p)
            .is_ok()
    );
    let (new, k) = normal(&f);
    let new_session = f.session(&new, &k);
    let new_ref = f
        .service
        .pack_reference(Some(new_session.secret.expose()))
        .unwrap();
    assert!(
        f.service
            .authorize_pack(new_ref, Capability::PackRealtime, now, &p)
            .is_ok()
    );
    assert!(matches!(
        f.service
            .authorize(new_session.secret.expose(), Capability::StatusView, None),
        Err(ManagementError::Forbidden)
    ));
    f.service
        .link_local(MinecraftUuid::parse(USER_UUID).unwrap(), BuiltinRole::Owner)
        .unwrap();
    assert!(
        f.service
            .authorize(token.secret.expose(), Capability::SecurityManage, None)
            .is_ok()
    );
    assert!(
        f.service
            .authorize_pack(
                reference,
                Capability::PackRealtime,
                now,
                &policy(&f, &empty, &bans)
            )
            .is_ok()
    );
}
#[test]
fn password_grants_are_device_bound_and_rotation_cannot_resurrect_them() {
    let f = Fixture::new();
    let revision = crate::identity::PasswordRevision::new(1).unwrap();
    let (secret, _) = f
        .service
        .test_pack_session(PackGrant::Password(revision), std::time::Instant::now())
        .unwrap();
    let subject = f.service.authenticate(secret.expose()).unwrap();
    let account_uuid: String =
        f.db.read(|c| {
            Ok(c.query_row(
                "SELECT minecraft_uuid FROM user_minecraft_accounts WHERE principal_id=?",
                [subject.principal.as_str()],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let key = key();
    let second = f
        .service
        .enroll_minecraft_for(
            profile(&f, &account_uuid),
            "Other device",
            &URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
            Some(secret.expose()),
        )
        .unwrap();
    assert_eq!(second.principal_id, subject.principal.as_str());
    let second_token = f.session(&second, &key);
    let w = Whitelist::default();
    let b = BanList::default();
    let mut p = policy(&f, &w, &b);
    p.password = Some(revision);
    let now = std::time::Instant::now();
    let reference = f.service.pack_reference(Some(secret.expose())).unwrap();
    let other = f
        .service
        .pack_reference(Some(second_token.secret.expose()))
        .unwrap();
    assert!(
        f.service
            .authorize_pack(reference, Capability::PackDownload, now, &p)
            .is_ok()
    );
    assert!(matches!(
        f.service
            .authorize_pack(other, Capability::PackDownload, now, &p),
        Err(ManagementError::Forbidden)
    ));
    assert!(matches!(
        f.service
            .authorize(secret.expose(), Capability::StatusView, None),
        Err(ManagementError::Forbidden)
    ));
    f.db.write(|c| {
        c.execute("UPDATE pack_passwords SET revision=2", [])?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        f.service
            .authorize_pack(reference, Capability::PackDownload, now, &p),
        Err(ManagementError::Forbidden)
    ));
    f.db.read(|c| {
        let count: u32 = c.query_row("SELECT count(*) FROM device_password_grants", [], |r| {
            r.get(0)
        })?;
        assert_eq!(count, 0);
        Ok(())
    })
    .unwrap();
    f.db.write(|c| {
        c.execute("UPDATE pack_passwords SET revision=1", [])?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        f.service
            .authorize_pack(reference, Capability::PackDownload, now, &p),
        Err(ManagementError::Forbidden)
    ));
    assert!(f.service.authenticate(secret.expose()).is_ok());
}
#[test]
fn unlink_revoke_removes_user_link_authority_and_future_enrollment_stays_unprivileged() {
    let f = Fixture::new();
    let uuid = MinecraftUuid::parse(USER_UUID).unwrap();
    f.service.link_local(uuid, BuiltinRole::Owner).unwrap();
    let k = key();
    let first = f
        .service
        .enroll_minecraft(
            profile(&f, USER_UUID),
            "Linked",
            &URL_SAFE_NO_PAD.encode(k.public_key().as_ref()),
        )
        .unwrap();
    let token = f.session(&first, &k);
    let added = key();
    let second = f
        .service
        .enroll_minecraft_for(
            profile(&f, USER_UUID),
            "Attached",
            &URL_SAFE_NO_PAD.encode(added.public_key().as_ref()),
            Some(token.secret.expose()),
        )
        .unwrap();
    let second_token = f.session(&second, &added);
    f.service.unlink_local(uuid, false).unwrap();
    assert!(
        f.service
            .authorize(token.secret.expose(), Capability::SecurityManage, None)
            .is_ok()
    );
    // Revoking the only remaining top-role holder is refused and changes nothing.
    assert!(f.service.unlink_local(uuid, true).is_err());
    assert!(f.service.authenticate(token.secret.expose()).is_ok());
    // With a second top-role holder, the linked user's devices and link role go.
    let other = MinecraftUuid::parse("6ba7b810-9dad-11d1-80b4-00c04fd430c8").unwrap();
    f.service.link_local(other, BuiltinRole::Owner).unwrap();
    let other_key = key();
    f.service
        .enroll_minecraft(
            profile(&f, "6ba7b810-9dad-11d1-80b4-00c04fd430c8"),
            "Second owner",
            &URL_SAFE_NO_PAD.encode(other_key.public_key().as_ref()),
        )
        .unwrap();
    f.service.unlink_local(uuid, true).unwrap();
    assert!(matches!(
        f.service.authenticate(token.secret.expose()),
        Err(ManagementError::SessionInvalid)
    ));
    assert!(matches!(
        f.service.authenticate(second_token.secret.expose()),
        Err(ManagementError::SessionInvalid)
    ));
    let third = key();
    let third_device = f
        .service
        .enroll_minecraft(
            profile(&f, USER_UUID),
            "Future",
            &URL_SAFE_NO_PAD.encode(third.public_key().as_ref()),
        )
        .unwrap();
    assert_eq!(third_device.principal_id, first.principal_id);
    let third_token = f.session(&third_device, &third);
    assert!(matches!(
        f.service
            .authorize(third_token.secret.expose(), Capability::StatusView, None),
        Err(ManagementError::Forbidden)
    ));
}

#[test]
fn durable_device_enrollment_is_bounded_without_eviction() {
    let f = Fixture::new();
    let (first, k) = normal(&f);
    f.db.write(|c| {
        for n in 0..MAX_PUBLIC_DEVICE_KEYS-1 {
            let id=format!("11111111-2222-4333-8444-{n:012x}");
            c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Bounded fixture',?,?)",params![id,f.service.server_id(),first.principal_id,format!("{n:064x}"),f.clock.millis()])?;
            c.execute("INSERT INTO management_device_keys (device_id,server_id,public_key) VALUES (?,?,?)",params![id,f.service.server_id(),[0u8;32].as_slice()])?;
        } Ok(())
    }).unwrap();
    let new = key();
    assert!(matches!(
        f.service.enroll_device(
            "Beyond bound",
            &URL_SAFE_NO_PAD.encode(new.public_key().as_ref())
        ),
        Err(ManagementError::Saturated)
    ));
    assert_eq!(
        f.service
            .enroll_device("Existing", &URL_SAFE_NO_PAD.encode(k.public_key().as_ref()))
            .unwrap()
            .device_id,
        first.device_id
    );
    // Untrusted public registration cannot exhaust the protected owner's
    // reserved recovery slots. This still uses a real code and signed session.
    let (owner, owner_key) = f.enroll(BuiltinRole::Owner);
    assert!(
        f.service
            .authorize(
                f.session(&owner, &owner_key).secret.expose(),
                Capability::SecurityManage,
                None
            )
            .is_ok()
    );
    assert!(f.service.challenge(&first.device_id).is_ok());
}

#[test]
fn unsafe_automatic_role_edits_fail_closed_and_the_local_owner_can_repair_selection() {
    let f = Fixture::new();
    f.db.write(|c| {c.execute_batch("INSERT INTO roles VALUES ('safe-auto','Safe',5,0,'fixture',0); INSERT INTO role_capabilities VALUES ('safe-auto','public.info','*');")?;Ok(())}).unwrap();
    f.service
        .patch_access_local(&[("anonymous".into(), "safe-auto".into())])
        .unwrap();
    f.db.write(|c| {
        c.execute(
            "INSERT INTO role_capabilities VALUES ('safe-auto','status.view','*')",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    let w = Whitelist::default();
    let b = BanList::default();
    let p = policy(&f, &w, &b);
    let r = f.service.pack_reference(None).unwrap();
    assert!(matches!(
        f.service
            .authorize_pack(r, Capability::PublicInfo, std::time::Instant::now(), &p),
        Err(ManagementError::InvalidRequest)
    ));
    assert_eq!(f.service.access_local().unwrap().anonymous, "safe-auto");
    f.service
        .patch_access_local(&[("anonymous".into(), "public".into())])
        .unwrap();
    assert!(
        f.service
            .authorize_pack(r, Capability::PublicInfo, std::time::Instant::now(), &p)
            .is_ok()
    );
}

#[path = "permissions_tests.rs"]
mod permissions_tests;

#[test]
fn automation_and_notifications_defaults_are_management_not_automatic_access() {
    use crate::identity::Capability;
    let f = Fixture::new();
    let capabilities = [
        Capability::AutomationView,
        Capability::AutomationManage,
        Capability::NotificationsView,
    ];
    for capability in capabilities {
        assert_eq!(Capability::from_id(capability.id()), Some(capability));
        assert!(!super::access::safe(capability));
        assert!(!capability.scoped());
        assert!(capability.delegable());
    }
    for role in BuiltinRole::ALL {
        let (device, key) = f.enroll(*role);
        let token = f.session(&device, &key);
        for capability in capabilities {
            let expected = matches!(role, BuiltinRole::Owner | BuiltinRole::Administrator)
                || (*role == BuiltinRole::Moderator && capability == Capability::NotificationsView);
            assert_eq!(
                f.service
                    .authorize(token.secret.expose(), capability, None)
                    .is_ok(),
                expected,
                "{} {}",
                role.id(),
                capability.id()
            );
        }
    }
    let before = f.service.access_local().unwrap();
    for (index, capability) in capabilities.iter().enumerate() {
        let role_id = format!("automation-auto-{index}");
        f.db.write(|c| {
            c.execute(
                "INSERT INTO roles VALUES(?,?,5,0,'fixture',0)",
                params![role_id, role_id],
            )?;
            c.execute(
                "INSERT INTO role_capabilities VALUES(?,'public.info','*')",
                [&role_id],
            )?;
            c.execute(
                "INSERT INTO role_capabilities VALUES(?,?,'*')",
                params![role_id, capability.id()],
            )?;
            Ok(())
        })
        .unwrap();
        for field in ["anonymous", "new_user", "whitelist", "password"] {
            assert!(
                matches!(
                    f.service
                        .patch_access_local(&[(field.into(), role_id.clone())]),
                    Err(ManagementError::InvalidRequest)
                ),
                "{field} {}",
                capability.id()
            );
            assert_eq!(f.service.access_local().unwrap(), before);
        }
    }
}

#[test]
fn self_projection_is_live_deny_first_expiry_aware_and_has_no_audit() {
    let f = Fixture::new();
    let (device, key) = f.enroll(BuiltinRole::Viewer);
    let token = f.session(&device, &key);
    let whitelist = Whitelist::default();
    let bans = BanList::default();
    let access = policy(&f, &whitelist, &bans);
    let audit = || {
        f.db.read(|c| {
            Ok(c.query_row("SELECT count(*) FROM audit_events", [], |r| {
                r.get::<_, i64>(0)
            })?)
        })
        .unwrap()
    };
    let before = audit();
    let first = f
        .service
        .session_projection(token.secret.expose(), &access)
        .unwrap();
    assert_eq!(first["principal_id"], device.principal_id);
    assert_eq!(first["device_id"], device.device_id);
    assert_eq!(first["session_expires_at_ms"], token.expires_at_ms);
    assert!(
        first["roles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "viewer")
    );
    assert!(
        first["capabilities"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("status.view"))
    );
    assert_eq!(audit(), before);
    f.db.write(|c| {
        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES('self-deny',?,'status.view','*','deny','test',0)", [&device.principal_id])?;
        Ok(())
    }).unwrap();
    let denied = f
        .service
        .session_projection(token.secret.expose(), &access)
        .unwrap();
    assert!(
        !denied["capabilities"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("status.view"))
    );
    f.db.write(|c| {
        c.execute(
            "UPDATE role_assignments SET expires_at=? WHERE principal_id=?",
            params![f.clock.millis(), device.principal_id],
        )?;
        Ok(())
    })
    .unwrap();
    let expired = f
        .service
        .session_projection(token.secret.expose(), &access)
        .unwrap();
    assert!(
        !expired["roles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "viewer")
    );
    assert!(
        !expired["capabilities"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("metrics.view"))
    );
    f.service.revoke_local(&device.device_id).unwrap();
    assert!(matches!(
        f.service.session_projection(token.secret.expose(), &access),
        Err(ManagementError::SessionInvalid)
    ));
}

#[test]
fn self_projection_resolves_safe_default_sources_and_expired_overrides() {
    let f = Fixture::new();
    let (device, key) = normal(&f);
    let token = f.session(&device, &key);
    let whitelist = Whitelist::default();
    let bans = BanList::default();
    let access = policy(&f, &whitelist, &bans);
    f.db.write(|c| {
        c.execute("UPDATE user_default_roles SET role_id='player' WHERE principal_id=?",[&device.principal_id])?;
        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at,expires_at) VALUES('expired-allow',?,'metrics.view','*','allow','test',0,?)", params![device.principal_id,f.clock.millis()])?;
        Ok(())
    }).unwrap();
    let value = f
        .service
        .session_projection(token.secret.expose(), &access)
        .unwrap();
    for cap in ["public.info", "pack.download", "pack.realtime"] {
        assert!(
            value["capabilities"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(cap))
        );
    }
    assert!(
        !value["capabilities"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("metrics.view"))
    );
    assert!(
        value["roles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "player")
    );
    f.clock.0.fetch_add(SESSION_TTL_MS, Ordering::SeqCst);
    assert!(matches!(
        f.service.session_projection(token.secret.expose(), &access),
        Err(ManagementError::SessionInvalid)
    ));
}

#[test]
fn self_projection_includes_scoped_grants_only_when_a_resource_survives_denies() {
    let f = Fixture::new();
    let (device, key) = normal(&f);
    let token = f.session(&device, &key);
    let whitelist = Whitelist::default();
    let bans = BanList::default();
    let access = policy(&f, &whitelist, &bans);
    let has_files = || {
        f.service
            .session_projection(token.secret.expose(), &access)
            .unwrap()["capabilities"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("files.read"))
    };
    f.db.write(|c| {
        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES('scope-allow',?,'files.read','logs/**','allow','test',0)",[&device.principal_id])?;
        Ok(())
    }).unwrap();
    assert!(has_files());
    f.db.write(|c| {
        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES('scope-deny',?,'files.read','logs','deny','test',0)",[&device.principal_id])?;
        Ok(())
    }).unwrap();
    assert!(
        has_files(),
        "denying the directory leaves permitted child resources"
    );
    f.db.write(|c| {
        c.execute(
            "UPDATE principal_overrides SET scope='LOGS/**' WHERE id='scope-deny'",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(
        !has_files(),
        "covering case-insensitive deny removes the capability"
    );
    f.db.write(|c| {
        c.execute(
            "UPDATE principal_overrides SET expires_at=? WHERE id='scope-deny'",
            [f.clock.millis()],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(
        has_files(),
        "expired deny is excluded by the live evaluator"
    );
    f.db.write(|c| {
        c.execute("UPDATE principal_overrides SET scope='logs/latest.log' WHERE id='scope-allow'",[])?;
        c.execute("UPDATE principal_overrides SET scope='LOGS/LATEST.LOG',expires_at=NULL WHERE id='scope-deny'",[])?;
        Ok(())
    }).unwrap();
    assert!(
        !has_files(),
        "a denied exact leaf leaves no invented resource grant"
    );
}

#[test]
fn rate_preflight_is_nonblocking_and_never_treats_unknown_or_expired_tokens_as_known() {
    let f = Fixture::new();
    let (device, key) = normal(&f);
    let token = f.session(&device, &key);
    assert!(f.service.known_session(token.secret.expose()).unwrap());
    assert!(
        !f.service
            .known_session(&URL_SAFE_NO_PAD.encode([0; 32]))
            .unwrap()
    );
    assert!(!f.service.known_session("invalid").unwrap());
    {
        let held = f.service.stores.lock().unwrap();
        let access = f.service.clone();
        let credential = token.secret.expose().to_owned();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = send.send(access.known_session(&credential));
        });
        assert!(receive.try_recv().is_err());
        drop(held);
        assert!(matches!(
            receive.recv_timeout(std::time::Duration::from_secs(2)),
            Ok(Ok(true))
        ));
        worker.join().unwrap();
    }
    f.clock.0.fetch_add(SESSION_TTL_MS, Ordering::SeqCst);
    assert!(!f.service.known_session(token.secret.expose()).unwrap());
}

#[test]
fn authenticated_status_checks_do_not_wait_for_an_unrelated_database_writer() {
    use std::{sync::mpsc, thread, time::Duration};
    let f = Fixture::new();
    let (device, key) = f.enroll(BuiltinRole::Owner);
    let token = f.session(&device, &key);
    let db = f.db.clone();
    let (holding, held) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let writer = thread::spawn(move || {
        db.write(|_| {
            holding.send(()).unwrap();
            released.recv().unwrap();
            Ok(())
        })
        .unwrap()
    });
    held.recv_timeout(Duration::from_secs(5)).unwrap();
    let access = f.service.clone();
    let credential = Zeroizing::new(token.secret.expose().to_owned());
    let (done, completed) = mpsc::channel();
    let authentication = thread::spawn(move || {
        done.send(access.authorize(&credential, super::super::Capability::StatusView, None))
            .unwrap();
    });
    let observed = completed.recv_timeout(Duration::from_secs(1));
    // Always release and join before asserting, including on a regression.
    release.send(()).unwrap();
    writer.join().unwrap();
    authentication.join().unwrap();
    assert_eq!(
        observed
            .expect("read-only authentication joined the writer queue")
            .unwrap()
            .device
            .as_str(),
        device.device_id
    );
    assert!(f.service.known_session(token.secret.expose()).unwrap());
    // The new read path still resolves revocation on the very next request.
    f.service.revoke_local(&device.device_id).unwrap();
    assert_eq!(
        f.service.authorize(
            token.secret.expose(),
            super::super::Capability::StatusView,
            None
        ),
        Err(ManagementError::SessionInvalid)
    );
}

#[test]
fn authenticated_checks_do_not_revive_revoked_sessions_or_cache_authority() {
    let f = Fixture::new();
    let (device, key) = f.enroll(BuiltinRole::Owner);
    let token = f.session(&device, &key);
    assert!(f.service.authenticate(token.secret.expose()).is_ok());
    assert!(f.service.revoke_session(token.secret.expose()).unwrap());
    assert_eq!(
        f.service.authenticate(token.secret.expose()),
        Err(ManagementError::SessionInvalid)
    );
    assert!(!f.service.known_session(token.secret.expose()).unwrap());
    let next = f.session(&device, &key);
    assert!(f.service.authenticate(next.secret.expose()).is_ok());
    f.db.write(|c| {
        c.execute(
            "DELETE FROM role_assignments WHERE principal_id=?",
            [&device.principal_id],
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        f.service.authorize(
            next.secret.expose(),
            super::super::Capability::StatusView,
            None
        ),
        Err(ManagementError::Forbidden)
    );
}

#[test]
fn verified_account_invitation_enrollment_is_atomic_and_cannot_reuse_another_accounts_key() {
    let f = Fixture::new();
    let k = key();
    let public = URL_SAFE_NO_PAD.encode(k.public_key().as_ref());
    let code = f.service.issue_local(BuiltinRole::Owner, 10).unwrap();
    let pack_profile = |id: &str| {
        VerifiedMinecraftProfile::for_test(
            ServerUuid::parse(f.service.server_id()).unwrap(),
            MinecraftUuid::parse(id).unwrap(),
        )
    };
    assert!(matches!(
        f.service.enroll_pack_profile_with_invitation(
            pack_profile(USER_UUID),
            "Owner",
            &public,
            Some("invalid")
        ),
        Err(ManagementError::PairInvalid)
    ));
    assert!(f.service.list_local().unwrap().is_empty());
    f.db.write(|c| { c.execute_batch("CREATE TRIGGER fail_verified_audit BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'fixture'); END;")?; Ok(()) }).unwrap();
    assert!(matches!(
        f.service.enroll_pack_profile_with_invitation(
            pack_profile(USER_UUID),
            "Owner",
            &public,
            Some(code.secret.expose())
        ),
        Err(ManagementError::Unavailable)
    ));
    assert!(f.service.list_local().unwrap().is_empty());
    f.db.write(|c| {
        c.execute_batch("DROP TRIGGER fail_verified_audit")?;
        Ok(())
    })
    .unwrap();
    let owner = f
        .service
        .enroll_pack_profile_with_invitation(
            pack_profile(USER_UUID),
            "Owner",
            &public,
            Some(code.secret.expose()),
        )
        .unwrap();
    let token = f.session(&owner, &k);
    assert!(
        f.service
            .authorize(token.secret.expose(), Capability::SecurityManage, None)
            .is_ok()
    );
    let stranger = "123e4567-e89b-42d3-a456-426614174000";
    assert!(matches!(
        f.service.enroll_pack_profile_with_invitation(
            pack_profile(stranger),
            "Other",
            &public,
            None
        ),
        Err(ManagementError::SessionInvalid)
    ));
    let other = key();
    let other_public = URL_SAFE_NO_PAD.encode(other.public_key().as_ref());
    assert!(matches!(
        f.service.enroll_pack_profile_with_invitation(
            pack_profile(stranger),
            "Other",
            &other_public,
            Some(code.secret.expose())
        ),
        Err(ManagementError::PairInvalid)
    ));
    let player = f
        .service
        .enroll_pack_profile_with_invitation(pack_profile(stranger), "Other", &other_public, None)
        .unwrap();
    assert_ne!(player.principal_id, owner.principal_id);
    let player_token = f.session(&player, &other);
    assert!(matches!(
        f.service.authorize(
            player_token.secret.expose(),
            Capability::SecurityManage,
            None
        ),
        Err(ManagementError::Forbidden)
    ));
    let reused = f
        .service
        .enroll_pack_profile_with_invitation(pack_profile(USER_UUID), "Owner", &public, None)
        .unwrap();
    assert_eq!(reused.principal_id, owner.principal_id);
    assert_eq!(reused.device_id, owner.device_id);
}

#[test]
fn legacy_unbound_owner_credentials_and_sessions_cannot_authenticate_or_attach_an_account() {
    let f = Fixture::new();
    let code = f.service.issue_local(BuiltinRole::Owner, 10).unwrap();
    let k = key();
    let public = URL_SAFE_NO_PAD.encode(k.public_key().as_ref());
    let legacy = f
        .service
        .enroll(code.secret.expose(), "Legacy", &public)
        .unwrap();
    assert!(matches!(
        f.service.challenge(&legacy.device_id),
        Err(ManagementError::SessionInvalid)
    ));
    assert!(matches!(
        f.service
            .enroll_minecraft(profile(&f, USER_UUID), "Legacy", &public),
        Err(ManagementError::SessionInvalid)
    ));
    let (linked, linked_key) = f.enroll(BuiltinRole::Owner);
    let session = f.session(&linked, &linked_key);
    // A legacy key on a linked user must remain blocked too. Principal-level
    // account linkage cannot authenticate keys which never verified that UUID.
    f.db.write(|c| {
        c.execute(
            "UPDATE management_device_keys SET linked_minecraft_uuid=NULL WHERE device_id=?",
            [&linked.device_id],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        f.service.challenge(&linked.device_id),
        Err(ManagementError::SessionInvalid)
    ));
    assert!(matches!(
        f.service.authenticate(session.secret.expose()),
        Err(ManagementError::SessionInvalid)
    ));
    f.db.write(|c| {c.execute("UPDATE management_device_keys SET linked_minecraft_uuid='123e4567-e89b-42d3-a456-426614174000' WHERE device_id=?", [&linked.device_id])?; Ok(())}).unwrap();
    assert!(matches!(
        f.service.authenticate(session.secret.expose()),
        Err(ManagementError::SessionInvalid)
    ));
    f.db.write(|c| {
        c.execute(
            "DELETE FROM user_minecraft_accounts WHERE principal_id=?",
            [&linked.principal_id],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        f.service.authenticate(session.secret.expose()),
        Err(ManagementError::SessionInvalid)
    ));
}

#[test]
fn lower_invitation_preserves_existing_linked_owner_authority() {
    let f = Fixture::new();
    f.service
        .link_local(MinecraftUuid::parse(USER_UUID).unwrap(), BuiltinRole::Owner)
        .unwrap();
    let code = f.service.issue_local(BuiltinRole::Viewer, 10).unwrap();
    let k = key();
    let owner = f
        .service
        .enroll_minecraft_with_invitation(
            profile(&f, USER_UUID),
            "Owner",
            &URL_SAFE_NO_PAD.encode(k.public_key().as_ref()),
            Some(code.secret.expose()),
        )
        .unwrap();
    assert_eq!(owner.role, "owner");
    let token = f.session(&owner, &k);
    assert!(
        f.service
            .authorize(token.secret.expose(), Capability::SecurityManage, None)
            .is_ok()
    );
}

#[test]
fn explicit_repair_after_device_revocation_requires_a_fresh_key_and_keeps_account_identity() {
    let f = Fixture::new();
    let (old, old_key) = f.enroll(BuiltinRole::Owner);
    let uuid: String =
        f.db.read(|c| {
            Ok(c.query_row(
                "SELECT minecraft_uuid FROM user_minecraft_accounts WHERE principal_id=?",
                [&old.principal_id],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    f.db.write(|c| {
        c.execute(
            "UPDATE devices SET revoked_at=? WHERE id=?",
            params![f.db.clock().millis(), old.device_id],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(f.service.challenge(&old.device_id).is_err());
    assert!(
        f.service
            .enroll_minecraft_with_invitation(
                profile(&f, &uuid),
                "Retry old key",
                &URL_SAFE_NO_PAD.encode(old_key.public_key().as_ref()),
                None
            )
            .is_err()
    );
    let fresh = key();
    let repaired = f
        .service
        .enroll_minecraft_with_invitation(
            profile(&f, &uuid),
            "Re-paired",
            &URL_SAFE_NO_PAD.encode(fresh.public_key().as_ref()),
            None,
        )
        .unwrap();
    assert_eq!(repaired.principal_id, old.principal_id);
    assert_eq!(repaired.server_id, old.server_id);
    assert_ne!(repaired.device_id, old.device_id);
    let token = f.session(&repaired, &fresh);
    assert!(
        f.service
            .authorize(token.secret.expose(), Capability::SecurityManage, None)
            .is_ok()
    );
    assert!(f.service.challenge(&old.device_id).is_err());
}
