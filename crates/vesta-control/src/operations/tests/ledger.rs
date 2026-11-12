use super::*;
use std::sync::{Condvar, Mutex, mpsc};

struct StartGate(Arc<(Mutex<bool>, Condvar)>);
impl StartGate {
    fn new() -> Self {
        Self(Arc::new((Mutex::new(false), Condvar::new())))
    }
}
impl Drop for StartGate {
    fn drop(&mut self) {
        let (mutex, changed) = self.0.as_ref();
        let mut open = mutex.lock().unwrap_or_else(|poison| poison.into_inner());
        *open = true;
        changed.notify_all();
    }
}
fn await_start(gate: &Arc<(Mutex<bool>, Condvar)>) {
    let (mutex, changed) = gate.as_ref();
    let mut open = mutex.lock().unwrap_or_else(|poison| poison.into_inner());
    while !*open {
        open = changed
            .wait(open)
            .unwrap_or_else(|poison| poison.into_inner());
    }
}

#[test]
fn canonical_admission_is_exact_and_summaries_events_audit_disclose_no_inputs() {
    const INPUT_SENTINEL: &str = "private-input-sentinel-operations-7c9d";
    let f = Fixture::open();
    let summary = match f
        .ops
        .admit(
            &f.subject,
            "fixture.write",
            &raw(&["b", "a"], INPUT_SENTINEL),
            Some("canonical"),
        )
        .expect("admit actual planted input")
    {
        Admission::Created(summary) => summary,
        Admission::Existing(_) => panic!("sentinel fixture must be newly admitted"),
    };
    let canonical =
        r#"{"version":1,"paths":["a","b"],"value":"private-input-sentinel-operations-7c9d"}"#;
    assert_eq!(
        f.input_status(&summary.id),
        (Some(canonical.to_owned()), "available".to_owned())
    );
    let same = f
        .ops
        .admit(
            &f.subject,
            "fixture.write",
            &raw(&["a", "b"], INPUT_SENTINEL),
            Some("canonical"),
        )
        .expect("exact canonical retry");
    assert!(matches!(same, Admission::Existing(ref item) if item.id == summary.id));
    assert_eq!(f.counts(), (1, 0, 1, 1));
    let observed = f.read(&summary.id);
    assert_eq!(observed.state, OperationState::Queued);
    assert_eq!(observed.progress_percent, 0);
    let dump = format!("{observed:?}");
    assert!(
        !dump.contains(INPUT_SENTINEL) && !dump.contains("paths") && !dump.contains("canonical")
    );
    let events = EventLog::new(f.db.clone())
        .read(0, 100)
        .expect("actual journal");
    assert_eq!(events.events.len(), 1);
    let projected: serde_json::Value =
        serde_json::from_str(&events.events[0].payload_json).expect("fixed event JSON");
    let object = projected.as_object().expect("object");
    assert_eq!(object.len(), 3);
    assert_eq!(object["operationId"].as_str(), Some(summary.id.as_str()));
    assert_eq!(object["state"].as_str(), Some("queued"));
    assert_eq!(object["progress"].as_u64(), Some(0));
    assert!(!events.events[0].payload_json.contains(INPUT_SENTINEL));
    let log = AuditLog::new(f.db.clone()).expect("verify production audit");
    assert!(log.verify_chain().expect("chain verification").valid);
    for row in log.page(0, 200).expect("audit rows") {
        assert!(!row.metadata_json.contains(INPUT_SENTINEL) && !row.target.contains("paths"));
    }
}

#[test]
fn malformed_duplicate_unknown_fields_and_semantically_unstable_input_are_audited_without_rows() {
    let f = Fixture::open();
    for input in [
        r#"{"version":1,"version":1,"paths":["a"],"value":"requested"}"#,
        r#"{"version":1,"paths":["a"],"value":"requested","locks":[]}"#,
        r#"{"version":1,"paths":["../escape"],"value":"requested"}"#,
        r#"{"version":1,"paths":["a","a"],"value":"requested"}"#,
        r#"{"version":1,"paths":["a"],"value":"\u0000"}"#,
    ] {
        code(
            f.ops.admit(&f.subject, "fixture.write", input, None),
            "invalid_request",
        );
    }
    code(
        f.ops.admit(
            &f.subject,
            "fixture.unstable",
            &raw(&["a"], "requested"),
            None,
        ),
        "invalid_request",
    );
    code(
        f.ops.admit(&f.subject, "unregistered.action", "{}", None),
        "unknown_type",
    );
    code(
        f.ops.admit(
            &f.subject,
            "fixture.write",
            &"x".repeat(domain::MAX_INPUT_BYTES + 1),
            None,
        ),
        "invalid_request",
    );
    code(
        f.ops.admit(
            &f.subject,
            "fixture.write",
            &raw(&["a"], "requested"),
            Some("bad\0key"),
        ),
        "invalid_request",
    );
    assert_eq!(f.counts(), (0, 0, 9, 0));
}

#[test]
fn concurrent_exact_retries_commit_one_operation_audit_and_event() {
    let f = Fixture::open();
    let outcomes = std::thread::scope(|scope| {
        // A counted Barrier would strand already-spawned workers if spawning the
        // remaining ones panicked. This RAII gate always releases on unwinding.
        let gate = StartGate::new();
        let (ready_tx, ready_rx) = mpsc::channel();
        let mut jobs = Vec::new();
        for _ in 0..8 {
            let ops = f.ops.clone();
            let subject = f.subject.clone();
            let start = gate.0.clone();
            let ready_tx = ready_tx.clone();
            jobs.push(scope.spawn(move || {
                ready_tx.send(()).expect("retry worker ready");
                drop(ready_tx);
                await_start(&start);
                ops.admit(
                    &subject,
                    "fixture.write",
                    &raw(&["a"], "requested"),
                    Some("parallel"),
                )
            }));
        }
        drop(ready_tx);
        for _ in 0..8 {
            ready_rx.recv().expect("all retry workers really started");
        }
        drop(gate);
        jobs.into_iter()
            .map(|job| {
                job.join()
                    .expect("retry worker exited")
                    .expect("authorized exact retry")
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Admission::Created(_)))
            .count(),
        1
    );
    let ids: Vec<_> = outcomes
        .iter()
        .map(|outcome| match outcome {
            Admission::Created(row) | Admission::Existing(row) => &row.id,
        })
        .collect();
    assert!(ids.iter().all(|id| **id == *ids[0]));
    assert_eq!(f.counts(), (1, 0, 1, 1));
}

#[test]
fn retries_require_current_action_authority_and_changed_devices_payloads_and_safety_conflict() {
    let f = Fixture::open();
    let row = f.admit(&["a"], "exact");
    let other_device = another_device(&f);
    code(
        f.ops.admit(
            &other_device,
            "fixture.write",
            &raw(&["a"], "requested"),
            Some("exact"),
        ),
        "idempotency_conflict",
    );
    code(
        f.ops.admit(
            &f.subject,
            "fixture.write",
            &raw(&["a"], "different"),
            Some("exact"),
        ),
        "idempotency_conflict",
    );
    code(
        f.ops.admit(
            &f.subject,
            "fixture.safe",
            &raw(&["a"], "requested"),
            Some("exact"),
        ),
        "idempotency_conflict",
    );
    code(
        f.ops.admit(
            &f.subject,
            "fixture.confirm",
            &raw(&["a"], "requested"),
            Some("exact"),
        ),
        "idempotency_conflict",
    );
    f.deny(&f.subject, Capability::ConfigEdit, "*");
    let denied = code(
        f.ops.admit(
            &f.subject,
            "fixture.write",
            &raw(&["a"], "requested"),
            Some("exact"),
        ),
        "forbidden",
    );
    assert_eq!(denied.reason(), Some(identity::DenyReason::ExplicitDeny));
    assert_eq!(f.read(&row.id).state, OperationState::Queued);
    assert_eq!(f.counts().0, 1);
}

#[test]
fn idempotency_compares_every_persisted_binding_field_including_source_and_exact_locks() {
    for field in [
        "source",
        "type",
        "target",
        "payload_hash",
        "external_effect_idempotent",
        "requires_confirmation",
        "resource",
    ] {
        let f = Fixture::open();
        let row = f.admit(&["a"], "bound");
        f.sql(|c| {
            match field {
                "source" => { c.execute("UPDATE operations SET source='loopback' WHERE id=?", [row.id.as_str()])?; }
                "type" => { c.execute("UPDATE operations SET type='fixture.safe' WHERE id=?", [row.id.as_str()])?; }
                "target" => { c.execute("UPDATE operations SET target='other-target' WHERE id=?", [row.id.as_str()])?; }
                "payload_hash" => { c.execute("UPDATE operations SET payload_hash=? WHERE id=?", params!["0".repeat(64),row.id.as_str()])?; }
                "external_effect_idempotent" => { c.execute("UPDATE operations SET external_effect_idempotent=1 WHERE id=?", [row.id.as_str()])?; }
                "requires_confirmation" => { c.execute("UPDATE operations SET requires_confirmation=1 WHERE id=?", [row.id.as_str()])?; }
                "resource" => { c.execute("INSERT INTO operation_resources (operation_id,resource) VALUES (?,'extra-resource')", [row.id.as_str()])?; }
                _ => unreachable!(),
            }
            Ok(())
        });
        code(
            f.ops.admit(
                &f.subject,
                "fixture.write",
                &raw(&["a"], "requested"),
                Some("bound"),
            ),
            "idempotency_conflict",
        );
        assert_eq!(f.counts().0, 1);
        assert_eq!(f.read(&row.id).state, OperationState::Queued);
    }
}

#[test]
fn views_are_current_read_only_and_do_not_project_unbounded_unrelated_text() {
    let f = Fixture::open();
    let row = f.admit(&["a"], "view");
    f.sql(|c| {
        c.execute("UPDATE operations SET result_json=?,error_message=?,progress_message=?,target=?,source=? WHERE id=?",
            params!["result-secret".repeat(8_000),"error-secret".repeat(8_000),"progress-secret".repeat(8_000),
                "target-secret".repeat(8_000),"source-secret".repeat(8_000),row.id.as_str()])?;
        Ok(())
    });
    let before = f.counts();
    assert_eq!(f.read(&row.id).state, OperationState::Queued);
    let page = f
        .ops
        .list(&f.subject, 0, 1)
        .expect("bounded sanitized observation");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.next_after_seq, row.seq);
    let next = f
        .ops
        .list(&f.subject, page.next_after_seq, 1)
        .expect("empty continuation");
    assert!(next.items.is_empty());
    assert_eq!(next.next_after_seq, row.seq);
    assert_eq!(before, f.counts());
    f.deny(&f.subject, Capability::TasksView, "*");
    code(f.ops.get(&f.subject, &row.id), "forbidden");
    code(f.ops.list(&f.subject, 0, 100), "forbidden");
    code(f.ops.list(&f.subject, -1, 1), "invalid_request");
    code(f.ops.list(&f.subject, 0, 0), "invalid_request");
    code(f.ops.list(&f.subject, 0, MAX_PAGE + 1), "invalid_request");
    assert_eq!(before, f.counts());
}

#[test]
fn missing_disabled_revoked_and_expired_identity_never_observe_tasks() {
    for mutation in ["absent", "disabled", "revoked", "expired"] {
        let f = Fixture::open();
        let row = f.admit(&["a"], "current");
        let subject = if mutation == "absent" {
            Subject {
                principal: PrincipalId::parse("absent").expect("lookup key"),
                device: DeviceId::parse("absent").expect("lookup key"),
            }
        } else {
            f.sql(|c| {
                match mutation {
                    "disabled" => {
                        c.execute(
                            "UPDATE principals SET status='disabled' WHERE id=?",
                            [f.subject.principal.as_str()],
                        )?;
                    }
                    "revoked" => {
                        c.execute(
                            "UPDATE devices SET revoked_at=? WHERE id=?",
                            params![f.clock.millis(), f.subject.device.as_str()],
                        )?;
                    }
                    "expired" => {
                        c.execute(
                            "UPDATE role_assignments SET expires_at=? WHERE principal_id=?",
                            params![f.clock.millis(), f.subject.principal.as_str()],
                        )?;
                    }
                    _ => unreachable!(),
                }
                Ok(())
            });
            f.subject.clone()
        };
        code(f.ops.get(&subject, &row.id), "forbidden");
    }
}

#[test]
fn task_access_alone_never_authorizes_cancellation_or_admission() {
    let f = Fixture::open();
    let weak = seed_subject(&f.db, false);
    f.allow(&weak, Capability::TasksView, "*");
    code(
        f.ops
            .admit(&weak, "fixture.write", &raw(&["a"], "requested"), None),
        "forbidden",
    );
    f.allow(&weak, Capability::ConfigEdit, "*");
    f.allow(&weak, Capability::FilesOverwrite, "a");
    let admitted = f
        .ops
        .admit(&weak, "fixture.write", &raw(&["a"], "requested"), None)
        .expect("exact original action grants");
    let row = match admitted {
        Admission::Created(row) => row,
        _ => unreachable!(),
    };
    f.deny(&weak, Capability::FilesOverwrite, "a");
    let error = code(f.ops.request_cancel(&weak, &row.id), "forbidden");
    assert_eq!(error.reason(), Some(identity::DenyReason::ExplicitDeny));
    assert_eq!(f.read(&row.id).state, OperationState::Queued);
}

#[test]
fn cancellation_is_exact_actor_device_and_checks_scoped_allow_deny_semantics() {
    let f = Fixture::open();
    let row = f.admit(&["world/data"], "cancel-scope");
    let other = seed_subject(&f.db, true);
    code(f.ops.request_cancel(&other, &row.id), "actor_mismatch");
    code(
        f.ops.request_cancel(&another_device(&f), &row.id),
        "actor_mismatch",
    );
    f.deny(&f.subject, Capability::FilesOverwrite, "WORLD/**");
    code(f.ops.request_cancel(&f.subject, &row.id), "forbidden");
    assert_eq!(f.read(&row.id).state, OperationState::Queued);
    let custom = seed_subject(&f.db, false);
    f.allow(&custom, Capability::TasksView, "*");
    f.allow(&custom, Capability::ConfigEdit, "*");
    f.allow(&custom, Capability::FilesOverwrite, "world/**");
    code(
        f.ops.admit(
            &custom,
            "fixture.write",
            &raw(&["world2/data"], "requested"),
            None,
        ),
        "forbidden",
    );
    code(
        f.ops.admit(
            &custom,
            "fixture.write",
            &raw(&["World/data"], "requested"),
            None,
        ),
        "forbidden",
    );
    let own = match f
        .ops
        .admit(
            &custom,
            "fixture.write",
            &raw(&["world/data"], "requested"),
            None,
        )
        .expect("segment-contained allow")
    {
        Admission::Created(row) => row,
        _ => unreachable!(),
    };
    assert!(matches!(
        f.ops
            .request_cancel(&custom, &own.id)
            .expect("authorized exact cancellation"),
        CancelOutcome::Cancelled(_)
    ));
}

#[test]
fn queued_and_awaiting_confirmation_cancellation_are_atomic_and_idempotent() {
    let f = Fixture::open();
    let queued = f.admit(&["a"], "cancel-queued");
    assert!(
        matches!(f.ops.request_cancel(&f.subject, &queued.id).expect("cancel unclaimed"), CancelOutcome::Cancelled(ref row) if row.finished_at_ms.is_some())
    );
    assert!(matches!(
        f.ops
            .request_cancel(&f.subject, &queued.id)
            .expect("terminal no-op"),
        CancelOutcome::AlreadyTerminal(_)
    ));
    let waiting = match f
        .ops
        .admit(
            &f.subject,
            "fixture.confirm",
            &raw(&["b"], "requested"),
            None,
        )
        .expect("awaiting fixture")
    {
        Admission::Created(row) => row,
        _ => unreachable!(),
    };
    assert_eq!(waiting.state, OperationState::AwaitingConfirmation);
    assert!(matches!(
        f.ops
            .request_cancel(&f.subject, &waiting.id)
            .expect("cancel awaiting"),
        CancelOutcome::Cancelled(_)
    ));
    assert!(f.held(&queued.id).is_empty() && f.held(&waiting.id).is_empty());
    assert!(f.owner(&queued.id).is_none());
}

#[test]
fn changed_source_is_not_binding_authority_for_cancel_claim_or_restart() {
    let f = Fixture::open();
    let queued = f.admit(&["a"], "source-cancel");
    f.sql(|c| {
        c.execute(
            "UPDATE operations SET source='loopback' WHERE id=?",
            [queued.id.as_str()],
        )?;
        Ok(())
    });
    code(
        f.ops.request_cancel(&f.subject, &queued.id),
        "binding_mismatch",
    );
    assert_eq!(f.read(&queued.id).state, OperationState::Queued);
    let mut owner = f.ops.open_owner().expect("owned reconciliation");
    assert!(
        owner
            .claim_next()
            .expect("invalid binding is safely failed")
            .is_none()
    );
    assert_eq!(f.read(&queued.id).state, OperationState::Failed);
    assert_eq!(
        f.error_code(&queued.id).as_deref(),
        Some("binding_mismatch")
    );
    let active = f.admit(&["b"], "source-restart");
    let claim = owner.claim_next().expect("claim").expect("active fixture");
    assert_eq!(queue::claim_id(&claim), &active.id);
    f.sql(|c| {
        c.execute(
            "UPDATE operations SET source='loopback' WHERE id=?",
            [active.id.as_str()],
        )?;
        Ok(())
    });
    drop(claim);
    drop(owner);
    let owner = f
        .ops
        .open_owner()
        .expect("reconcile changed source at restart");
    assert_eq!(f.read(&active.id).state, OperationState::Failed);
    assert_eq!(
        f.error_code(&active.id).as_deref(),
        Some("binding_mismatch")
    );
    assert!(f.held(&active.id).is_empty());
    drop(owner);
}

#[test]
fn persisted_hash_nul_byte_size_and_header_bounds_fail_closed() {
    for corruption in ["hash", "nul", "bytes", "header", "resource-overflow"] {
        let f = Fixture::open();
        let row = f.admit(&["a"], "corrupt");
        f.sql(|c| {
            match corruption {
                "hash" => {
                    c.execute(
                        "UPDATE operations SET payload_hash=? WHERE id=?",
                        params!["0".repeat(64), row.id.as_str()],
                    )?;
                }
                "nul" => {
                    c.execute(
                        "UPDATE operations SET input_json=? WHERE id=?",
                        params!["{}\0trailing-untrusted", row.id.as_str()],
                    )?;
                }
                "bytes" => {
                    let huge = format!("\"{}\"", "é".repeat(40_000));
                    c.execute(
                        "UPDATE operations SET input_json=?,payload_hash=? WHERE id=?",
                        params![huge, input::digest(&huge), row.id.as_str()],
                    )?;
                }
                "header" => {
                    c.execute(
                        "UPDATE operations SET type=? WHERE id=?",
                        params!["x".repeat(100_000), row.id.as_str()],
                    )?;
                }
                "resource-overflow" => {
                    for index in 0..33 {
                        c.execute(
                            "INSERT INTO operation_resources (operation_id,resource) VALUES (?,?)",
                            params![row.id.as_str(), format!("extra-{index}")],
                        )?;
                    }
                }
                _ => unreachable!(),
            }
            Ok(())
        });
        let error = f
            .ops
            .request_cancel(&f.subject, &row.id)
            .expect_err("corrupt binding cannot cancel");
        if matches!(corruption, "header" | "resource-overflow") {
            assert_eq!(error.code(), "state_operation_corrupt");
        } else {
            assert_eq!(error.code(), "input_unavailable");
        }
        assert_eq!(f.read(&row.id).state, OperationState::Queued);
        assert!(f.held(&row.id).is_empty());
        let rendered = format!("{error:?} {error}");
        assert!(
            !rendered.contains("trailing-untrusted") && !rendered.contains("UPDATE operations")
        );
    }
}

#[test]
fn contradictory_active_cancel_flag_is_not_silently_healed_by_public_mutation() {
    let f = Fixture::open();
    let row = f.admit(&["a"], "contradictory-cancel");
    let mut owner = f.ops.open_owner().expect("owner");
    let claim = owner.claim_next().expect("claim").expect("claim exists");
    f.sql(|c| {
        c.execute(
            "UPDATE operations SET cancel_requested=1 WHERE id=?",
            [row.id.as_str()],
        )?;
        Ok(())
    });
    let before = f.counts();
    let token = f.owner(&row.id);
    code(
        f.ops.request_cancel(&f.subject, &row.id),
        "state_operation_corrupt",
    );
    assert_eq!(before, f.counts());
    assert_eq!(f.read(&row.id).state, OperationState::Validating);
    assert!(token == f.owner(&row.id));
    assert_eq!(f.held(&row.id), vec!["fixture-files:a".to_owned()]);
    drop(claim);
}

#[test]
fn exact_retry_does_not_hide_corrupt_terminal_or_active_lock_ownership() {
    for terminal in [false, true] {
        let f = Fixture::open();
        let row = f.admit(&["a"], "retry-locks");
        let mut owner = f.ops.open_owner().expect("owner");
        let claim = owner.claim_next().expect("claim").expect("claim exists");
        if terminal {
            f.terminal(&row.id, true);
            f.sql(|c| { c.execute("INSERT INTO resource_locks (resource,operation_id,acquired_at) VALUES ('fixture-files:a',?,?)", params![row.id.as_str(),INITIAL_MS])?; Ok(()) });
        } else {
            f.sql(|c| {
                c.execute(
                    "DELETE FROM resource_locks WHERE operation_id=?",
                    [row.id.as_str()],
                )?;
                Ok(())
            });
        }
        let before = f.counts();
        code(
            f.ops.admit(
                &f.subject,
                "fixture.write",
                &raw(&["a"], "requested"),
                Some("retry-locks"),
            ),
            "state_operation_corrupt",
        );
        assert_eq!(before, f.counts());
        drop(claim);
    }
}

#[test]
fn noncanonical_hash_matching_persisted_input_cannot_select_a_binding() {
    let f = Fixture::open();
    let row = f.admit(&["a"], "noncanonical");
    let noncanonical = "{ \"value\":\"requested\", \"paths\":[\"a\"], \"version\":1 }";
    f.sql(|c| {
        c.execute(
            "UPDATE operations SET input_json=?,payload_hash=? WHERE id=?",
            params![noncanonical, input::digest(noncanonical), row.id.as_str()],
        )?;
        Ok(())
    });
    code(
        f.ops.request_cancel(&f.subject, &row.id),
        "binding_mismatch",
    );
    let mut owner = f
        .ops
        .open_owner()
        .expect("hash-valid queued row is not executed during startup");
    assert!(owner.claim_next().expect("canonicality refusal").is_none());
    assert_eq!(f.error_code(&row.id).as_deref(), Some("binding_mismatch"));
    assert_eq!(f.read(&row.id).state, OperationState::Failed);
}

/// RAII unblocks the writer even if a test assertion unwinds. No timeout or
/// dropped-thread handle is treated as completion; every scoped thread is joined.
struct Release(Option<mpsc::Sender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[test]
fn admitted_writer_time_not_command_time_controls_expiry_and_created_timestamp() {
    for expires in [false, true] {
        let f = Fixture::open();
        if expires {
            f.sql(|c| {
                c.execute(
                    "UPDATE role_assignments SET expires_at=? WHERE principal_id=?",
                    params![INITIAL_MS + 1_000, f.subject.principal.as_str()],
                )?;
                Ok(())
            });
        }
        let outcome = std::thread::scope(|scope| {
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let release = Release(Some(release_tx));
            let db = f.db.clone();
            let holder = scope.spawn(move || {
                db.write(|_| {
                    entered_tx.send(()).expect("writer admission signal");
                    release_rx.recv().expect("release admitted writer");
                    Ok(())
                })
                .expect("holder transaction completes");
            });
            entered_rx
                .recv()
                .expect("actual admitted writer is holding gate");
            let (started_tx, started_rx) = mpsc::channel();
            let ops = f.ops.clone();
            let subject = f.subject.clone();
            let requester = scope.spawn(move || {
                started_tx.send(()).expect("command start signal");
                ops.admit(
                    &subject,
                    "fixture.write",
                    &raw(&["a"], "requested"),
                    Some("writer-time"),
                )
            });
            started_rx
                .recv()
                .expect("request thread started with writer gate held");
            f.clock.set(INITIAL_MS + 2_000);
            drop(release);
            holder.join().expect("holder actually exited");
            requester.join().expect("requester actually exited")
        });
        if expires {
            code(outcome, "forbidden");
            assert_eq!(f.counts().0, 0);
        } else {
            let row = match outcome.expect("current grant still live") {
                Admission::Created(row) => row,
                _ => unreachable!(),
            };
            assert_eq!(row.created_at_ms, INITIAL_MS + 2_000);
            assert_eq!(row.updated_at_ms, INITIAL_MS + 2_000);
        }
    }
}

#[test]
fn cancellation_waiting_on_writer_sees_that_writers_committed_revocation() {
    let f = Fixture::open();
    let row = f.admit(&["a"], "revocation-race");
    let outcome = std::thread::scope(|scope| {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release = Release(Some(release_tx));
        let db = f.db.clone();
        let device = f.subject.device.clone();
        let holder = scope.spawn(move || {
            db.write(|c| {
                c.execute(
                    "UPDATE devices SET revoked_at=? WHERE id=?",
                    params![INITIAL_MS, device.as_str()],
                )?;
                entered_tx.send(()).expect("revocation held uncommitted");
                release_rx.recv().expect("release revocation commit");
                Ok(())
            })
            .expect("revocation commits");
        });
        entered_rx.recv().expect("revocation writer admitted");
        let (started_tx, started_rx) = mpsc::channel();
        let ops = f.ops.clone();
        let subject = f.subject.clone();
        let id = row.id.clone();
        let requester = scope.spawn(move || {
            started_tx.send(()).expect("request thread starts");
            ops.request_cancel(&subject, &id)
        });
        started_rx
            .recv()
            .expect("cancel begins behind admitted writer");
        drop(release);
        holder.join().expect("revoker actually exits");
        requester.join().expect("requester actually exits")
    });
    assert_eq!(
        code(outcome, "forbidden").reason(),
        Some(identity::DenyReason::NoIdentity)
    );
    let stored =
        f.db.read(|c| {
            Ok(c.query_row(
                "SELECT state,cancel_requested FROM operations WHERE id=?",
                [row.id.as_str()],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )?)
        })
        .expect("trusted state check");
    assert_eq!(stored, ("queued".to_owned(), 0));
}
