use super::*;

fn audit_valid(f: &Fixture) {
    assert!(
        AuditLog::new(f.db.clone())
            .expect("verify actual chain on open")
            .verify_chain()
            .expect("verify actual chain")
            .valid
    );
}
fn install_audit_fault(f: &Fixture) {
    f.sql(|c| {
        c.execute_batch("CREATE TRIGGER fixture_audit_fault BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'private-fault-marker'); END;")?;
        Ok(())
    });
}
fn install_event_fault(f: &Fixture) {
    f.sql(|c| {
        c.execute_batch("CREATE TRIGGER fixture_event_fault BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'private-fault-marker'); END;")?;
        Ok(())
    });
}

#[test]
fn lock_insertions_are_sorted_before_claiming_and_all_or_none_on_second_insert_failure() {
    // A trigger requires A already present when B is inserted: observing sorted
    // rows after the fact alone would not prove actual lock acquisition order.
    let f = Fixture::open();
    let row = f.admit(&["b", "a"], "sorted");
    let mut owner = f.ops.open_owner().expect("owner");
    f.sql(|c| {
        c.execute_batch("CREATE TRIGGER fixture_sorted_locks BEFORE INSERT ON resource_locks
            WHEN NEW.resource='fixture-files:b' AND NOT EXISTS(SELECT 1 FROM resource_locks WHERE resource='fixture-files:a')
            BEGIN SELECT RAISE(ABORT,'lock-order-was-not-sorted'); END;")?;
        Ok(())
    });
    let claim = owner
        .claim_next()
        .expect("actual sorted insertions satisfy order witness")
        .expect("claim");
    assert_eq!(queue::claim_id(&claim), &row.id);
    assert_eq!(
        f.held(&row.id),
        vec!["fixture-files:a".to_owned(), "fixture-files:b".to_owned()]
    );
    owner
        .release_unstarted(claim)
        .expect("release known unstarted claim");
    drop(owner);

    let f = Fixture::open();
    let row = f.admit(&["b", "a"], "rollback-all-locks");
    let mut owner = f.ops.open_owner().expect("owner");
    f.sql(|c| {
        c.execute_batch("CREATE TRIGGER fixture_second_lock_fault BEFORE INSERT ON resource_locks
            WHEN NEW.resource='fixture-files:b' BEGIN SELECT RAISE(ABORT,'private-fault-marker'); END;")?;
        Ok(())
    });
    let before = f.counts();
    let error = code(owner.claim_next(), "state_io");
    assert!(!format!("{error:?} {error}").contains("private-fault-marker"));
    assert_eq!(before, f.counts());
    assert_eq!(f.read(&row.id).state, OperationState::Queued);
    assert!(f.held(&row.id).is_empty() && f.owner(&row.id).is_none());
    code(owner.claim_next(), "state_operation_owner_unhealthy");
    drop(owner);
    f.sql(|c| {
        c.execute_batch("DROP TRIGGER fixture_second_lock_fault")?;
        Ok(())
    });
    let mut owner = f
        .ops
        .open_owner()
        .expect("new owned reconciliation after fault");
    let claim = owner
        .claim_next()
        .expect("restored claim")
        .expect("row was not partly claimed");
    owner
        .release_unstarted(claim)
        .expect("release restored claim");
    audit_valid(&f);
}

#[test]
fn required_audit_and_event_failures_rollback_admission_resources_and_claim_state() {
    for audit in [true, false] {
        let f = Fixture::open();
        if audit {
            install_audit_fault(&f);
        } else {
            install_event_fault(&f);
        }
        let before = f.counts();
        code(
            f.ops.admit(
                &f.subject,
                "fixture.write",
                &raw(&["a", "b"], "requested"),
                Some("fault"),
            ),
            "state_io",
        );
        assert_eq!(before, f.counts());
        let resources =
            f.db.read(|c| count(c, "operation_resources"))
                .expect("resource rollback");
        assert_eq!(resources, 0);
        audit_valid(&f);
    }
    for audit in [true, false] {
        let f = Fixture::open();
        let row = f.admit(&["a", "b"], "fault-claim");
        let mut owner = f.ops.open_owner().expect("owner");
        if audit {
            install_audit_fault(&f);
        } else {
            install_event_fault(&f);
        }
        let before = f.counts();
        code(owner.claim_next(), "state_io");
        assert_eq!(before, f.counts());
        assert_eq!(f.read(&row.id).state, OperationState::Queued);
        assert!(f.held(&row.id).is_empty() && f.owner(&row.id).is_none());
        code(owner.claim_next(), "state_operation_owner_unhealthy");
        audit_valid(&f);
    }
}

#[test]
fn cancellation_audit_or_event_failure_keeps_exact_state_flag_owner_and_locks() {
    for claimed in [false, true] {
        for audit in [true, false] {
            let f = Fixture::open();
            let row = f.admit(&["a"], "cancel-rollback");
            let mut owner = f.ops.open_owner().expect("owner");
            let claim = if claimed {
                owner.claim_next().expect("claim")
            } else {
                None
            };
            let owner_before = f.owner(&row.id);
            let locks_before = f.held(&row.id);
            let summary_before = f.read(&row.id);
            if audit {
                install_audit_fault(&f);
            } else {
                install_event_fault(&f);
            }
            let before = f.counts();
            code(f.ops.request_cancel(&f.subject, &row.id), "state_io");
            assert_eq!(before, f.counts());
            assert_eq!(summary_before, f.read(&row.id));
            assert!(!f.read(&row.id).cancel_requested);
            assert!(owner_before == f.owner(&row.id));
            assert_eq!(locks_before, f.held(&row.id));
            audit_valid(&f);
            drop(claim);
        }
    }
}

#[test]
fn event_fault_on_release_does_not_acknowledge_cancellation_or_release_any_locks() {
    let f = Fixture::open();
    let row = f.admit(&["a", "b"], "release-fault");
    let mut owner = f.ops.open_owner().expect("owner");
    let claim = owner.claim_next().expect("claim").expect("claim exists");
    f.ops
        .request_cancel(&f.subject, &row.id)
        .expect("request retained cancellation");
    let token = f.owner(&row.id);
    install_event_fault(&f);
    let before = f.counts();
    code(owner.release_unstarted(claim), "state_io");
    assert_eq!(before, f.counts());
    assert_eq!(f.read(&row.id).state, OperationState::Cancelling);
    assert!(f.read(&row.id).finished_at_ms.is_none());
    assert!(token == f.owner(&row.id));
    assert_eq!(
        f.held(&row.id),
        vec!["fixture-files:a".to_owned(), "fixture-files:b".to_owned()]
    );
    code(owner.claim_next(), "state_operation_owner_unhealthy");
    drop(owner);
    f.sql(|c| {
        c.execute_batch("DROP TRIGGER fixture_event_fault")?;
        Ok(())
    });
    let owner = f
        .ops
        .open_owner()
        .expect("separate owned unstarted reconciliation");
    assert_eq!(f.read(&row.id).state, OperationState::Cancelled);
    assert!(f.held(&row.id).is_empty() && f.owner(&row.id).is_none());
    drop(owner);
    audit_valid(&f);
}

#[test]
fn refusal_audit_failure_preserves_primary_action_denial_without_mutating_task() {
    let f = Fixture::open();
    let row = f.admit(&["a"], "deny-audit-fault");
    f.deny(&f.subject, Capability::ConfigEdit, "*");
    install_audit_fault(&f);
    let before = f.counts();
    let error = code(
        f.ops.request_cancel(&f.subject, &row.id),
        "audit_unavailable",
    );
    assert!(matches!(&error, OperationError::AuditUnavailable { .. }));
    assert_eq!(error.primary().code(), "forbidden");
    assert_eq!(
        error.primary().reason(),
        Some(identity::DenyReason::ExplicitDeny)
    );
    assert_eq!(
        error
            .secondary()
            .expect("audit storage failure is retained")
            .code(),
        "state_io"
    );
    assert_eq!(before, f.counts());
    assert_eq!(f.read(&row.id).state, OperationState::Queued);
    assert!(!f.read(&row.id).cancel_requested);
    audit_valid(&f);
}

fn deferred_fixture(f: &Fixture, trigger: &str) {
    f.sql(|c| {
        c.execute_batch("CREATE TABLE fixture_commit_parent (id TEXT PRIMARY KEY) STRICT;
            CREATE TABLE fixture_commit_child (id TEXT NOT NULL REFERENCES fixture_commit_parent(id) DEFERRABLE INITIALLY DEFERRED) STRICT;")?;
        c.execute_batch(trigger)?;
        Ok(())
    });
}
fn deferred_empty(f: &Fixture) {
    assert_eq!(
        f.db.read(|c| count(c, "fixture_commit_child"))
            .expect("deferred-FK write rolled back"),
        0
    );
    audit_valid(f);
}

#[test]
fn deferred_foreign_key_commit_failure_rolls_back_admission_and_both_journals() {
    let f = Fixture::open();
    deferred_fixture(
        &f,
        "CREATE TRIGGER fixture_deferred_operation AFTER INSERT ON operations BEGIN INSERT INTO fixture_commit_child VALUES ('missing-parent'); END;",
    );
    let before = f.counts();
    code(
        f.ops.admit(
            &f.subject,
            "fixture.write",
            &raw(&["a", "b"], "requested"),
            None,
        ),
        "state_commit_failed",
    );
    assert_eq!(before, f.counts());
    assert_eq!(
        f.db.read(|c| count(c, "operation_resources"))
            .expect("resources rolled back"),
        0
    );
    deferred_empty(&f);
}

#[test]
fn deferred_foreign_key_commit_failure_rolls_back_claim_and_marks_owner_unhealthy() {
    let f = Fixture::open();
    let row = f.admit(&["a", "b"], "commit-claim");
    let mut owner = f.ops.open_owner().expect("owner");
    deferred_fixture(
        &f,
        "CREATE TRIGGER fixture_deferred_claim AFTER UPDATE OF state ON operations WHEN NEW.state='validating' BEGIN INSERT INTO fixture_commit_child VALUES ('missing-parent'); END;",
    );
    let before = f.counts();
    code(owner.claim_next(), "state_commit_failed");
    assert_eq!(before, f.counts());
    assert_eq!(f.read(&row.id).state, OperationState::Queued);
    assert!(f.owner(&row.id).is_none() && f.held(&row.id).is_empty());
    code(owner.claim_next(), "state_operation_owner_unhealthy");
    deferred_empty(&f);
}

#[test]
fn deferred_foreign_key_commit_failure_rolls_back_cancel_and_terminal_input_erasure() {
    for purge in [false, true] {
        let f = Fixture::open();
        let row = f.admit(&["a"], "commit-cancel-or-purge");
        if purge {
            f.terminal(&row.id, true);
        }
        let summary = f.read(&row.id);
        let input = f.input_status(&row.id);
        let trigger = if purge {
            "CREATE TRIGGER fixture_deferred_purge AFTER UPDATE OF input_status ON operations WHEN NEW.input_status='purged' BEGIN INSERT INTO fixture_commit_child VALUES ('missing-parent'); END;"
        } else {
            "CREATE TRIGGER fixture_deferred_cancel AFTER UPDATE OF state ON operations WHEN NEW.state='cancelled' BEGIN INSERT INTO fixture_commit_child VALUES ('missing-parent'); END;"
        };
        deferred_fixture(&f, trigger);
        let before = f.counts();
        if purge {
            code(
                f.ops.purge_terminal_inputs(INITIAL_MS + 1, 100),
                "state_commit_failed",
            );
        } else {
            code(
                f.ops.request_cancel(&f.subject, &row.id),
                "state_commit_failed",
            );
        }
        assert_eq!(before, f.counts());
        assert_eq!(summary, f.read(&row.id));
        assert_eq!(input, f.input_status(&row.id));
        deferred_empty(&f);
    }
}

#[test]
fn orphan_and_foreign_server_locks_are_detected_inside_a_real_deferred_transaction_and_rolled_back()
{
    for orphan in [false, true] {
        let f = Fixture::open();
        let row = f.admit(&["a"], "lock-integrity");
        let before = f.counts();
        let result = f.db.write_with(|transaction| {
            let c = transaction.connection();
            // Defer this test transaction's immediate FK only; production DB
            // configuration is not weakened and no corrupt orphan is committed.
            c.execute_batch("PRAGMA defer_foreign_keys=ON")?;
            if orphan {
                c.execute("INSERT INTO resource_locks (resource,operation_id,acquired_at) VALUES ('orphan-lock','missing-operation',?)", [INITIAL_MS])?;
            } else {
                c.execute("UPDATE operations SET server_id='different-server' WHERE id=?", [row.id.as_str()])?;
                c.execute("INSERT INTO resource_locks (resource,operation_id,acquired_at) VALUES ('foreign-lock',?,?)", params![row.id.as_str(),INITIAL_MS])?;
            }
            let repo = Repository::new(c,f.db.server_id(),f.clock.millis());
            repo.check_foreign_or_terminal_locks()
        }).map_err(OperationError::from);
        code(result, "state_operation_corrupt");
        assert_eq!(before, f.counts());
        assert_eq!(f.read(&row.id).state, OperationState::Queued);
        assert!(f.held(&row.id).is_empty());
        let owner = f
            .ops
            .open_owner()
            .expect("rolled back orphan/foreign lock does not contaminate startup");
        drop(owner);
    }
}

#[test]
fn terminal_started_marker_with_no_locks_is_valid_for_noop_cancel_exact_retry_and_retention() {
    for state in ["succeeded", "failed", "cancelled"] {
        let f = Fixture::open();
        let row = f.admit(&["a"], "terminal-marker");
        f.terminal(&row.id, true);
        f.sql(|c| {
            c.execute(
                "UPDATE operations SET state=? WHERE id=?",
                params![state, row.id.as_str()],
            )?;
            Ok(())
        });
        let before = f.counts();
        assert!(matches!(
            f.ops
                .request_cancel(&f.subject, &row.id)
                .expect("known terminal effect is not still owned"),
            CancelOutcome::AlreadyTerminal(_)
        ));
        assert_eq!(before, f.counts());
        assert_eq!(
            f.ops
                .purge_terminal_inputs(INITIAL_MS, 100)
                .expect("strict cutoff boundary")
                .purged,
            0
        );
        assert_eq!(
            f.ops
                .purge_terminal_inputs(INITIAL_MS + 1, 1)
                .expect("purge known terminal input")
                .purged,
            1
        );
        assert_eq!(f.input_status(&row.id), (None, "purged".to_owned()));
        assert_eq!(f.counts(), (before.0, before.1, before.2 + 1, before.3));
        assert!(matches!(
            f.ops
                .request_cancel(&f.subject, &row.id)
                .expect("terminal no-op after retention"),
            CancelOutcome::AlreadyTerminal(_)
        ));
        assert!(
            matches!(f.ops.admit(&f.subject,"fixture.write",&raw(&["a"],"requested"),Some("terminal-marker")).expect("exact terminal retry after input retention"),Admission::Existing(ref existing) if existing.id==row.id)
        );
        assert_eq!(
            f.ops
                .purge_terminal_inputs(INITIAL_MS + 1, 100)
                .expect("idempotent retention")
                .purged,
            0
        );
        let audit = AuditLog::new(f.db.clone())
            .expect("verified audit")
            .page(0, 100)
            .expect("audit rows");
        let purge = audit
            .iter()
            .find(|entry| entry.action == "operation.inputs_purged")
            .expect("required retention audit");
        let metadata: serde_json::Value =
            serde_json::from_str(&purge.metadata_json).expect("fixed purge metadata");
        assert_eq!(metadata["count"].as_str(), Some("1"));
        assert_eq!(
            metadata["finished_before_ms"].as_str(),
            Some((INITIAL_MS + 1).to_string().as_str())
        );
        audit_valid(&f);
    }
}

#[test]
fn terminal_retention_is_bounded_and_never_removes_nonterminal_or_recovery_input() {
    let f = Fixture::open();
    let one = f.admit(&["a"], "purge-one");
    let two = f.admit(&["b"], "purge-two");
    let queued = f.admit(&["c"], "keep-queued");
    f.terminal(&one.id, true);
    f.terminal(&two.id, false);
    let started = f.admit(&["d"], "keep-recovery");
    let mut owner = f.ops.open_owner().expect("owner");
    let queued_claim = owner
        .claim_next()
        .expect("claim oldest queued")
        .expect("queued claim");
    assert_eq!(queue::claim_id(&queued_claim), &queued.id);
    let started_claim = owner
        .claim_next()
        .expect("independent claim")
        .expect("started claim");
    assert_eq!(queue::claim_id(&started_claim), &started.id);
    f.sql(|c| {
        c.execute(
            "UPDATE operations SET external_effect_started=1,state='running' WHERE id=?",
            [started.id.as_str()],
        )?;
        Ok(())
    });
    drop(started_claim);
    drop(queued_claim);
    drop(owner);
    let owner = f.ops.open_owner().expect("conservative reconciliation");
    let recovery_input = f.input_status(&started.id);
    assert_eq!(
        f.ops
            .purge_terminal_inputs(INITIAL_MS + 1, 1)
            .expect("bounded first page")
            .purged,
        1
    );
    assert_eq!(f.input_status(&one.id), (None, "purged".to_owned()));
    assert!(f.input_status(&two.id).0.is_some());
    assert_eq!(
        f.ops
            .purge_terminal_inputs(INITIAL_MS + 1, 1)
            .expect("bounded next page")
            .purged,
        1
    );
    assert!(f.input_status(&queued.id).0.is_some());
    assert_eq!(f.input_status(&started.id), recovery_input);
    assert_eq!(f.read(&started.id).state, OperationState::RecoveryRequired);
    assert_eq!(f.held(&started.id), vec!["fixture-files:d".to_owned()]);
    assert_eq!(
        f.ops
            .purge_terminal_inputs(INITIAL_MS + 1, 100)
            .expect("no nonterminal erasure")
            .purged,
        0
    );
    drop(owner);
}

#[test]
fn terminal_held_lock_fails_cancel_startup_and_rolls_back_the_entire_retention_batch() {
    let f = Fixture::open();
    let first = f.admit(&["a"], "purge-before-corruption");
    let corrupt = f.admit(&["b"], "terminal-held");
    f.terminal(&first.id, true);
    f.terminal(&corrupt.id, true);
    f.sql(|c| { c.execute("INSERT INTO resource_locks (resource,operation_id,acquired_at) VALUES ('fixture-files:b',?,?)",params![corrupt.id.as_str(),INITIAL_MS])?; Ok(()) });
    let before = f.counts();
    code(
        f.ops.request_cancel(&f.subject, &corrupt.id),
        "state_operation_corrupt",
    );
    assert_eq!(before, f.counts());
    code(
        f.ops.purge_terminal_inputs(INITIAL_MS + 1, 100),
        "state_operation_corrupt",
    );
    assert_eq!(before, f.counts());
    assert!(f.input_status(&first.id).0.is_some() && f.input_status(&corrupt.id).0.is_some());
    assert_eq!(f.held(&corrupt.id), vec!["fixture-files:b".to_owned()]);
    code(f.ops.open_owner(), "state_operation_corrupt");
    let lock =
        f.db.directory()
            .acquire_runner_lock()
            .expect("failed startup releases only its OS lock");
    drop(lock);
}

#[test]
fn purge_audit_fault_rolls_back_all_input_erasure_and_emits_no_event() {
    let f = Fixture::open();
    let one = f.admit(&["a"], "audit-purge-one");
    let two = f.admit(&["b"], "audit-purge-two");
    f.terminal(&one.id, true);
    f.terminal(&two.id, false);
    let inputs = (f.input_status(&one.id), f.input_status(&two.id));
    install_audit_fault(&f);
    let before = f.counts();
    code(f.ops.purge_terminal_inputs(INITIAL_MS + 1, 100), "state_io");
    assert_eq!(before, f.counts());
    assert_eq!(inputs, (f.input_status(&one.id), f.input_status(&two.id)));
    audit_valid(&f);
}
