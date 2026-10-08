use super::*;

#[test]
fn transitive_conflicting_fifo_reserves_full_locksets_but_independent_work_progresses() {
    let f = Fixture::open();
    let held = f.admit(&["a"], "held");
    let mut owner = f.ops.open_owner().expect("OS owned session");
    let held_claim = owner
        .claim_next()
        .expect("claim held")
        .expect("held exists");
    assert_eq!(queue::claim_id(&held_claim), &held.id);
    let first = f.admit(&["b", "a"], "first");
    let second = f.admit(&["c", "b"], "second");
    let third = f.admit(&["c"], "third");
    let independent = f.admit(&["d"], "independent");
    let independent_claim = owner
        .claim_next()
        .expect("walk blocked FIFO")
        .expect("independent progresses");
    assert_eq!(queue::claim_id(&independent_claim), &independent.id);
    assert!(
        f.held(&first.id).is_empty()
            && f.held(&second.id).is_empty()
            && f.held(&third.id).is_empty()
    );
    assert!(
        owner
            .claim_next()
            .expect("all remaining work is reserved")
            .is_none()
    );
    assert!(matches!(
        f.ops
            .request_cancel(&f.subject, &held.id)
            .expect("request held cancellation"),
        CancelOutcome::Requested(_)
    ));
    assert_eq!(
        owner
            .release_unstarted(held_claim)
            .expect("release truly unstarted held claim")
            .state,
        OperationState::Cancelled
    );
    let first_claim = owner
        .claim_next()
        .expect("first after actual release")
        .expect("first ready");
    assert_eq!(queue::claim_id(&first_claim), &first.id);
    assert_eq!(
        f.held(&first.id),
        vec!["fixture-files:a".to_owned(), "fixture-files:b".to_owned()]
    );
    assert!(
        owner
            .claim_next()
            .expect("transitive reservation remains")
            .is_none()
    );
    f.ops
        .request_cancel(&f.subject, &first.id)
        .expect("cancel first");
    owner.release_unstarted(first_claim).expect("release first");
    let second_claim = owner
        .claim_next()
        .expect("second ready")
        .expect("second claim");
    assert_eq!(queue::claim_id(&second_claim), &second.id);
    assert!(
        owner
            .claim_next()
            .expect("third remains blocked by second")
            .is_none()
    );
    f.ops
        .request_cancel(&f.subject, &second.id)
        .expect("cancel second");
    owner
        .release_unstarted(second_claim)
        .expect("release second");
    let third_claim = owner
        .claim_next()
        .expect("third ready")
        .expect("third claim");
    assert_eq!(queue::claim_id(&third_claim), &third.id);
    drop(third_claim);
    drop(independent_claim);
}

#[test]
fn keyset_walk_crosses_bounded_pages_without_starving_independent_work() {
    let f = Fixture::open();
    f.admit(&["a"], "holder");
    let mut owner = f.ops.open_owner().expect("owner");
    let holder = owner.claim_next().expect("holder").expect("holder claim");
    let mut blocked = Vec::new();
    for index in 0..(MAX_PAGE + 5) {
        blocked.push(f.admit(&["a", "b"], &format!("blocked-{index}")));
    }
    let transitive = f.admit(&["b", "c"], "cross-page-reservation");
    let chained = f.admit(&["c"], "cross-page-chained");
    let ready = f.admit(&["d"], "beyond-page-ready");
    let claim = owner
        .claim_next()
        .expect("walk full necessary keyset")
        .expect("independent work beyond first page");
    assert_eq!(queue::claim_id(&claim), &ready.id);
    assert!(
        owner
            .claim_next()
            .expect("all earlier conflicts remain queued")
            .is_none()
    );
    assert_eq!(f.read(&transitive.id).state, OperationState::Queued);
    assert_eq!(f.read(&chained.id).state, OperationState::Queued);
    assert!(
        blocked.iter().all(
            |row| f.read(&row.id).state == OperationState::Queued && f.held(&row.id).is_empty()
        )
    );
    drop(claim);
    drop(holder);
}

#[test]
fn cancellation_retains_exact_claim_locks_until_unstarted_release_and_redacts_tokens() {
    let f = Fixture::open();
    let row = f.admit(&["b", "a"], "cancel-active");
    let mut owner = f.ops.open_owner().expect("owner");
    let claim = owner.claim_next().expect("claim").expect("claim exists");
    let token = queue::claim_token(&claim).to_owned();
    assert!(f.owner(&row.id).as_deref() == Some(token.as_str()));
    assert!(!format!("{claim:?} {owner:?}").contains(&token));
    let before = f.counts();
    let cancel = f
        .ops
        .request_cancel(&f.subject, &row.id)
        .expect("exact actor request");
    assert!(
        matches!(cancel, CancelOutcome::Requested(ref summary) if summary.state == OperationState::Cancelling && summary.cancel_requested && summary.finished_at_ms.is_none())
    );
    assert_eq!(
        f.held(&row.id),
        vec!["fixture-files:a".to_owned(), "fixture-files:b".to_owned()]
    );
    assert!(f.owner(&row.id).as_deref() == Some(token.as_str()));
    let after = f.counts();
    assert_eq!(after, (before.0, before.1, before.2 + 1, before.3 + 1));
    assert!(matches!(
        f.ops
            .request_cancel(&f.subject, &row.id)
            .expect("idempotent request"),
        CancelOutcome::AlreadyRequested(_)
    ));
    assert_eq!(after, f.counts());
    let terminal = owner
        .release_unstarted(claim)
        .expect("no effect has started");
    assert_eq!(terminal.state, OperationState::Cancelled);
    assert!(terminal.finished_at_ms.is_some());
    assert!(f.held(&row.id).is_empty() && f.owner(&row.id).is_none());
    let events = f.events_for_test();
    assert!(
        events
            .iter()
            .all(|event| !event.payload_json.contains(&token))
    );
    let audit = AuditLog::new(f.db.clone())
        .expect("verified journal")
        .page(0, 100)
        .expect("audit");
    assert!(
        audit
            .iter()
            .all(|entry| !entry.metadata_json.contains(&token))
    );
}

#[test]
fn wrong_token_and_prior_session_claims_cannot_release_current_ownership() {
    let f = Fixture::open();
    let row = f.admit(&["a"], "claim-owner");
    let mut first = f.ops.open_owner().expect("first owner");
    let old = first.claim_next().expect("claim").expect("old claim");
    let old_token = queue::claim_token(&old).to_owned();
    code(
        first.release_unstarted(queue::forge_token(&old)),
        "not_owner",
    );
    assert_eq!(f.read(&row.id).state, OperationState::Validating);
    assert!(f.owner(&row.id).as_deref() == Some(old_token.as_str()));
    let before = f.counts();
    drop(first);
    assert_eq!(before, f.counts());
    assert_eq!(f.read(&row.id).state, OperationState::Validating);
    let mut second = f.ops.open_owner().expect("actual next OS owner reconciles");
    assert_eq!(f.read(&row.id).state, OperationState::Queued);
    assert!(f.owner(&row.id).is_none() && f.held(&row.id).is_empty());
    let current = second
        .claim_next()
        .expect("reclaim known effect-free binding")
        .expect("current claim");
    assert!(queue::claim_token(&current) != old_token);
    code(second.release_unstarted(old), "not_owner");
    assert!(f.owner(&row.id).as_deref() == Some(queue::claim_token(&current)));
    assert_eq!(
        second
            .release_unstarted(current)
            .expect("only current exact owner releases")
            .state,
        OperationState::Queued
    );
}

#[test]
fn null_owner_is_not_a_tokenless_public_or_claim_mutation_overload() {
    let f = Fixture::open();
    let row = f.admit(&["a"], "null-owner");
    let mut owner = f.ops.open_owner().expect("owner");
    let claim = owner.claim_next().expect("claim").expect("claim exists");
    f.sql(|c| {
        c.execute(
            "UPDATE operations SET owner_token=NULL WHERE id=?",
            [row.id.as_str()],
        )?;
        Ok(())
    });
    code(
        f.ops.request_cancel(&f.subject, &row.id),
        "state_operation_corrupt",
    );
    code(owner.release_unstarted(claim), "not_owner");
    assert_eq!(f.read(&row.id).state, OperationState::Validating);
    assert_eq!(f.held(&row.id), vec!["fixture-files:a".to_owned()]);
    assert!(f.owner(&row.id).is_none());
}

#[test]
fn os_lock_is_exclusive_and_drop_never_claims_worker_termination() {
    let f = Fixture::open();
    let row = f.admit(&["a"], "os-owner");
    let mut owner = f.ops.open_owner().expect("first OS owner");
    let error = f
        .ops
        .open_owner()
        .expect_err("another owner cannot share lock");
    assert_eq!(error.code(), "state_runner_lock_held");
    let claim = owner.claim_next().expect("claim").expect("unstarted claim");
    let before = f.counts();
    drop(claim);
    drop(owner);
    assert_eq!(f.counts(), before);
    assert_eq!(f.read(&row.id).state, OperationState::Validating);
    assert!(f.owner(&row.id).is_some());
    assert_eq!(f.held(&row.id), vec!["fixture-files:a".to_owned()]);
    let owner = f
        .ops
        .open_owner()
        .expect("drop releases only OS lock, startup separately reconciles");
    assert_eq!(f.read(&row.id).state, OperationState::Queued);
    assert!(f.owner(&row.id).is_none() && f.held(&row.id).is_empty());
    drop(owner);
}

#[test]
fn reopen_of_actual_database_recovers_unstarted_claim_without_fabricating_effect_exit() {
    let f = Fixture::open();
    let row = f.admit(&["a"], "reopen");
    let mut owner = f.ops.open_owner().expect("initial owner");
    let claim = owner.claim_next().expect("claim").expect("claim exists");
    drop(claim);
    drop(owner);
    let directory = f.db.directory().clone();
    f.db.close().expect("real initial database drains");
    let reopened =
        StateDatabase::open(directory, f.clock.clone()).expect("reopen actual persisted database");
    let ops = Operations::new(reopened.clone()).expect("verify actual retained audit chain");
    let mut owner = ops.open_owner().expect("new OS owned reconciliation");
    assert_eq!(
        ops.get(&f.subject, &row.id)
            .expect("current policy observation")
            .expect("retained row")
            .state,
        OperationState::Queued
    );
    let claim = owner
        .claim_next()
        .expect("known effect-free binding may be privately reclaimed")
        .expect("new claim");
    assert_eq!(queue::claim_id(&claim), &row.id);
    owner
        .release_unstarted(claim)
        .expect("release before closing actual reopened DB");
    drop(owner);
    reopened.close().expect("drain reopened database");
}

#[test]
fn started_work_becomes_nonterminal_recovery_even_with_idempotent_bit_and_keeps_input_locks() {
    for kind in ["fixture.write", "fixture.safe"] {
        let f = Fixture::open();
        let row = match f
            .ops
            .admit(&f.subject, kind, &raw(&["a"], "requested"), None)
            .expect("fixture")
        {
            Admission::Created(row) => row,
            _ => unreachable!(),
        };
        let input = f.input_status(&row.id);
        let mut owner = f.ops.open_owner().expect("owner");
        let claim = owner.claim_next().expect("claim").expect("exists");
        f.sql(|c| {
            c.execute(
                "UPDATE operations SET state='running',external_effect_started=1 WHERE id=?",
                [row.id.as_str()],
            )?;
            Ok(())
        });
        code(owner.release_unstarted(claim), "recovery_required");
        assert_eq!(f.read(&row.id).state, OperationState::Running);
        drop(owner);
        let mut owner = f.ops.open_owner().expect("conservative startup");
        let recovery = f.read(&row.id);
        assert_eq!(recovery.state, OperationState::RecoveryRequired);
        assert!(!recovery.state.is_terminal());
        assert!(recovery.finished_at_ms.is_none() && f.owner(&row.id).is_none());
        assert_eq!(f.held(&row.id), vec!["fixture-files:a".to_owned()]);
        assert_eq!(f.input_status(&row.id), input);
        code(
            f.ops.request_cancel(&f.subject, &row.id),
            "recovery_required",
        );
        assert_eq!(
            f.ops
                .purge_terminal_inputs(INITIAL_MS + 1, 100)
                .expect("retention excludes recovery")
                .purged,
            0
        );
        assert!(
            owner
                .claim_next()
                .expect("recovery is never replayed")
                .is_none()
        );
        let before = f.counts();
        drop(owner);
        let owner = f.ops.open_owner().expect("retained recovery is unchanged");
        assert_eq!(before, f.counts());
        assert_eq!(f.input_status(&row.id), input);
        drop(owner);
    }
}

#[test]
fn missing_or_contradictory_recovery_lock_ownership_stops_startup_and_releases_os_lock() {
    for extra in [false, true] {
        let f = Fixture::open();
        let id = f.seed_legacy(
            "recovery",
            "recovery_required",
            true,
            true,
            Some("legacy-lock"),
        );
        f.sql(|c| {
            if extra {
                c.execute("INSERT INTO resource_locks (resource,operation_id,acquired_at) VALUES ('undeclared-lock',?,?)", params![id.as_str(),INITIAL_MS])?;
            } else { c.execute("DELETE FROM resource_locks WHERE operation_id=?", [id.as_str()])?; }
            Ok(())
        });
        let before = f.counts();
        code(f.ops.open_owner(), "state_operation_corrupt");
        assert_eq!(before, f.counts());
        let lock =
            f.db.directory()
                .acquire_runner_lock()
                .expect("failed startup releases its acquired OS lock");
        drop(lock);
        assert_eq!(f.read(&id).state, OperationState::RecoveryRequired);
    }
}

#[test]
fn legacy_inputs_are_not_fabricated_safe_work_fails_and_uncertain_work_keeps_locks() {
    let f = Fixture::open();
    let queued = f.seed_legacy("legacy-queued", "queued", false, false, Some("queued-lock"));
    let validating = f.seed_legacy(
        "legacy-validating",
        "validating",
        false,
        false,
        Some("validating-lock"),
    );
    let cancelling = f.seed_legacy(
        "legacy-cancelling",
        "cancelling",
        false,
        false,
        Some("cancelling-lock"),
    );
    let started = f.seed_legacy(
        "legacy-started",
        "running",
        true,
        true,
        Some("started-lock"),
    );
    let mut owner = f.ops.open_owner().expect("OS-owned legacy reconciliation");
    for id in [&queued, &validating] {
        assert_eq!(f.read(id).state, OperationState::Failed);
        assert_eq!(f.error_code(id).as_deref(), Some("input_unavailable"));
        assert!(f.held(id).is_empty());
    }
    assert_eq!(f.read(&cancelling).state, OperationState::Cancelled);
    assert!(f.held(&cancelling).is_empty());
    assert_eq!(f.read(&started).state, OperationState::RecoveryRequired);
    assert_eq!(f.held(&started), vec!["started-lock".to_owned()]);
    for id in [&queued, &validating, &cancelling, &started] {
        assert_eq!(f.input_status(id), (None, "unavailable_legacy".to_owned()));
    }
    assert!(
        owner
            .claim_next()
            .expect("legacy rows never execute")
            .is_none()
    );
}

#[test]
fn unstarted_restart_uses_current_policy_not_an_old_admission_decision() {
    let f = Fixture::open();
    let row = f.admit(&["a"], "restart-policy");
    let mut owner = f.ops.open_owner().expect("owner");
    let claim = owner.claim_next().expect("claim").expect("exists");
    f.deny(&f.subject, Capability::FilesOverwrite, "a");
    drop(claim);
    drop(owner);
    let owner = f
        .ops
        .open_owner()
        .expect("current denial safely terminalizes unstarted work");
    assert_eq!(f.read(&row.id).state, OperationState::Failed);
    assert_eq!(f.error_code(&row.id).as_deref(), Some("forbidden"));
    assert!(f.held(&row.id).is_empty() && f.owner(&row.id).is_none());
    drop(owner);
}

#[test]
fn confirmation_flag_does_not_become_a_forged_consumption_proof() {
    let f = Fixture::open();
    let waiting = match f
        .ops
        .admit(
            &f.subject,
            "fixture.confirm",
            &raw(&["a"], "requested"),
            None,
        )
        .expect("waiting fixture")
    {
        Admission::Created(row) => row,
        _ => unreachable!(),
    };
    f.sql(|c| {
        c.execute(
            "UPDATE operations SET state='queued' WHERE id=?",
            [waiting.id.as_str()],
        )?;
        Ok(())
    });
    let independent = f.admit(&["b"], "no-confirmation-needed");
    let mut owner = f.ops.open_owner().expect("owner");
    let claim = owner
        .claim_next()
        .expect("fail forged queue state and continue")
        .expect("independent is ready");
    assert_eq!(queue::claim_id(&claim), &independent.id);
    assert_eq!(f.read(&waiting.id).state, OperationState::Failed);
    assert_eq!(
        f.error_code(&waiting.id).as_deref(),
        Some("invalid_transition")
    );
    assert!(f.held(&waiting.id).is_empty());
    drop(claim);
}

impl Fixture {
    fn events_for_test(&self) -> Vec<vesta_state::events::StoredEvent> {
        EventLog::new(self.db.clone())
            .read(0, 100)
            .expect("trusted small fixture journal")
            .events
    }
}
