//! Audited pack-password lifecycle and authorization regressions over real SQLite.
//!
//! `PackPasswords` intentionally remains a child-module test target so this module can
//! compose its private low-cost KDF policy/gate and use only the production hook that
//! runs after derivation. The fixture inserts trusted identity rows directly; it never
//! treats a `Subject` lookup key as proof of caller identity.
use super::super::password::KdfCost;
use super::super::{BuiltinRole, DenyReason, DeviceId, PrincipalId, Subject};
use super::*;
use crate::ControlError;
use rusqlite::{OptionalExtension, params};
use sha2::Digest;
use std::sync::{
    Arc,
    atomic::{AtomicI64, AtomicUsize, Ordering},
};
use tempfile::TempDir;
use vesta_state::audit::AuditLog;
use vesta_state::{Clock, StateDatabase, StateDirectory};

const NOW: i64 = 1_791_115_200_000;
type CredentialRow = (i64, Option<String>, i64, i64, Option<i64>);

#[path = "pack_passwords_failure_paths.rs"]
mod failure_paths;

struct TestClock {
    now: AtomicI64,
    next_sample: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
}
impl TestClock {
    fn new(now: i64) -> Self {
        Self {
            now: AtomicI64::new(now),
            next_sample: std::sync::Mutex::new(None),
        }
    }
    fn set(&self, now: i64) {
        self.now.store(now, Ordering::SeqCst);
    }
    fn watch_next_sample(&self) -> std::sync::mpsc::Receiver<()> {
        let (sender, receiver) = std::sync::mpsc::channel();
        *self.next_sample.lock().expect("clock notification lock") = Some(sender);
        receiver
    }
}
impl Clock for TestClock {
    fn millis(&self) -> i64 {
        if let Some(sender) = self
            .next_sample
            .lock()
            .expect("clock notification lock")
            .take()
        {
            let _ = sender.send(());
        }
        self.now.load(Ordering::SeqCst)
    }
}

struct Fixture {
    _root: TempDir,
    clock: Arc<TestClock>,
    db: StateDatabase,
    audit: AuditLog,
    admin: Subject,
    viewer: Subject,
}
impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().expect("create pack-password test directory");
        let clock = Arc::new(TestClock::new(NOW));
        let directory = StateDirectory::new(root.path().join("state"), None)
            .expect("create pack-password state directory");
        let db = StateDatabase::open(directory, clock.clone()).expect("open real SQLite state");
        let audit = AuditLog::new(db.clone()).expect("verify empty audit chain");
        let admin = seed_subject(&db, NOW, BuiltinRole::Administrator.id());
        let viewer = seed_subject(&db, NOW, BuiltinRole::Viewer.id());
        Self {
            _root: root,
            clock,
            db,
            audit,
            admin,
            viewer,
        }
    }

    fn service(&self) -> PackPasswords {
        PackPasswords::compose(
            self.db.clone(),
            fast_policy(),
            GateConfig::new(2, 10_000, 1_000)
                .and_then(KdfGate::new)
                .expect("independent test gate"),
        )
        .expect("compose real pack-password service")
    }

    fn count(&self, table: &str) -> i64 {
        assert!(matches!(
            table,
            "pack_passwords"
                | "principals"
                | "devices"
                | "role_assignments"
                | "role_capabilities"
                | "principal_overrides"
                | "audit_events"
        ));
        self.db
            .read(|connection| {
                Ok(
                    connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })?,
                )
            })
            .expect("count fixture rows")
    }

    fn row(&self) -> Option<CredentialRow> {
        self.db.read(|connection| Ok(connection.query_row(
            "SELECT revision,phc,created_at,updated_at,revoked_at FROM pack_passwords WHERE server_id=?",
            [self.db.server_id()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        ).optional()?)).expect("read credential row")
    }

    fn override_capability(&self, subject: &Subject, effect: &str) {
        self.db.write(|connection| {
            connection.execute(
                "INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) \
                 VALUES (?,?, 'credentials.manage','*',?,'trusted-test-fixture',?)",
                params![uuid::Uuid::new_v4().to_string(), subject.principal.as_str(), effect, self.clock.millis()],
            )?;
            Ok(())
        }).expect("insert trusted policy fixture override");
    }

    fn audit_entries(&self) -> Vec<vesta_state::audit::AuditEntry> {
        self.audit.page(0, 500).expect("read audit entries")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.db
            .close()
            .expect("close pack-password fixture database");
    }
}

fn fast_policy() -> HashPolicy {
    HashPolicy::new(KdfCost::HARD_MIN, KdfCost::DEFAULT_VERIFY_LIMIT)
        .expect("valid low-cost test policy")
}
fn password(text: &str) -> SecretPassword {
    SecretPassword::try_from(text.to_owned()).expect("valid test password")
}
fn revision(value: u64) -> PasswordRevision {
    PasswordRevision::new(value).expect("positive test revision")
}
fn seed_subject(db: &StateDatabase, now: i64, role: &str) -> Subject {
    let principal_text = uuid::Uuid::new_v4().to_string();
    let device_text = uuid::Uuid::new_v4().to_string();
    let fingerprint = hex::encode(sha2::Sha256::digest(uuid::Uuid::new_v4().as_bytes()));
    db.write(|connection| {
        connection.execute(
            "INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,?,'active',?,?)",
            params![principal_text, db.server_id(), format!("Fixture {role}"), now, now],
        )?;
        connection.execute(
            "INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Fixture device',?,?)",
            params![device_text, db.server_id(), principal_text, fingerprint, now],
        )?;
        connection.execute(
            "INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,?,?,?)",
            params![uuid::Uuid::new_v4().to_string(), principal_text, role, "trusted-test-fixture", now],
        )?;
        Ok(())
    }).expect("seed trusted SQLite identity facts");
    Subject {
        principal: PrincipalId::parse(&principal_text).expect("fixture principal id"),
        device: DeviceId::parse(&device_text).expect("fixture device id"),
    }
}

fn assert_forbidden(error: &ControlError, reason: DenyReason) {
    assert!(
        matches!(error,
            ControlError::Rejected { code: crate::IdentityErrorCode::Forbidden, reason: Some(actual) }
                if *actual == reason
        ),
        "expected forbidden/{reason:?}, got {error:?}"
    );
}

#[test]
fn create_rotate_revoke_repeat_and_reactivate_keep_exact_positive_revisions() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let first_secret = "first pack secret, never a login or authority token";
    let first = service
        .set(
            &fixture.admin,
            PasswordRevision::ABSENT,
            &password(first_secret),
        )
        .expect("create at absent revision");
    assert_eq!(first.revision(), revision(1));
    assert!(first.active());
    assert_eq!(first.created_at(), Some(NOW));
    assert_eq!(first.updated_at(), Some(NOW));
    assert_eq!(first.revoked_at(), None);

    let initial = fixture.row().expect("created credential");
    let stored_phc = initial.1.clone().expect("active verifier");
    assert!(!stored_phc.contains(first_secret));
    assert!(!format!("{first:?}").contains(&stored_phc));
    assert!(!format!("{first:?}").contains(first_secret));
    let conflict = service
        .set(&fixture.admin, PasswordRevision::ABSENT, &password("stale"))
        .expect_err("creation cannot reuse absent after row exists");
    assert_eq!(conflict.code(), "credential_conflict");
    assert_eq!(fixture.row().expect("row remains").0, 1);

    fixture.clock.set(NOW + 10);
    let rotated = service
        .set(&fixture.admin, revision(1), &password("second pack secret"))
        .expect("rotate exact revision");
    assert_eq!(rotated.revision(), revision(2));
    assert_eq!(rotated.created_at(), Some(NOW));
    assert_eq!(rotated.updated_at(), Some(NOW + 10));
    assert!(rotated.active());

    fixture.clock.set(NOW + 20);
    let revoked = service
        .revoke(&fixture.admin, revision(2))
        .expect("revoke exact revision");
    assert_eq!(revoked.revision(), revision(3));
    assert!(!revoked.active());
    assert_eq!(revoked.created_at(), Some(NOW));
    assert_eq!(revoked.updated_at(), Some(NOW + 20));
    assert_eq!(revoked.revoked_at(), Some(NOW + 20));
    assert_eq!(fixture.row().expect("tombstone").1, None);

    let unavailable = service
        .verify(b"second pack secret".to_vec())
        .expect("audit unavailable result");
    assert_eq!(
        unavailable.status(),
        PasswordVerificationStatus::Unavailable
    );
    assert_eq!(unavailable.revision(), revision(3));

    fixture.clock.set(NOW + 30);
    let repeated = service
        .revoke(&fixture.admin, revision(3))
        .expect("tombstone remains monotonic");
    assert_eq!(repeated.revision(), revision(4));
    assert!(!repeated.active());
    assert_eq!(repeated.created_at(), Some(NOW));
    assert_eq!(repeated.revoked_at(), Some(NOW + 30));

    fixture.clock.set(NOW + 40);
    let reactivated = service
        .set(
            &fixture.admin,
            revision(4),
            &password("reactivated pack secret"),
        )
        .expect("reactivate tombstone at exact revision");
    assert_eq!(reactivated.revision(), revision(5));
    assert!(reactivated.active());
    assert_eq!(reactivated.created_at(), Some(NOW));
    assert_eq!(reactivated.updated_at(), Some(NOW + 40));
    assert_eq!(reactivated.revoked_at(), None);

    let max_revision = revision(i64::MAX as u64);
    fixture
        .db
        .write(|connection| {
            connection.execute(
                "UPDATE pack_passwords SET revision=? WHERE server_id=?",
                params![i64::MAX, fixture.db.server_id()],
            )?;
            Ok(())
        })
        .expect("seed valid positive maximum revision");
    let before = fixture.count("audit_events");
    let exhausted = service
        .revoke(&fixture.admin, max_revision)
        .expect_err("revision cannot wrap");
    assert_eq!(exhausted.code(), "credential_revision_exhausted");
    assert_eq!(fixture.row().expect("row remains at maximum").0, i64::MAX);
    assert_eq!(
        fixture.count("audit_events"),
        before + 1,
        "denial is separately audited"
    );
    let entries = fixture.audit_entries();
    let mutations: Vec<_> = entries
        .iter()
        .filter(|entry| entry.action.starts_with("pack_password."))
        .map(|entry| (entry.action.as_str(), entry.outcome.as_str()))
        .collect();
    assert!(
        mutations
            .iter()
            .any(|(action, outcome)| *action == "pack_password.set" && *outcome == "success")
    );
    assert!(
        mutations
            .iter()
            .any(|(action, outcome)| *action == "pack_password.revoke" && *outcome == "success")
    );
    assert!(
        mutations
            .iter()
            .any(|(action, outcome)| *action == "pack_password.revoke" && *outcome == "denied")
    );
    for entry in &entries {
        let text = format!("{entry:?}");
        assert!(!text.contains(first_secret));
        assert!(!text.contains(&stored_phc));
    }
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn canonical_authority_denials_happen_before_kdf_for_deny_reserved_expiry_device_and_foreign_subjects()
 {
    let fixture = Fixture::new();
    let explicit_deny = seed_subject(&fixture.db, NOW, BuiltinRole::Administrator.id());
    fixture.override_capability(&explicit_deny, "deny");
    let reserved_allow = fixture.viewer.clone();
    fixture.override_capability(&reserved_allow, "allow");
    let expired = seed_subject(&fixture.db, NOW, BuiltinRole::Administrator.id());
    fixture.db.write(|connection| {
        connection.execute("UPDATE role_assignments SET expires_at=? WHERE principal_id=? AND role_id='administrator' AND revoked_at IS NULL",
            params![NOW - 1, expired.principal.as_str()])?;
        Ok(())
    }).expect("expire administrator assignment");
    let revoked_device = seed_subject(&fixture.db, NOW, BuiltinRole::Administrator.id());
    fixture
        .db
        .write(|connection| {
            connection.execute(
                "UPDATE devices SET revoked_at=? WHERE id=?",
                params![NOW, revoked_device.device.as_str()],
            )?;
            Ok(())
        })
        .expect("revoke device");
    let foreign = Subject {
        principal: PrincipalId::parse("not-a-current-principal").expect("foreign lookup id"),
        device: DeviceId::parse("not-a-current-device").expect("foreign lookup id"),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let mut hooked = fixture.service();
    hooked.before_commit = Some(Arc::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
    }));

    let work_before = super::super::password::test_work_counts();
    for (subject, expected) in [
        (&explicit_deny, DenyReason::ExplicitDeny),
        (&reserved_allow, DenyReason::Reserved),
        (&expired, DenyReason::Reserved),
        (&revoked_device, DenyReason::NoIdentity),
        (&foreign, DenyReason::NoIdentity),
        (&fixture.viewer, DenyReason::Reserved),
    ] {
        let error = hooked
            .set(
                subject,
                PasswordRevision::ABSENT,
                &password("must not derive"),
            )
            .expect_err("current canonical authority denies mutation");
        assert_forbidden(&error, expected);
    }
    assert_eq!(
        super::super::password::test_work_counts(),
        work_before,
        "denials do no KDF or workspace reservation"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "precheck refusal never reaches post-KDF hook"
    );
    assert_eq!(fixture.count("pack_passwords"), 0);
    assert_eq!(
        fixture.count("audit_events"),
        6,
        "each domain refusal has a separate denial audit"
    );
    assert!(
        fixture
            .audit
            .verify_chain()
            .expect("denial audit chain")
            .valid
    );
}

#[derive(Clone, Copy)]
enum PostKdfChange {
    ExplicitDeny,
    ReservedTier,
    Expire,
    RevokeDevice,
    DisablePrincipal,
    ForeignBinding,
}

fn assert_authority_rechecked_after_kdf(change: PostKdfChange, expected: DenyReason) {
    let fixture = Fixture::new();
    let subject = seed_subject(&fixture.db, NOW, BuiltinRole::Administrator.id());
    let db = fixture.db.clone();
    let actor = subject.clone();
    let observed = Arc::new(AtomicUsize::new(0));
    let hook_observed = Arc::clone(&observed);
    let mut service = fixture.service();
    service.before_commit = Some(Arc::new(move || {
        // The derivation hook is outside every SQLite transaction: this independent write
        // must complete before the password command starts its admitted mutation transaction.
        db.write(|connection| {
            let _: i64 = connection.query_row("SELECT count(*) FROM principals", [], |row| row.get(0))?;
            match change {
                PostKdfChange::ExplicitDeny => {
                    connection.execute(
                        "INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) \
                         VALUES (?,?, 'credentials.manage','*','deny','test-hook',?)",
                        params![uuid::Uuid::new_v4().to_string(), actor.principal.as_str(), NOW],
                    )?;
                }
                PostKdfChange::ReservedTier => {
                    connection.execute("UPDATE role_assignments SET role_id='viewer' WHERE principal_id=? AND role_id='administrator' AND revoked_at IS NULL",
                        [actor.principal.as_str()])?;
                    connection.execute(
                        "INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) \
                         VALUES (?,?, 'credentials.manage','*','allow','test-hook',?)",
                        params![uuid::Uuid::new_v4().to_string(), actor.principal.as_str(), NOW],
                    )?;
                }
                PostKdfChange::Expire => {
                    connection.execute("UPDATE role_assignments SET expires_at=? WHERE principal_id=? AND role_id='administrator' AND revoked_at IS NULL",
                        params![NOW, actor.principal.as_str()])?;
                }
                PostKdfChange::RevokeDevice => {
                    connection.execute("UPDATE devices SET revoked_at=? WHERE id=?",
                        params![NOW, actor.device.as_str()])?;
                }
                PostKdfChange::DisablePrincipal => {
                    connection.execute("UPDATE principals SET status='disabled' WHERE id=?",
                        [actor.principal.as_str()])?;
                }
                PostKdfChange::ForeignBinding => {
                    let foreign_principal = uuid::Uuid::new_v4().to_string();
                    connection.execute(
                        "INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?, 'Foreign fixture','active',?,?)",
                        params![foreign_principal, db.server_id(), NOW, NOW],
                    )?;
                    connection.execute("UPDATE devices SET principal_id=? WHERE id=?",
                        params![foreign_principal, actor.device.as_str()])?;
                }
            }
            Ok(())
        }).expect("post-KDF fixture mutation commits with no held service transaction");
        hook_observed.fetch_add(1, Ordering::SeqCst);
    }));

    let error = service
        .set(
            &subject,
            PasswordRevision::ABSENT,
            &password("derived before authority changed"),
        )
        .expect_err("transaction reload rejects authority changed after KDF");
    assert_forbidden(&error, expected);
    assert_eq!(
        observed.load(Ordering::SeqCst),
        1,
        "real KDF reached the post-derivation hook"
    );
    assert_eq!(
        fixture.count("pack_passwords"),
        0,
        "refusal leaves no credential"
    );
    assert_eq!(
        fixture.count("audit_events"),
        1,
        "post-KDF refusal is audited separately"
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn mutation_transaction_reloads_explicit_deny_reserved_expiry_device_and_identity_facts_after_kdf()
{
    for (change, reason) in [
        (PostKdfChange::ExplicitDeny, DenyReason::ExplicitDeny),
        (PostKdfChange::ReservedTier, DenyReason::Reserved),
        (PostKdfChange::Expire, DenyReason::Reserved),
        (PostKdfChange::RevokeDevice, DenyReason::NoIdentity),
        (PostKdfChange::DisablePrincipal, DenyReason::NoIdentity),
        (PostKdfChange::ForeignBinding, DenyReason::NoIdentity),
    ] {
        assert_authority_rechecked_after_kdf(change, reason);
    }
}

fn identity_facts(fixture: &Fixture) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    fixture
        .db
        .read(|connection| {
            [
                "SELECT * FROM principals ORDER BY id",
                "SELECT * FROM devices ORDER BY id",
                "SELECT * FROM role_assignments ORDER BY id",
                "SELECT * FROM role_capabilities ORDER BY role_id, capability, scope",
                "SELECT * FROM principal_overrides ORDER BY id",
            ]
            .into_iter()
            .map(|sql| {
                connection
                    .prepare(sql)?
                    .query_map([], |row| {
                        (0..row.as_ref().column_count())
                            .map(|index| row.get(index))
                            .collect::<rusqlite::Result<Vec<_>>>()
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Into::into)
            })
            .collect()
        })
        .expect("snapshot complete authority facts")
}

fn identity_counts(fixture: &Fixture) -> (i64, i64, i64, i64, i64) {
    (
        fixture.count("principals"),
        fixture.count("devices"),
        fixture.count("role_assignments"),
        fixture.count("role_capabilities"),
        fixture.count("principal_overrides"),
    )
}

#[test]
fn verification_is_audited_match_only_and_stale_hash_or_revision_never_returns_match() {
    let fixture = Fixture::new();
    let mut service = fixture.service();
    let old = "old match-only pack password";
    let created = service
        .set(&fixture.admin, PasswordRevision::ABSENT, &password(old))
        .expect("create");
    assert_eq!(created.revision(), revision(1));
    let identity_before = identity_counts(&fixture);
    let facts_before = identity_facts(&fixture);
    let matched = service
        .verify(old.as_bytes().to_vec())
        .expect("verify current password");
    assert_eq!(matched.status(), PasswordVerificationStatus::Match);
    assert_eq!(matched.revision(), revision(1));
    assert_eq!(
        identity_counts(&fixture),
        identity_before,
        "match is no identity, session, role or grant"
    );

    let phc_at_revision_one = service
        .hasher
        .hash(&password("different valid PHC, same revision"))
        .expect("derive independent bounded verifier")
        .as_str()
        .to_owned();
    let db = fixture.db.clone();
    let replacement_phc = phc_at_revision_one.clone();
    service.before_commit = Some(Arc::new(move || {
        db.write(|connection| {
            connection.execute(
                "UPDATE pack_passwords SET phc=? WHERE server_id=? AND revision=1",
                params![replacement_phc, db.server_id()],
            )?;
            Ok(())
        })
        .expect("replace PHC at same revision in separate committed write");
    }));
    let same_revision = service
        .verify(old.as_bytes().to_vec())
        .expect("audit changed snapshot");
    assert_eq!(
        same_revision.status(),
        PasswordVerificationStatus::Changed,
        "snapshot comparison includes PHC, not revision alone"
    );
    assert_eq!(same_revision.revision(), revision(1));

    service.before_commit = None;
    assert_eq!(
        service
            .verify(b"different valid PHC, same revision".to_vec())
            .expect("baseline current verifier")
            .status(),
        PasswordVerificationStatus::Match
    );
    let rotate_gate = service.gate.clone();
    let rotator = PackPasswords::compose(fixture.db.clone(), fast_policy(), rotate_gate.clone())
        .expect("independent real service sharing this gate");
    let admin = fixture.admin.clone();
    service.before_commit = Some(Arc::new(move || {
        rotator
            .set(&admin, revision(1), &password("rotated password"))
            .expect("real admitted rotation from transaction-free hook");
    }));
    let stale_after_rotation = service
        .verify(b"different valid PHC, same revision".to_vec())
        .expect("audit stale result");
    assert_eq!(
        stale_after_rotation.status(),
        PasswordVerificationStatus::Changed
    );
    assert_eq!(stale_after_rotation.revision(), revision(1));
    assert_eq!(fixture.row().expect("rotated row").0, 2);

    service.before_commit = None;
    let revoker = PackPasswords::compose(fixture.db.clone(), fast_policy(), rotate_gate)
        .expect("real service sharing gate for revoke hook");
    let admin = fixture.admin.clone();
    service.before_commit = Some(Arc::new(move || {
        revoker
            .revoke(&admin, revision(2))
            .expect("real revoke in transaction-free hook");
    }));
    let stale_after_revoke = service
        .verify(b"rotated password".to_vec())
        .expect("audit stale result");
    assert_eq!(
        stale_after_revoke.status(),
        PasswordVerificationStatus::Changed
    );
    assert_eq!(stale_after_revoke.revision(), revision(2));
    service.before_commit = None;
    let revoked = service
        .verify(b"rotated password".to_vec())
        .expect("verify after revoke");
    assert_eq!(revoked.status(), PasswordVerificationStatus::Unavailable);
    assert_eq!(revoked.revision(), revision(3));
    assert_eq!(identity_counts(&fixture), identity_before);
    assert_eq!(
        identity_facts(&fixture),
        facts_before,
        "password matches never change existing authority facts"
    );
    let viewer_policy = super::super::IdentityService::new(fixture.db.clone())
        .expect("identity observer")
        .current_policy(Some(&fixture.viewer))
        .expect("current viewer policy");
    assert_eq!(
        viewer_policy.check(Capability::CredentialsManage, None),
        Decision::Deny(DenyReason::Reserved)
    );
    assert_forbidden(
        &service
            .metadata(&fixture.viewer)
            .expect_err("password matching never grants management"),
        DenyReason::Reserved,
    );

    let entries = fixture.audit_entries();
    assert!(
        entries
            .iter()
            .any(|entry| entry.action == "pack_password.verify" && entry.outcome == "match")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.action == "pack_password.verify" && entry.outcome == "changed")
    );
    for entry in &entries {
        let rendered = format!("{entry:?}");
        assert!(!rendered.contains(old));
        assert!(!rendered.contains("rotated password"));
        assert!(!rendered.contains(&phc_at_revision_one));
        assert!(!entry.metadata_json.contains("phc"));
    }
    assert!(!format!("{matched:?}").contains(old));
    assert!(!format!("{created:?}").contains(old));
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn complete_sqlite_text_bytes_are_bounded_before_copy_and_bounded_malformed_phc_is_audited() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service
        .set(
            &fixture.admin,
            PasswordRevision::ABSENT,
            &password("stored phc boundary fixture"),
        )
        .expect("create active verifier");
    let prefix = fixture.row().expect("active row").1.expect("verifier");
    let tailed = format!("{prefix}\0{}", "x".repeat(300));
    fixture
        .db
        .write(|connection| {
            connection.execute(
                "UPDATE pack_passwords SET phc=? WHERE server_id=?",
                params![tailed, fixture.db.server_id()],
            )?;
            Ok(())
        })
        .expect("schema TEXT length check sees allowed prefix before NUL");
    let (text_length, byte_length): (i64, i64) = fixture
        .db
        .read(|connection| {
            Ok(connection.query_row(
        "SELECT length(phc),length(CAST(phc AS BLOB)) FROM pack_passwords WHERE server_id=?",
        [fixture.db.server_id()], |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
        })
        .expect("inspect complete stored value without copying in production");
    assert!(text_length <= 256);
    assert!(byte_length > 256);
    let invalid_storage = service
        .verify(b"stored phc boundary fixture".to_vec())
        .expect_err("reject oversized complete SQLite text bytes before copying");
    assert_eq!(invalid_storage.code(), "state_credential_invalid");
    assert_eq!(
        fixture.count("audit_events"),
        1,
        "corrupt-state read fails before audit"
    );

    fixture
        .db
        .write(|connection| {
            connection.execute(
                "UPDATE pack_passwords SET phc='bounded-malformed' WHERE server_id=?",
                [fixture.db.server_id()],
            )?;
            Ok(())
        })
        .expect("install bounded malformed verifier fixture");
    let malformed = service
        .verify(b"stored phc boundary fixture".to_vec())
        .expect("audit bounded malformed verifier");
    assert_eq!(
        malformed.status(),
        PasswordVerificationStatus::InvalidVerifier
    );
    assert_eq!(malformed.revision(), revision(1));
    assert_eq!(fixture.count("audit_events"), 2);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn failed_set_and_verification_audits_do_not_commit_or_return_unrecorded_outcomes() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.db.write(|connection| {
        connection.execute_batch("CREATE TRIGGER fail_pack_audit BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'test fixture'); END")?;
        Ok(())
    }).expect("install real SQLite audit failure trigger");
    let set_error = service
        .set(
            &fixture.admin,
            PasswordRevision::ABSENT,
            &password("rollback secret"),
        )
        .expect_err("mutation success is not returned without its audit");
    assert!(
        matches!(set_error, ControlError::State(_)),
        "audit insertion failure is infrastructure"
    );
    assert_eq!(
        fixture.count("pack_passwords"),
        0,
        "failed mutation audit rolls back credential row"
    );
    assert_eq!(fixture.count("audit_events"), 0);
    fixture
        .db
        .write(|connection| {
            connection.execute_batch("DROP TRIGGER fail_pack_audit")?;
            Ok(())
        })
        .expect("remove audit failure trigger");

    service
        .set(
            &fixture.admin,
            PasswordRevision::ABSENT,
            &password("verification audit fixture"),
        )
        .expect("create credential after rollback");
    let prior_audits = fixture.count("audit_events");
    fixture.db.write(|connection| {
        connection.execute_batch("CREATE TRIGGER fail_pack_audit BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'test fixture'); END")?;
        Ok(())
    }).expect("restore SQLite audit failure trigger");
    let verify_error = service
        .verify(b"verification audit fixture".to_vec())
        .expect_err("match cannot escape without its audit");
    assert!(
        matches!(verify_error, ControlError::State(_)),
        "no successful verification escapes failed audit insertion"
    );
    assert_eq!(fixture.count("audit_events"), prior_audits);
    fixture
        .db
        .write(|connection| {
            connection.execute_batch("DROP TRIGGER fail_pack_audit")?;
            Ok(())
        })
        .expect("remove audit trigger");
    assert!(
        fixture
            .audit
            .verify_chain()
            .expect("unchanged audit chain")
            .valid
    );
}

#[test]
fn busy_and_rate_refusals_are_not_audited_and_revoke_needs_no_kdf_slot() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service
        .set(
            &fixture.admin,
            PasswordRevision::ABSENT,
            &password("permit accounting fixture"),
        )
        .expect("create credential");
    let first = service
        .gate
        .try_acquire(Purpose::Verification)
        .expect("occupy first real gate slot");
    let second = service
        .gate
        .try_acquire(Purpose::Management)
        .expect("occupy second real gate slot");
    let before_busy = fixture.count("audit_events");
    let busy = service
        .verify(b"permit accounting fixture".to_vec())
        .expect("busy refusal is a status");
    assert_eq!(busy.status(), PasswordVerificationStatus::Busy);
    assert_eq!(
        fixture.count("audit_events"),
        before_busy,
        "busy attempts cannot flood audit"
    );
    let revoked = service
        .revoke(&fixture.admin, revision(1))
        .expect("cheap revoke while every KDF slot is held");
    assert_eq!(revoked.revision(), revision(2));
    assert!(!revoked.active());
    drop((first, second));

    let limited_fixture = Fixture::new();
    let rate_gate =
        KdfGate::new(GateConfig::new(2, 1, 1).expect("valid isolated one-per-minute gate"))
            .expect("isolated rate gate");
    let limited = PackPasswords::compose(limited_fixture.db.clone(), fast_policy(), rate_gate)
        .expect("compose isolated-rate test service");
    limited
        .set(
            &limited_fixture.admin,
            PasswordRevision::ABSENT,
            &password("one management admission"),
        )
        .expect("first management admission");
    let audits_after_first_set = limited_fixture.count("audit_events");
    let management_limited = limited
        .set(
            &limited_fixture.admin,
            revision(1),
            &password("must not hash"),
        )
        .expect_err("management bucket refuses immediately without waiting");
    assert_eq!(management_limited.code(), "password_rate_limited");
    assert_eq!(
        limited_fixture.count("audit_events"),
        audits_after_first_set,
        "local admission refusals are not denial-audited"
    );

    let first_verify = limited
        .verify(b"wrong but valid password".to_vec())
        .expect("first verification");
    assert_eq!(first_verify.status(), PasswordVerificationStatus::Mismatch);
    let audits_after_verify = limited_fixture.count("audit_events");
    let rate_limited = limited
        .verify(b"wrong but valid password".to_vec())
        .expect("verification rate refusal status");
    assert_eq!(
        rate_limited.status(),
        PasswordVerificationStatus::RateLimited
    );
    assert_eq!(
        limited_fixture.count("audit_events"),
        audits_after_verify,
        "rate refusals are deliberately unaudited"
    );
    assert!(
        limited_fixture
            .audit
            .verify_chain()
            .expect("audit chain")
            .valid
    );
}

#[test]
fn closing_database_from_transaction_free_kdf_hook_fails_closed_and_releases_gate_permit() {
    let fixture = Fixture::new();
    let mut service = fixture.service();
    let gate = service.gate.clone();
    let db = fixture.db.clone();
    let close_count = Arc::new(AtomicUsize::new(0));
    let hook_count = Arc::clone(&close_count);
    service.before_commit = Some(Arc::new(move || {
        // close() would reject a callback that still held an admitted DB transaction.
        db.close()
            .expect("close database while hook is outside all transactions");
        hook_count.fetch_add(1, Ordering::SeqCst);
    }));
    let error = service
        .set(
            &fixture.admin,
            PasswordRevision::ABSENT,
            &password("close in hook fixture"),
        )
        .expect_err("closed database rejects the post-KDF mutation");
    assert_eq!(close_count.load(Ordering::SeqCst), 1);
    assert_eq!(error.code(), "state_closed");

    // The operation's actual KDF permit was dropped on the closed-database error path.
    let first = gate
        .try_acquire(Purpose::Management)
        .expect("first slot released");
    let second = gate
        .try_acquire(Purpose::Management)
        .expect("second slot released");
    assert!(matches!(
        gate.try_acquire(Purpose::Management),
        Err(GateError::Busy)
    ));
    drop((first, second));

    let directory = StateDirectory::new(fixture._root.path().join("state"), None)
        .expect("reopen test state directory");
    let reopened =
        StateDatabase::open(directory, fixture.clock.clone()).expect("reopen database after close");
    let row_count: i64 =
        reopened
            .read(|connection| {
                Ok(connection
                    .query_row("SELECT count(*) FROM pack_passwords", [], |row| row.get(0))?)
            })
            .expect("confirm failed closed operation persisted nothing");
    assert_eq!(row_count, 0);
    reopened.close().expect("close reopened database");
}
