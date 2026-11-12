//! Additional application-boundary faults; real KDF, SQLite and writer admission.
use super::*;
use std::{
    sync::{Mutex, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};

#[test]
fn admitted_missing_invalid_and_mismatched_attempts_are_audited() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let absent = service
        .metadata(&fixture.admin)
        .expect("authorized absence");
    assert_eq!(absent.revision(), PasswordRevision::ABSENT);
    assert!(!absent.active());
    assert_eq!(
        service
            .verify(b"valid but absent".to_vec())
            .expect("absent attempt")
            .status(),
        PasswordVerificationStatus::Unavailable
    );
    service
        .set(
            &fixture.admin,
            PasswordRevision::ABSENT,
            &password("exact password"),
        )
        .expect("create");
    for bytes in [Vec::new(), vec![0xff], vec![b'x'; 4097]] {
        assert_eq!(
            service
                .verify(bytes)
                .expect("audited invalid input")
                .status(),
            PasswordVerificationStatus::InvalidPassword
        );
    }
    assert_eq!(
        service
            .verify(b"different password".to_vec())
            .expect("mismatch")
            .status(),
        PasswordVerificationStatus::Mismatch
    );
    let entries = fixture.audit_entries();
    let recorded = entries
        .iter()
        .filter(|entry| entry.action == VERIFY)
        .map(|entry| {
            let metadata: serde_json::Value =
                serde_json::from_str(&entry.metadata_json).expect("verification metadata");
            (
                entry.outcome.as_str(),
                metadata["revision"].as_str().expect("revision").to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        recorded,
        vec![
            ("unavailable", "0".to_owned()),
            ("invalid_password", "1".to_owned()),
            ("invalid_password", "1".to_owned()),
            ("invalid_password", "1".to_owned()),
            ("mismatch", "1".to_owned())
        ]
    );
    assert!(fixture.audit.verify_chain().expect("chain").valid);
}

#[test]
fn credential_revision_and_verifier_survive_reopen_without_an_authority_change() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let expected = service
        .set(
            &fixture.admin,
            PasswordRevision::ABSENT,
            &password("persistent pack password"),
        )
        .expect("create");
    fixture.db.close().expect("close original state");
    let directory =
        StateDirectory::new(fixture._root.path().join("state"), None).expect("same directory");
    let reopened = StateDatabase::open(directory, fixture.clock.clone()).expect("reopen state");
    let service = PackPasswords::compose(
        reopened.clone(),
        fast_policy(),
        KdfGate::new(GateConfig::default()).expect("test gate"),
    )
    .expect("recompose");
    assert_eq!(
        service
            .metadata(&fixture.admin)
            .expect("current authority after reopen"),
        expected
    );
    assert_eq!(
        service
            .verify(b"persistent pack password".to_vec())
            .expect("verify after reopen")
            .status(),
        PasswordVerificationStatus::Match
    );
    assert_forbidden(
        &service
            .metadata(&fixture.viewer)
            .expect_err("viewer still lacks management"),
        DenyReason::Reserved,
    );
    assert!(
        AuditLog::new(reopened.clone())
            .expect("audit after reopen")
            .verify_chain()
            .expect("chain")
            .valid
    );
    reopened.close().expect("close reopened state");
}

#[test]
fn revision_is_rechecked_after_kdf_and_exhaustion_prevents_derivation() {
    let fixture = Fixture::new();
    let mut service = fixture.service();
    let creator = fixture.service();
    let admin = fixture.admin.clone();
    service.before_commit = Some(Arc::new(move || {
        creator
            .set(
                &admin,
                PasswordRevision::ABSENT,
                &password("winning creator"),
            )
            .expect("competing create");
    }));
    let rejected = service
        .set(
            &fixture.admin,
            PasswordRevision::ABSENT,
            &password("losing creator"),
        )
        .expect_err("final revision conflict");
    assert!(matches!(
        rejected,
        ControlError::Rejected {
            code: crate::IdentityErrorCode::CredentialConflict,
            reason: None
        }
    ));
    assert_eq!(fixture.row().expect("only winning row").0, 1);
    service.before_commit = None;
    assert_eq!(
        service
            .verify(b"winning creator".to_vec())
            .expect("winning password")
            .status(),
        PasswordVerificationStatus::Match
    );
    assert_eq!(
        service
            .verify(b"losing creator".to_vec())
            .expect("losing password")
            .status(),
        PasswordVerificationStatus::Mismatch
    );
    fixture
        .db
        .write(|connection| {
            connection.execute("UPDATE pack_passwords SET revision=?", [i64::MAX])?;
            Ok(())
        })
        .expect("maximum persisted revision");
    let before = super::super::super::password::test_work_counts();
    let exhausted = service
        .set(
            &fixture.admin,
            revision(i64::MAX as u64),
            &password("cannot wrap"),
        )
        .expect_err("overflow precheck");
    assert!(matches!(
        exhausted,
        ControlError::Rejected {
            code: crate::IdentityErrorCode::CredentialRevisionExhausted,
            reason: None
        }
    ));
    assert_eq!(super::super::super::password::test_work_counts(), before);
    assert_eq!(fixture.row().expect("unchanged maximum").0, i64::MAX);
    assert!(fixture.audit.verify_chain().expect("chain").valid);
}

#[test]
fn separate_refusal_audit_failure_preserves_the_exact_primary_rejection() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service
        .set(
            &fixture.admin,
            PasswordRevision::ABSENT,
            &password("refusal audit fixture"),
        )
        .expect("create");
    let original = fixture.row();
    let audit_count = fixture.count("audit_events");
    fixture.db.write(|connection| {
        connection.execute_batch("CREATE TRIGGER fail_refusal_audit BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'private fixture text'); END;")?;
        Ok(())
    }).expect("fault refusal audit");
    for (outcome, code, reason) in [
        (
            service.set(&fixture.viewer, revision(1), &password("not management")),
            crate::IdentityErrorCode::Forbidden,
            Some(DenyReason::Reserved),
        ),
        (
            service.set(
                &fixture.admin,
                PasswordRevision::ABSENT,
                &password("stale revision"),
            ),
            crate::IdentityErrorCode::CredentialConflict,
            None,
        ),
        (
            service.revoke(&fixture.admin, PasswordRevision::ABSENT),
            crate::IdentityErrorCode::InvalidRequest,
            None,
        ),
    ] {
        let error = outcome.expect_err("refusal audit failure is not a successful refusal");
        assert!(matches!(&error, ControlError::AuditUnavailable { .. }));
        assert_eq!(error.primary(), &ControlError::Rejected { code, reason });
        assert_eq!(
            error.secondary().expect("secondary state error").code(),
            "state_io"
        );
        assert!(!format!("{error:?}").contains("private fixture text"));
        assert_eq!(fixture.row(), original);
        assert_eq!(fixture.count("audit_events"), audit_count);
    }
    assert!(fixture.audit.verify_chain().expect("unchanged chain").valid);
}

#[test]
fn successful_audit_insertion_followed_by_failed_commit_never_returns_match_or_mutates_credentials()
{
    let fixture = Fixture::new();
    let service = fixture.service();
    service
        .set(
            &fixture.admin,
            PasswordRevision::ABSENT,
            &password("commit fault password"),
        )
        .expect("create");
    let original = fixture.row();
    let audit_count = fixture.count("audit_events");
    fixture.db.write(|connection| {
        connection.execute_batch(
            "CREATE TABLE commit_parent (id INTEGER PRIMARY KEY);\
             CREATE TABLE commit_guard (id INTEGER REFERENCES commit_parent(id) DEFERRABLE INITIALLY DEFERRED);\
             CREATE TRIGGER fail_pack_commit AFTER INSERT ON audit_events BEGIN INSERT INTO commit_guard VALUES (1); END;"
        )?;
        Ok(())
    }).expect("install deferred commit fault, not an audit-insert abort");
    for outcome in [
        service
            .set(
                &fixture.admin,
                revision(1),
                &password("uncommitted rotation"),
            )
            .map(|_| ()),
        service.revoke(&fixture.admin, revision(1)).map(|_| ()),
        service
            .verify(b"commit fault password".to_vec())
            .map(|_| ()),
    ] {
        let error = outcome.expect_err("deferred constraint fails COMMIT after work succeeds");
        assert!(
            matches!(&error, ControlError::State(_)),
            "commit failure is infrastructure"
        );
        assert_eq!(
            error.code(),
            "state_commit_failed",
            "work and audit insertion succeeded; COMMIT failed"
        );
        assert_eq!(
            fixture.row(),
            original,
            "credential and audit rollback together"
        );
        assert_eq!(fixture.count("audit_events"), audit_count);
        let guards: i64 = fixture
            .db
            .read(|connection| {
                Ok(connection
                    .query_row("SELECT count(*) FROM commit_guard", [], |row| row.get(0))?)
            })
            .expect("rolled-back guard insert");
        assert_eq!(guards, 0);
    }
    fixture
        .db
        .write(|connection| {
            connection.execute_batch("DROP TRIGGER fail_pack_commit")?;
            Ok(())
        })
        .expect("remove commit fault");
    assert_eq!(
        service
            .verify(b"commit fault password".to_vec())
            .expect("retry after rollback")
            .status(),
        PasswordVerificationStatus::Match
    );
    assert!(fixture.audit.verify_chain().expect("chain").valid);
}

// Both ownership handles are drained before assertions; Drop and the callback timeout
// also prevent an assertion panic from leaving a permanent admitted writer behind.
struct HeldWriter {
    release: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<vesta_state::Result<()>>>,
}
impl HeldWriter {
    fn start(db: StateDatabase) -> Self {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            db.write(|_| {
                let _ = entered_tx.send(());
                release_rx
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|_| StateError::new("state_test_release_timeout"))?;
                Ok(())
            })
        });
        let holder = Self {
            release: Some(release),
            worker: Some(worker),
        };
        entered
            .recv_timeout(Duration::from_secs(5))
            .expect("real writer is admitted");
        holder
    }
    fn finish(&mut self) -> vesta_state::Result<()> {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        self.worker
            .take()
            .expect("owned writer")
            .join()
            .expect("writer did not panic")
    }
}
impl Drop for HeldWriter {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[test]
fn authority_time_is_sampled_only_after_final_writer_admission() {
    let fixture = Fixture::new();
    fixture
        .db
        .write(|connection| {
            connection.execute(
                "UPDATE role_assignments SET expires_at=? WHERE principal_id=?",
                params![NOW + 1, fixture.admin.principal.as_str()],
            )?;
            Ok(())
        })
        .expect("administrator about to expire");
    let mut holder = HeldWriter::start(fixture.db.clone());
    let (derived_tx, derived) = mpsc::channel();
    let (continue_tx, continue_rx) = mpsc::channel();
    let continue_rx = Mutex::new(continue_rx);
    let mut service = fixture.service();
    service.before_commit = Some(Arc::new(move || {
        derived_tx.send(()).expect("report completed derivation");
        continue_rx
            .lock()
            .expect("hook gate")
            .recv_timeout(Duration::from_secs(5))
            .expect("continue final mutation");
    }));
    let admin = fixture.admin.clone();
    let request = thread::spawn(move || {
        service.set(
            &admin,
            PasswordRevision::ABSENT,
            &password("expires while queued"),
        )
    });
    let reached_hook = derived.recv_timeout(Duration::from_secs(5)).is_ok();
    let sample = fixture.clock.watch_next_sample();
    let _ = continue_tx.send(());
    let premature = sample.recv_timeout(Duration::from_millis(200)).is_ok();
    fixture.clock.set(NOW + 2);
    let holder_result = holder.finish();
    let outcome = request.join();
    assert!(
        reached_hook,
        "precheck and real KDF complete while another writer is held"
    );
    assert!(
        !premature,
        "final authority time is not sampled before writer admission"
    );
    holder_result.expect("holder released cleanly");
    let error = outcome
        .expect("request did not panic")
        .expect_err("expired authority cannot set password");
    assert_forbidden(&error, DenyReason::Reserved);
    assert_eq!(fixture.count("pack_passwords"), 0);
    let entries = fixture.audit_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].outcome, "denied");
    assert_eq!(entries[0].at_ms, NOW + 2);
    assert!(fixture.audit.verify_chain().expect("chain").valid);
}

#[test]
fn production_factories_share_concurrency_even_across_databases() {
    let first_fixture = Fixture::new();
    let second_fixture = Fixture::new();
    let first = PackPasswords::new(first_fixture.db.clone()).expect("first production factory");
    let same_db =
        PackPasswords::new(first_fixture.db.clone()).expect("another factory same database");
    let other_db =
        PackPasswords::new(second_fixture.db.clone()).expect("factory different database");
    let one = first
        .gate
        .try_acquire(Purpose::Verification)
        .expect("first process slot");
    let two = same_db
        .gate
        .try_acquire(Purpose::Management)
        .expect("second process slot");
    assert!(
        matches!(
            other_db.gate.try_acquire(Purpose::Verification),
            Err(GateError::Busy)
        ),
        "factory is not a budget reset"
    );
    drop((one, two));
    let replacement_one = other_db
        .gate
        .try_acquire(Purpose::Verification)
        .expect("reclaim first slot");
    let replacement_two = first
        .gate
        .try_acquire(Purpose::Management)
        .expect("reclaim second slot");
    drop((replacement_one, replacement_two));
}
