//! Intent retention, aggregate reservation, and bounded maintenance regressions.
use super::*;

fn intent_rows(f: &Fixture) -> i64 {
    f.db.read(|c| Ok(c.query_row("SELECT count(*) FROM file_intents", [], |r| r.get(0))?))
        .unwrap()
}

/// Legacy v11 rows have no staged_bytes reservation field.
fn seed_intents(f: &Fixture, count: usize, status: &str, size: u64) {
    f.db.write(|c| {
        for n in 0..count {
            let id = format!("{n:032x}");
            let intent = Intent {
                effect: (size != 0).then(|| sha(b"fixture")),
                stage: (size != 0).then(|| format!("{STAGE_PREFIX}{id}.tmp")),
                size,
                ..Intent::default()
            };
            let mut json = serde_json::to_value(&intent).unwrap();
            json.as_object_mut().unwrap().remove("staged_bytes");
            c.execute("INSERT INTO file_intents (id,server_id,principal_id,device_id,kind,path,details_json,status,created_at) VALUES (?,?,?,?,?,?,?,?,?)",
                params![id, f.db.server_id(), f.owner.principal.as_str(), f.owner.device.as_str(), if size == 0 { "mkdir" } else { "write" }, format!("config/reserved-{n}.txt"), json.to_string(), status, START])?;
        }
        Ok(())
    }).unwrap();
}

#[test]
fn sequential_writes_retire_intents_without_losing_audit_evidence() {
    let f = Fixture::open();
    let count = MAX_FILE_INTENTS + 1;
    for n in 0..count {
        f.files
            .write(
                &f.owner,
                &Fixture::path(&format!("config/write-{n}.txt")),
                &WritePrecondition::Absent,
                b"new",
            )
            .unwrap();
        assert_eq!(intent_rows(&f), 0);
        assert_eq!(f.stages("config"), 0);
    }
    let audits: i64 = f.db.read(|c| Ok(c.query_row("SELECT count(*) FROM audit_events WHERE action='files.write' AND outcome='success' AND operation_id IS NOT NULL", [], |r| r.get(0))?)).unwrap();
    assert_eq!(audits, count as i64);
    assert!(
        AuditLog::new(f.db.clone())
            .unwrap()
            .verify_chain()
            .unwrap()
            .valid
    );
}

#[test]
fn intent_count_cap_refuses_every_mutation_before_any_staging() {
    for kind in [
        "create",
        "overwrite",
        "delete",
        "mkdir",
        "rename",
        "restore",
    ] {
        let f = Fixture::open();
        let path = Fixture::path("config/target.txt");
        if matches!(kind, "overwrite" | "delete" | "rename" | "restore") {
            f.put(path.as_str(), b"old");
        }
        let restore = if kind == "restore" {
            Some(f.files.delete(&f.owner, &path, &sha(b"old")).unwrap().id)
        } else {
            None
        };
        let trash = (f.trash_rows(), f.trash_files());
        seed_intents(&f, MAX_FILE_INTENTS, "recovery-required", 0);
        let result = match kind {
            "create" => f
                .files
                .write(&f.owner, &path, &WritePrecondition::Absent, b"new")
                .map(|_| ()),
            "overwrite" => f
                .files
                .write(&f.owner, &path, &matches(b"old"), b"new")
                .map(|_| ()),
            "delete" => f.files.delete(&f.owner, &path, &sha(b"old")).map(|_| ()),
            "mkdir" => f.files.mkdir(&f.owner, &path),
            "rename" => f
                .files
                .rename(
                    &f.owner,
                    &path,
                    &Fixture::path("config/destination.txt"),
                    &sha(b"old"),
                )
                .map(|_| ()),
            "restore" => f
                .files
                .restore(&f.owner, restore.as_deref().unwrap())
                .map(|_| ()),
            _ => unreachable!(),
        };
        let error = result.unwrap_err();
        assert_eq!(error.code(), "file_intents_full", "{kind}");
        assert_eq!(format!("{error}"), "File operation failed");
        assert_eq!(
            format!("{error:?}"),
            "FileError { code: \"file_intents_full\" }"
        );
        assert_eq!(f.stages("config"), 0);
        assert_eq!((f.trash_rows(), f.trash_files()), trash);
        assert_eq!(intent_rows(&f), MAX_FILE_INTENTS as i64);
        assert_eq!(
            f.get(path.as_str()),
            matches!(kind, "overwrite" | "delete" | "rename").then(|| b"old".to_vec())
        );
        assert!(!f.game.join("config/destination.txt").exists());
        assert!(!f.game.join(path.as_str()).is_dir());
        assert_eq!(f.files.reconcile_pending().unwrap(), 0);
        assert_eq!(intent_rows(&f), MAX_FILE_INTENTS as i64);
    }
}

#[test]
fn recovery_required_legacy_stages_remain_reserved_against_byte_cap() {
    let f = Fixture::open();
    seed_intents(&f, 4, "recovery-required", MAX_TRASH_FILE_BYTES);
    assert_eq!(MAX_INTENT_STAGED_BYTES, 4 * MAX_TRASH_FILE_BYTES);
    refused(
        f.files.write(
            &f.owner,
            &Fixture::path("config/new.txt"),
            &WritePrecondition::Absent,
            b"x",
        ),
        "file_intents_full",
    );
    assert_eq!(f.stages("config"), 0);
    assert!(!f.game.join("config/new.txt").exists());
    assert_eq!(f.files.reconcile_pending().unwrap(), 0);
    assert_eq!(intent_rows(&f), 4);
    f.files
        .mkdir(&f.owner, &Fixture::path("config/new-dir"))
        .unwrap();
    assert_eq!(intent_rows(&f), 4);
}

#[test]
fn byte_reservation_allows_exact_boundary_and_charges_compensation() {
    let f = Fixture::open();
    seed_intents(&f, 1, "recovery-required", MAX_INTENT_STAGED_BYTES - 3);
    f.files
        .write(
            &f.owner,
            &Fixture::path("config/exact.txt"),
            &WritePrecondition::Absent,
            b"new",
        )
        .unwrap();
    assert_eq!(intent_rows(&f), 1);
    assert_eq!(f.stages("config"), 0);
    f.put("config/old.txt", b"larger old bytes");
    refused(
        f.files.write(
            &f.owner,
            &Fixture::path("config/old.txt"),
            &matches(b"larger old bytes"),
            b"x",
        ),
        "file_intents_full",
    );
    refused(
        f.files.delete(
            &f.owner,
            &Fixture::path("config/old.txt"),
            &sha(b"larger old bytes"),
        ),
        "file_intents_full",
    );
    assert_eq!(f.get("config/old.txt").unwrap(), b"larger old bytes");
    assert_eq!(
        (f.trash_rows(), f.trash_files(), f.stages("config")),
        (0, 0, 0)
    );
}

#[test]
fn reconciliation_and_owned_stage_cleanup_span_multiple_batches() {
    let mut f = Fixture::open();
    let count = RECONCILE_BATCH_SIZE * 2 + 3;
    for n in 0..count {
        journal::interrupt_at("before");
        refused(
            f.files.write(
                &f.owner,
                &Fixture::path(&format!("config/pending-{n}.txt")),
                &WritePrecondition::Absent,
                b"new",
            ),
            "file_recovery_required",
        );
    }
    assert_eq!(intent_rows(&f), count as i64);
    assert_eq!(f.stages("config"), count);
    reopen(&mut f);
    assert_eq!(f.files.reconcile_pending().unwrap(), count);
    assert_eq!(intent_rows(&f), 0);
    assert_eq!(f.stages("config"), 0);
    assert_eq!(f.files.reconcile_pending().unwrap(), 0);
    let audits: i64 = f.db.read(|c| Ok(c.query_row("SELECT count(*) FROM audit_events WHERE action='files.write' AND outcome='aborted' AND json_extract(metadata_json,'$.reconciled')='true' AND operation_id IS NOT NULL", [], |r| r.get(0))?)).unwrap();
    assert_eq!(audits, count as i64);
    assert!(
        AuditLog::new(f.db.clone())
            .unwrap()
            .verify_chain()
            .unwrap()
            .valid
    );
}

#[test]
fn pending_rows_count_against_admission_and_are_only_retired_by_reconciliation() {
    let f = Fixture::open();
    seed_intents(&f, MAX_FILE_INTENTS, "pending", 0);
    refused(
        f.files.mkdir(&f.owner, &Fixture::path("config/new-dir")),
        "file_intents_full",
    );
    assert_eq!(intent_rows(&f), MAX_FILE_INTENTS as i64);
    assert!(!f.game.join("config/new-dir").exists());
    assert_eq!(f.files.reconcile_pending().unwrap(), MAX_FILE_INTENTS);
    assert_eq!(intent_rows(&f), 0);
    f.files
        .mkdir(&f.owner, &Fixture::path("config/new-dir"))
        .unwrap();
    assert_eq!(intent_rows(&f), 0);
}

#[test]
fn denied_terminal_retirement_preserves_success_and_capacity_until_retry() {
    let f = Fixture::open();
    seed_intents(&f, MAX_FILE_INTENTS - 1, "recovery-required", 0);
    f.db.write(|c| { c.execute_batch("CREATE TRIGGER reject_retirement BEFORE DELETE ON file_intents BEGIN SELECT RAISE(ABORT,'fixture'); END;")?; Ok(()) }).unwrap();
    f.files
        .write(
            &f.owner,
            &Fixture::path("config/success.txt"),
            &WritePrecondition::Absent,
            b"new",
        )
        .unwrap();
    assert_eq!(f.get("config/success.txt").unwrap(), b"new");
    assert_eq!(f.stages("config"), 0);
    assert_eq!(intent_rows(&f), MAX_FILE_INTENTS as i64);
    refused(
        f.files.write(
            &f.owner,
            &Fixture::path("config/blocked.txt"),
            &WritePrecondition::Absent,
            b"new",
        ),
        "file_intents_full",
    );
    assert!(!f.game.join("config/blocked.txt").exists());
    f.db.write(|c| {
        c.execute_batch("DROP TRIGGER reject_retirement")?;
        Ok(())
    })
    .unwrap();
    assert_eq!(f.files.reconcile_pending().unwrap(), 0);
    assert_eq!(intent_rows(&f), (MAX_FILE_INTENTS - 1) as i64);
    f.files
        .write(
            &f.owner,
            &Fixture::path("config/blocked.txt"),
            &WritePrecondition::Absent,
            b"new",
        )
        .unwrap();
    assert_eq!(intent_rows(&f), (MAX_FILE_INTENTS - 1) as i64);
}

#[test]
fn dropping_an_uncreated_planned_stage_never_removes_an_existing_file() {
    let f = Fixture::open();
    let stage = Stage::planned(&f.game.join("config")).unwrap();
    let path = stage.path.clone();
    fs::write(&path, b"not ours").unwrap();
    assert!(stage.create(b"replacement").is_err());
    drop(stage);
    assert_eq!(fs::read(path).unwrap(), b"not ours");
}

#[test]
fn orphan_trash_reconciliation_also_spans_multiple_bounded_batches() {
    let f = Fixture::open();
    let count = RECONCILE_BATCH_SIZE + 3;
    for n in 0..count {
        let text = format!("config/orphan-{n}.txt");
        f.put(&text, b"old");
        f.files
            .prepare_trash(
                &f.owner,
                &Fixture::path(&text),
                &sha(b"old"),
                Reason::Delete,
                None,
            )
            .unwrap();
    }
    assert_eq!(f.files.reconcile_pending().unwrap(), count);
    assert_eq!(f.files.reconcile_pending().unwrap(), 0);
    assert_eq!((f.trash_rows(), f.trash_files()), (count as i64, count));
    let recovery: i64 =
        f.db.read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM file_trash WHERE status='recovery-required'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(recovery, count as i64);
}

#[test]
fn compensated_abort_is_audited_before_its_intent_is_retired() {
    let f = Fixture::open();
    f.put("config/target.txt", b"old");
    f.db.write(|c| {
        c.execute_batch("CREATE TRIGGER reject_success BEFORE INSERT ON audit_events WHEN NEW.source='files' AND NEW.outcome='success' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;
        Ok(())
    }).unwrap();
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
    assert_eq!(f.get("config/target.txt").unwrap(), b"old");
    assert_eq!(intent_rows(&f), 0);
    assert_eq!(
        (f.trash_rows(), f.trash_files(), f.stages("config")),
        (0, 0, 0)
    );
    let audits: i64 = f.db.read(|c| Ok(c.query_row("SELECT count(*) FROM audit_events WHERE action='files.write' AND outcome='aborted' AND operation_id IS NOT NULL", [], |r| r.get(0))?)).unwrap();
    assert_eq!(audits, 1);
    assert!(
        AuditLog::new(f.db.clone())
            .unwrap()
            .verify_chain()
            .unwrap()
            .valid
    );
}

#[test]
fn interruption_during_compensation_retains_accounted_stage_then_cleans_it() {
    let mut f = Fixture::open();
    f.put("config/target.txt", b"original");
    f.db.write(|c| {
        c.execute_batch("CREATE TRIGGER reject_success BEFORE INSERT ON audit_events WHEN NEW.source='files' AND NEW.outcome='success' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;
        Ok(())
    }).unwrap();
    journal::interrupt_at("compensation");
    refused(
        f.files.write(
            &f.owner,
            &Fixture::path("config/target.txt"),
            &matches(b"original"),
            b"new",
        ),
        "file_recovery_required",
    );
    assert_eq!(f.get("config/target.txt").unwrap(), b"new");
    assert_eq!(f.stages("config"), 1);
    let (status, json): (String, String) =
        f.db.read(|c| {
            Ok(
                c.query_row("SELECT status,details_json FROM file_intents", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?,
            )
        })
        .unwrap();
    assert_eq!(status, "pending");
    let intent: Intent = serde_json::from_str(&json).unwrap();
    assert_eq!(intent.staged_bytes, Some(8));
    let stage = f
        .game
        .join("config")
        .join(intent.compensation_stage.unwrap());
    assert_eq!(fs::read(&stage).unwrap(), b"original");
    f.db.write(|c| {
        c.execute_batch("DROP TRIGGER reject_success")?;
        Ok(())
    })
    .unwrap();
    reopen(&mut f);
    assert_eq!(f.files.reconcile_pending().unwrap(), 1);
    assert_eq!(f.get("config/target.txt").unwrap(), b"new");
    assert_eq!(intent_rows(&f), 0);
    assert_eq!(f.stages("config"), 0);
    assert_eq!((f.trash_rows(), f.trash_files()), (1, 1));
    assert!(
        AuditLog::new(f.db.clone())
            .unwrap()
            .verify_chain()
            .unwrap()
            .valid
    );
}
