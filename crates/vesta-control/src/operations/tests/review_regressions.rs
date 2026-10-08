//! Review regressions over the real policy resolver and SQLite transactions.
//! The parent owns module wiring and execution; this file adds no production seam.
use super::*;

fn install_uninterpretable_deny(f: &Fixture) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    f.sql(|c| {
        c.execute(
            "INSERT INTO principal_overrides \
             (id,principal_id,capability,scope,effect,granted_by,created_at) \
             VALUES (?,?,'review.uninterpretable.capability','*','deny','fixture',?)",
            params![id, f.subject.principal.as_str(), f.clock.millis()],
        )?;
        Ok(())
    });
    id
}

fn remove_uninterpretable_deny(f: &Fixture, id: &str) {
    f.sql(|c| {
        assert_eq!(
            c.execute("DELETE FROM principal_overrides WHERE id=?", [id])?,
            1
        );
        Ok(())
    });
}

/// Observe fixed task facts without asking the deliberately broken policy resolver.
/// This is trusted fixture inspection, not an alternative public access path.
fn stored_summary(f: &Fixture, id: &OperationId) -> OperationSummary {
    f.db.read_with(|snapshot| -> OperationsResult<OperationSummary> {
        Repository::new(snapshot.connection(), f.db.server_id(), f.clock.millis())
            .summary(id)?
            .ok_or_else(domain::corrupt)
    })
    .expect("inspect retained task in its own snapshot")
}

fn assert_policy_failure(error: &OperationError) {
    assert!(matches!(
        error,
        OperationError::Policy(crate::ControlError::State(state))
            if state.code() == "state_identity_invalid"
    ));
    assert_eq!(error.code(), "state_identity_invalid");
    assert!(matches!(error.primary(), OperationError::Policy(_)));
    assert_eq!(error.primary().code(), "state_identity_invalid");
    assert_eq!(error.reason(), None);
    assert_eq!(error.secondary(), None);
}

fn audit_valid(f: &Fixture) {
    assert!(
        AuditLog::new(f.db.clone())
            .expect("open actual retained audit")
            .verify_chain()
            .expect("verify actual retained audit")
            .valid
    );
}

#[test]
fn policy_resolution_failure_is_not_an_audited_action_refusal() {
    let f = Fixture::open();
    let row = f.admit(&["a"], "review-policy-request");
    let original = f.read(&row.id);
    let original_input = f.input_status(&row.id);

    // A real, interpretable deny remains an ordinary rejection with refusal audit.
    f.deny(&f.subject, Capability::ConfigEdit, "*");
    let before_denial = f.counts();
    let denied = f
        .ops
        .request_cancel(&f.subject, &row.id)
        .expect_err("ordinary action deny must refuse cancellation");
    assert!(matches!(
        denied,
        OperationError::Rejected {
            code: OperationErrorCode::Forbidden,
            reason: Some(identity::DenyReason::ExplicitDeny),
        }
    ));
    assert_eq!(
        f.counts(),
        (
            before_denial.0,
            before_denial.1,
            before_denial.2 + 1,
            before_denial.3,
        )
    );
    let before_admission_denial = f.counts();
    let denied = f
        .ops
        .admit(
            &f.subject,
            "fixture.write",
            &raw(&["b"], "requested"),
            Some("review-denied-admit"),
        )
        .expect_err("ordinary action deny must refuse admission");
    assert!(matches!(
        denied,
        OperationError::Rejected {
            code: OperationErrorCode::Forbidden,
            reason: Some(identity::DenyReason::ExplicitDeny),
        }
    ));
    assert_eq!(
        f.counts(),
        (
            before_admission_denial.0,
            before_admission_denial.1,
            before_admission_denial.2 + 1,
            before_admission_denial.3,
        )
    );
    assert_eq!(original, f.read(&row.id));

    // An unknown ACTIVE deny fails resolution itself, rather than returning Deny.
    // It is intentionally present alongside the ordinary ConfigEdit denial above.
    install_uninterpretable_deny(&f);
    let before_failure = f.counts();
    let error = f
        .ops
        .request_cancel(&f.subject, &row.id)
        .expect_err("policy infrastructure failure cannot become a cancellation refusal");
    assert_policy_failure(&error);
    assert_eq!(before_failure, f.counts());
    let error = f
        .ops
        .admit(
            &f.subject,
            "fixture.write",
            &raw(&["b"], "requested"),
            Some("review-failed-admit"),
        )
        .expect_err("policy infrastructure failure cannot become an admission refusal");
    assert_policy_failure(&error);
    assert_eq!(before_failure, f.counts());
    assert_eq!(original, stored_summary(&f, &row.id));
    assert_eq!(original_input, f.input_status(&row.id));
    assert!(f.owner(&row.id).is_none());
    assert!(f.held(&row.id).is_empty());
    audit_valid(&f);
}

#[test]
fn claim_policy_failure_rolls_back_prior_safe_refusal_and_stops_the_owner() {
    let f = Fixture::open();
    let first = f.admit(&["a"], "review-claim-first");
    let second = f.admit(&["b"], "review-claim-second");
    let mut owner = f
        .ops
        .open_owner()
        .expect("open healthy owner before injecting faults");
    // Corrupt only after startup: claim_next will first safely fail this row,
    // append its audit/event, and then reach the next row's broken policy.
    f.sql(|c| {
        assert_eq!(
            c.execute(
                "UPDATE operations SET payload_hash=? WHERE id=?",
                params!["0".repeat(64), first.id.as_str()],
            )?,
            1
        );
        Ok(())
    });
    install_uninterpretable_deny(&f);
    let summaries = (
        stored_summary(&f, &first.id),
        stored_summary(&f, &second.id),
    );
    let inputs = (f.input_status(&first.id), f.input_status(&second.id));
    let before = f.counts();
    f.clock.set(INITIAL_MS + 10);

    let error = owner
        .claim_next()
        .expect_err("second row's policy resolution fails the claim transaction");
    assert_policy_failure(&error);
    assert_eq!(before, f.counts());
    assert_eq!(summaries.0, stored_summary(&f, &first.id));
    assert_eq!(summaries.1, stored_summary(&f, &second.id));
    assert_eq!(
        inputs,
        (f.input_status(&first.id), f.input_status(&second.id))
    );
    assert!(f.error_code(&first.id).is_none() && f.error_code(&second.id).is_none());
    assert!(f.owner(&first.id).is_none() && f.owner(&second.id).is_none());
    assert!(f.held(&first.id).is_empty() && f.held(&second.id).is_empty());
    code(owner.claim_next(), "state_operation_owner_unhealthy");
    assert_eq!(before, f.counts());
    drop(owner);
    audit_valid(&f);
}

#[test]
fn restart_policy_failure_rolls_back_prior_cancellation_and_releases_the_os_lock() {
    let f = Fixture::open();
    let first = f.admit(&["a"], "review-restart-cancelling");
    let second = f.admit(&["b"], "review-restart-policy");
    let mut owner = f.ops.open_owner().expect("initial owned reconciliation");
    let first_claim = owner
        .claim_next()
        .expect("claim first")
        .expect("first claim exists");
    let second_claim = owner
        .claim_next()
        .expect("claim second")
        .expect("second claim exists");
    assert_eq!(queue::claim_id(&first_claim), &first.id);
    assert_eq!(queue::claim_id(&second_claim), &second.id);
    assert!(matches!(
        f.ops
            .request_cancel(&f.subject, &first.id)
            .expect("request effect-free cancellation"),
        CancelOutcome::Requested(_)
    ));
    drop(first_claim);
    drop(second_claim);
    drop(owner);

    let deny = install_uninterpretable_deny(&f);
    let summaries = (
        stored_summary(&f, &first.id),
        stored_summary(&f, &second.id),
    );
    let tokens = (f.owner(&first.id), f.owner(&second.id));
    assert!(tokens.0.is_some() && tokens.1.is_some());
    let locks = (f.held(&first.id), f.held(&second.id));
    let inputs = (f.input_status(&first.id), f.input_status(&second.id));
    let before = f.counts();
    f.clock.set(INITIAL_MS + 10);

    // The first row needs no policy lookup to finish its known-unstarted
    // cancellation. The second does: the entire startup transaction must undo
    // the first row's terminalization, released locks, and both journal appends.
    let error = f
        .ops
        .open_owner()
        .expect_err("second recovery's policy infrastructure fails startup");
    assert_policy_failure(&error);
    assert_eq!(before, f.counts());
    assert_eq!(summaries.0, stored_summary(&f, &first.id));
    assert_eq!(summaries.1, stored_summary(&f, &second.id));
    assert!(tokens.0 == f.owner(&first.id) && tokens.1 == f.owner(&second.id));
    assert_eq!(locks, (f.held(&first.id), f.held(&second.id)));
    assert_eq!(
        inputs,
        (f.input_status(&first.id), f.input_status(&second.id))
    );
    assert!(f.error_code(&first.id).is_none() && f.error_code(&second.id).is_none());
    let lock =
        f.db.directory()
            .acquire_runner_lock()
            .expect("failed policy reconciliation releases its acquired OS lock");
    drop(lock);
    audit_valid(&f);

    remove_uninterpretable_deny(&f, &deny);
    let owner = f
        .ops
        .open_owner()
        .expect("separate restored startup may actually recover both rows");
    assert_eq!(f.read(&first.id).state, OperationState::Cancelled);
    assert_eq!(f.read(&second.id).state, OperationState::Queued);
    assert!(f.owner(&first.id).is_none() && f.owner(&second.id).is_none());
    assert!(f.held(&first.id).is_empty() && f.held(&second.id).is_empty());
    assert_eq!(
        inputs,
        (f.input_status(&first.id), f.input_status(&second.id))
    );
    assert_eq!(f.counts(), (before.0, 0, before.2 + 2, before.3 + 2));
    drop(owner);
    audit_valid(&f);
}

#[test]
fn second_reconciliation_journal_fault_rolls_back_both_rows_and_releases_the_os_lock() {
    for audit_fault in [true, false] {
        let f = Fixture::open();
        let first = f.admit(&["a"], "review-late-first");
        let second = f.admit(&["b"], "review-late-second");
        let mut owner = f.ops.open_owner().expect("initial owned reconciliation");
        let first_claim = owner
            .claim_next()
            .expect("claim first")
            .expect("first claim exists");
        let second_claim = owner
            .claim_next()
            .expect("claim second")
            .expect("second claim exists");
        assert_eq!(queue::claim_id(&first_claim), &first.id);
        assert_eq!(queue::claim_id(&second_claim), &second.id);
        drop(first_claim);
        drop(second_claim);
        drop(owner);

        let summaries = (f.read(&first.id), f.read(&second.id));
        let tokens = (f.owner(&first.id), f.owner(&second.id));
        assert!(tokens.0.is_some() && tokens.1.is_some());
        let locks = (f.held(&first.id), f.held(&second.id));
        assert_eq!(locks.0, vec!["fixture-files:a".to_owned()]);
        assert_eq!(locks.1, vec!["fixture-files:b".to_owned()]);
        let inputs = (f.input_status(&first.id), f.input_status(&second.id));
        let before = f.counts();
        let (audit_seq, event_cursor) =
            f.db.read(|c| {
                Ok((
                    c.query_row("SELECT coalesce(max(seq),0) FROM audit_events", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                    c.query_row("SELECT coalesce(max(cursor),0) FROM events", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                ))
            })
            .expect("capture committed journal boundaries before restart");
        let table = if audit_fault {
            "audit_events"
        } else {
            "events"
        };
        let action = if audit_fault {
            "NEW.action='operation.recover'"
        } else {
            "NEW.type='operation.state'"
        };
        // IDs here are internally generated validated UUIDs, not arbitrary SQL
        // input. The trigger fires ONLY on row two and ONLY after row one's state,
        // token, locks, audit AND event changed in this same transaction. Merely
        // failing the first recovery cannot satisfy this witness.
        let trigger = format!(
            "CREATE TRIGGER review_late_reconciliation_fault BEFORE INSERT ON {table} \
             WHEN NEW.operation_id='{second_id}' AND {action} \
             AND EXISTS(SELECT 1 FROM operations o WHERE o.id='{first_id}' \
                 AND o.state='queued' AND o.owner_token IS NULL) \
             AND NOT EXISTS(SELECT 1 FROM resource_locks l WHERE l.operation_id='{first_id}') \
             AND EXISTS(SELECT 1 FROM audit_events a WHERE a.operation_id='{first_id}' \
                 AND a.action='operation.recover' AND a.seq>{audit_seq}) \
             AND EXISTS(SELECT 1 FROM events e WHERE e.operation_id='{first_id}' \
                 AND e.type='operation.state' AND e.cursor>{event_cursor}) \
             BEGIN SELECT RAISE(ABORT,'review-second-reconciliation-fault'); END;",
            first_id = first.id.as_str(),
            second_id = second.id.as_str(),
        );
        f.sql(|c| {
            c.execute_batch(&trigger)?;
            Ok(())
        });
        f.clock.set(INITIAL_MS + 10);

        let error = code(f.ops.open_owner(), "state_io");
        assert!(!format!("{error:?} {error}").contains("review-second-reconciliation-fault"));
        assert_eq!(before, f.counts());
        assert_eq!(summaries.0, f.read(&first.id));
        assert_eq!(summaries.1, f.read(&second.id));
        assert!(tokens.0 == f.owner(&first.id) && tokens.1 == f.owner(&second.id));
        assert_eq!(locks, (f.held(&first.id), f.held(&second.id)));
        assert_eq!(
            inputs,
            (f.input_status(&first.id), f.input_status(&second.id))
        );
        assert!(f.error_code(&first.id).is_none() && f.error_code(&second.id).is_none());
        let lock =
            f.db.directory()
                .acquire_runner_lock()
                .expect("late journal fault releases only startup's OS ownership");
        drop(lock);
        audit_valid(&f);

        f.sql(|c| {
            c.execute_batch("DROP TRIGGER review_late_reconciliation_fault")?;
            Ok(())
        });
        let owner = f
            .ops
            .open_owner()
            .expect("separate fault-free startup recovers both retained claims");
        assert_eq!(f.read(&first.id).state, OperationState::Queued);
        assert_eq!(f.read(&second.id).state, OperationState::Queued);
        assert!(f.owner(&first.id).is_none() && f.owner(&second.id).is_none());
        assert!(f.held(&first.id).is_empty() && f.held(&second.id).is_empty());
        assert_eq!(
            inputs,
            (f.input_status(&first.id), f.input_status(&second.id))
        );
        assert_eq!(f.counts(), (before.0, 0, before.2 + 2, before.3 + 2));
        drop(owner);
        audit_valid(&f);
    }
}
