//! Deterministic fault/reopen harness. New test-owned roots only; no real process.
use super::*;
fn owned() -> Fixture {
    let f = Fixture::open();
    let root = f._temp.path().canonicalize().unwrap();
    assert!(f.game.canonicalize().unwrap().starts_with(&root));
    assert!(
        f.db.directory()
            .path()
            .canonicalize()
            .unwrap()
            .starts_with(&root)
    );
    fs::write(root.join("recovery-assurance-owned"), b"fixture-only").unwrap();
    fs::create_dir(f.game.join("world")).unwrap();
    fs::write(f.game.join("world/level.dat"), b"world witness").unwrap();
    f.put("config/managed.properties", b"managed witness");
    f
}
fn witnesses(f: &Fixture) {
    assert_eq!(f.get("world/level.dat").unwrap(), b"world witness");
    assert_eq!(
        f.get("config/managed.properties").unwrap(),
        b"managed witness"
    );
}
fn scalar(f: &Fixture, sql: &str) -> i64 {
    f.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}
fn status(f: &Fixture) -> String {
    f.db.read(|c| {
        Ok(c.query_row(
            "SELECT status FROM file_intents ORDER BY created_at,id LIMIT 1",
            [],
            |r| r.get(0),
        )?)
    })
    .unwrap()
}
fn converged(f: &mut Fixture, expected: &[u8], expected_status: &str) {
    for round in 0..3 {
        reopen(f);
        f.files.reconcile_pending().unwrap();
        assert_eq!(
            f.get("config/target.txt").unwrap(),
            expected,
            "reopen {round}"
        );
        if expected_status == "recovery-required" {
            assert_eq!(status(f), expected_status);
        } else {
            assert_eq!(
                scalar(f, "SELECT count(*) FROM file_intents"),
                0,
                "terminal intents retire only after audit/cleanup"
            );
            let outcome:String=f.db.read(|c|Ok(c.query_row("SELECT outcome FROM audit_events WHERE source='files' ORDER BY seq DESC LIMIT 1",[],|r|r.get(0))?)).unwrap();
            assert_eq!(
                outcome,
                if expected_status == "complete" {
                    "success"
                } else {
                    "aborted"
                }
            );
        }
        witnesses(f);
        assert!(
            AuditLog::new(f.db.clone())
                .unwrap()
                .verify_chain()
                .unwrap()
                .valid
        );
    }
}
#[test]
fn recovery_assurance_effect_boundaries_converge_across_three_database_reopens() {
    for repeat in 0..2 {
        for same in [false, true] {
            for point in ["before", "after"] {
                let mut f = owned();
                f.put("config/target.txt", b"old");
                let new = if same {
                    b"old".as_slice()
                } else {
                    b"new".as_slice()
                };
                journal::interrupt_at(point);
                assert!(matches!(
                    f.files.write(
                        &f.owner,
                        &Fixture::path("config/target.txt"),
                        &matches(b"old"),
                        new
                    ),
                    Err(FileError::RecoveryRequired(_))
                ));
                assert_eq!(
                    scalar(
                        &f,
                        "SELECT count(*) FROM file_intents WHERE status='pending'"
                    ),
                    1
                );
                assert_eq!(
                    scalar(&f, "SELECT count(*) FROM audit_events WHERE source='files'"),
                    0
                );
                let expected = if point == "before" {
                    b"old".as_slice()
                } else {
                    new
                };
                let state = if point == "before" {
                    "aborted"
                } else if same {
                    "recovery-required"
                } else {
                    "complete"
                };
                converged(&mut f, expected, state);
                assert_eq!(
                    scalar(&f, "SELECT count(*) FROM audit_events WHERE source='files'"),
                    1,
                    "no duplicate reconciliation audits repeat{repeat}"
                );
                assert_eq!(
                    scalar(
                        &f,
                        "SELECT count(*) FROM audit_events WHERE source='files' AND outcome='success'"
                    ),
                    if state == "complete" { 1 } else { 0 }
                );
            }
        }
    }
}
#[test]
fn recovery_assurance_reconciliation_commit_or_audit_failure_preserves_proof_for_retry() {
    for commit in [false, true] {
        let mut f = owned();
        f.put("config/target.txt", b"old");
        journal::interrupt_at("after");
        assert!(
            f.files
                .write(
                    &f.owner,
                    &Fixture::path("config/target.txt"),
                    &matches(b"old"),
                    b"new"
                )
                .is_err()
        );
        f.db.write(|c|{c.execute_batch(if commit {"CREATE TABLE recovery_failure (principal TEXT REFERENCES principals(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER recovery_fail AFTER INSERT ON audit_events WHEN NEW.source='files' BEGIN INSERT INTO recovery_failure VALUES('missing'); END;"}else{"CREATE TRIGGER recovery_fail BEFORE INSERT ON audit_events WHEN NEW.source='files' BEGIN SELECT RAISE(ABORT,'owned-fixture'); END;"})?;Ok(())}).unwrap();
        for _ in 0..2 {
            reopen(&mut f);
            assert!(f.files.reconcile_pending().is_err());
            assert_eq!(status(&f), "pending");
            assert_eq!(f.get("config/target.txt").unwrap(), b"new");
            assert_eq!(
                scalar(&f, "SELECT count(*) FROM audit_events WHERE source='files'"),
                0
            );
            witnesses(&f);
        }
        f.db.write(|c| {
            c.execute_batch("DROP TRIGGER recovery_fail")?;
            Ok(())
        })
        .unwrap();
        converged(&mut f, b"new", "complete");
        assert_eq!(
            scalar(
                &f,
                "SELECT count(*) FROM audit_events WHERE source='files' AND outcome='success'"
            ),
            1
        );
    }
}
#[test]
fn recovery_assurance_effect_completion_commit_failure_compensates_without_false_success() {
    for commit in [false, true] {
        let mut f = owned();
        f.put("config/target.txt", b"old");
        f.db.write(|c|{c.execute_batch(if commit {"CREATE TABLE recovery_failure (principal TEXT REFERENCES principals(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER recovery_fail AFTER INSERT ON audit_events WHEN NEW.action='files.write' BEGIN INSERT INTO recovery_failure VALUES('missing'); END;"}else{"CREATE TRIGGER recovery_fail BEFORE INSERT ON audit_events WHEN NEW.action='files.write' BEGIN SELECT RAISE(ABORT,'owned-fixture'); END;"})?;Ok(())}).unwrap();
        let effect = std::rc::Rc::new(std::cell::Cell::new(false));
        let observed = effect.clone();
        journal::after_write_for_test(move || observed.set(true));
        assert!(
            f.files
                .write(
                    &f.owner,
                    &Fixture::path("config/target.txt"),
                    &matches(b"old"),
                    b"new"
                )
                .is_err()
        );
        assert!(
            effect.get(),
            "fault must follow the actual native effect before compensation"
        );
        assert_eq!(f.get("config/target.txt").unwrap(), b"old");
        assert_eq!(
            scalar(
                &f,
                "SELECT count(*) FROM audit_events WHERE source='files' AND outcome='success'"
            ),
            0
        );
        f.db.write(|c| {
            c.execute_batch("DROP TRIGGER recovery_fail")?;
            Ok(())
        })
        .unwrap();
        converged(&mut f, b"old", "aborted");
        assert_eq!(f.stages("config"), 0);
    }
}
#[test]
fn recovery_assurance_injected_storage_full_before_and_during_staging_is_not_success() {
    for point in ["pre_stage", "stage_write"] {
        let mut f = owned();
        f.put("config/target.txt", b"old");
        journal::fail_storage_at(point);
        assert!(
            f.files
                .write(
                    &f.owner,
                    &Fixture::path("config/target.txt"),
                    &matches(b"old"),
                    &vec![3; 4096]
                )
                .is_err()
        );
        assert!(
            journal::storage_fault_consumed(),
            "the requested ENOSPC seam must actually fire"
        );
        assert_eq!(f.get("config/target.txt").unwrap(), b"old");
        assert_eq!(f.stages("config"), 0);
        for _ in 0..3 {
            reopen(&mut f);
            f.files.reconcile_pending().unwrap();
            assert!(
                journal::storage_fault_consumed(),
                "the requested ENOSPC seam must actually fire"
            );
            assert_eq!(f.get("config/target.txt").unwrap(), b"old");
            assert_eq!(
                scalar(
                    &f,
                    "SELECT count(*) FROM file_intents WHERE status='pending'"
                ),
                0
            );
            assert_eq!(
                scalar(
                    &f,
                    "SELECT count(*) FROM audit_events WHERE source='files' AND outcome='success'"
                ),
                0
            );
            witnesses(&f);
        }
    }
}
fn stage(f: &Fixture, count: usize) -> Vec<String> {
    let rows: Vec<_> = (0..count)
        .map(|n| {
            (
                format!("config/pack/{n}.properties"),
                None,
                format!("file-{n}").into_bytes(),
                None,
            )
        })
        .collect();
    f.db.write_with(|tx| {
        f.files
            .stage_pack_in(tx.connection(), &f.owner, "recovery-pack", &rows)
    })
    .unwrap()
}
fn pending(f: &Fixture) -> i64 {
    scalar(
        f,
        "SELECT count(*) FROM server_content_changes WHERE state='pending'",
    )
}
fn probe(f: &Fixture) -> usize {
    crate::operations::recovery_start_probe(f.files.clone(), &f.game)
}
#[test]
fn recovery_assurance_partial_pack_rolls_forward_once_and_start_drains_before_native_boundary() {
    let mut f = owned();
    let ids = stage(&f, 6);
    assert!(f.db.directory().path().join("content-pending").exists());
    assert!(
        f.files
            .apply_pack_ids_with_progress(&ids, |index, _| if index == 3 {
                Err(FileError::Unavailable)
            } else {
                Ok(())
            })
            .is_err()
    );
    assert_eq!(pending(&f), 3);
    for n in 0..6 {
        assert_eq!(
            f.game.join(format!("config/pack/{n}.properties")).exists(),
            n < 3
        );
    }
    for round in 0..3 {
        reopen(&mut f);
        assert_eq!(
            probe(&f),
            1,
            "native boundary only after durable drain round{round}"
        );
        assert_eq!(pending(&f), 0);
        assert!(!f.db.directory().path().join("content-pending").exists());
        for n in 0..6 {
            assert_eq!(
                f.get(&format!("config/pack/{n}.properties")).unwrap(),
                format!("file-{n}").as_bytes()
            );
        }
        witnesses(&f);
    }
    assert_eq!(
        scalar(
            &f,
            "SELECT count(*) FROM audit_events WHERE action='files.write' AND outcome='success'"
        ),
        6
    );
    assert_eq!(
        scalar(
            &f,
            "SELECT count(*) FROM audit_events WHERE action='content.apply'"
        ),
        6
    );
}
#[test]
fn recovery_assurance_content_receipt_survives_lost_outer_commit_without_duplicate_effect() {
    let mut f = owned();
    let ids = stage(&f, 2);
    f.db.write(|c|{c.execute_batch("CREATE TRIGGER reject_content_commit BEFORE INSERT ON audit_events WHEN NEW.action='content.apply' BEGIN SELECT RAISE(ABORT,'owned-fixture'); END;")?;Ok(())}).unwrap();
    assert!(f.files.apply_pack_ids(&ids).is_err());
    assert_eq!(pending(&f), 2);
    assert_eq!(f.get("config/pack/0.properties").unwrap(), b"file-0");
    assert!(!f.game.join("config/pack/1.properties").exists());
    assert_eq!(
        scalar(
            &f,
            "SELECT count(*) FROM audit_events WHERE action='files.write' AND outcome='success'"
        ),
        1
    );
    for _ in 0..2 {
        reopen(&mut f);
        assert_eq!(probe(&f), 0, "failed completion keeps native start blocked");
        assert_eq!(pending(&f), 2);
        assert_eq!(
            scalar(
                &f,
                "SELECT count(*) FROM audit_events WHERE action='files.write' AND outcome='success'"
            ),
            1
        );
        witnesses(&f);
    }
    f.db.write(|c| {
        c.execute_batch("DROP TRIGGER reject_content_commit")?;
        Ok(())
    })
    .unwrap();
    for _ in 0..3 {
        reopen(&mut f);
        assert_eq!(probe(&f), 1);
        assert_eq!(pending(&f), 0);
        assert_eq!(
            scalar(
                &f,
                "SELECT count(*) FROM audit_events WHERE action='files.write' AND outcome='success'"
            ),
            2
        );
        assert_eq!(
            scalar(
                &f,
                "SELECT count(*) FROM audit_events WHERE action='content.apply'"
            ),
            2
        );
        witnesses(&f);
    }
}
#[test]
fn recovery_assurance_revoked_actor_pending_pack_blocks_start_and_preserves_witnesses() {
    let mut f = owned();
    stage(&f, 2);
    crate::identity::management::ManagementAccess::new(f.db.clone())
        .unwrap()
        .revoke_local(f.owner.device.as_str())
        .unwrap();
    for _ in 0..3 {
        reopen(&mut f);
        assert_eq!(probe(&f), 0);
        assert_eq!(pending(&f), 2);
        assert!(!f.game.join("config/pack/0.properties").exists());
        assert_eq!(
            scalar(
                &f,
                "SELECT count(*) FROM audit_events WHERE action='files.write' AND outcome='success'"
            ),
            0
        );
        assert!(f.db.directory().path().join("content-pending").exists());
        witnesses(&f);
    }
}
#[test]
fn recovery_assurance_substituted_payload_and_parent_refuse_after_repeated_reopen() {
    for payload in [true, false] {
        let mut f = owned();
        if payload {
            let store = crate::packs::payload::Store::open(&f.db).unwrap();
            let value = store
                .put(&mut std::io::Cursor::new(b"good".to_vec()))
                .unwrap();
            let rows = vec![("config/pack/0.properties".into(), None, value.clone(), None)];
            f.db.write_with(|tx| {
                f.files
                    .stage_pack_payloads_in(tx.connection(), &f.owner, "payload-pack", &rows)
            })
            .unwrap();
            let path =
                f.db.directory()
                    .path()
                    .join("pack-payloads")
                    .join(format!("blob-{}", value.sha256));
            assert!(
                path.canonicalize()
                    .unwrap()
                    .starts_with(f._temp.path().canonicalize().unwrap())
            );
            fs::write(path, b"evil").unwrap();
            drop(store);
        } else {
            stage(&f, 1);
            fs::write(f.game.join("config/pack"), b"parent witness").unwrap();
        }
        for _ in 0..3 {
            reopen(&mut f);
            assert_eq!(probe(&f), 0);
            assert_eq!(pending(&f), 1);
            assert!(!f.game.join("config/pack/0.properties").exists());
            if !payload {
                assert_eq!(f.get("config/pack").unwrap(), b"parent witness");
            }
            witnesses(&f);
        }
    }
}
#[test]
fn recovery_assurance_link_substituted_stage_retains_identity_evidence_after_reopen() {
    let mut f = owned();
    f.put("config/target.txt", b"old");
    journal::interrupt_at("before");
    assert!(
        f.files
            .write(
                &f.owner,
                &Fixture::path("config/target.txt"),
                &matches(b"old"),
                b"new"
            )
            .is_err()
    );
    let name: String =
        f.db.read(|c| {
            Ok(c.query_row(
                "SELECT json_extract(details_json,'$.stage') FROM file_intents",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert!(!name.contains('/') && !name.contains('\\'));
    let stage = f.game.join("config").join(name);
    assert!(
        stage
            .canonicalize()
            .unwrap()
            .starts_with(f._temp.path().canonicalize().unwrap())
    );
    let witness = f._temp.path().join("same-hash-alias-witness");
    fs::write(&witness, b"new").unwrap();
    fs::remove_file(&stage).unwrap();
    fs::hard_link(&witness, &stage).unwrap();
    for _ in 0..3 {
        reopen(&mut f);
        f.files.reconcile_pending().unwrap();
        assert_eq!(status(&f), "recovery-required");
        assert_eq!(fs::read(&stage).unwrap(), b"new");
        assert_eq!(fs::read(&witness).unwrap(), b"new");
        assert_eq!(f.get("config/target.txt").unwrap(), b"old");
        assert_eq!(
            scalar(
                &f,
                "SELECT count(*) FROM audit_events WHERE source='files' AND outcome='success'"
            ),
            0
        );
        witnesses(&f);
    }
    assert_eq!(
        scalar(
            &f,
            "SELECT count(*) FROM audit_events WHERE source='files' AND outcome='recovery-required'"
        ),
        1
    );
}
