//! Public observations over real SQLite state, without production fixture bindings.
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tempfile::TempDir;
use vesta_control::{
    identity::{BuiltinRole, DenyReason, DeviceId, IdentityService, PrincipalId, Subject},
    operations::{
        CancelOutcome, MAX_PAGE, OperationError, OperationErrorCode, OperationId, OperationState,
        Operations, OperationsResult,
    },
};
use vesta_state::{Clock, StateDatabase, StateDirectory, audit::AuditLog, events::EventLog};

const NOW: i64 = 1_791_115_200_000;
const STATES: [OperationState; 9] = [
    OperationState::Queued,
    OperationState::Validating,
    OperationState::AwaitingConfirmation,
    OperationState::Running,
    OperationState::Cancelling,
    OperationState::Succeeded,
    OperationState::Failed,
    OperationState::Cancelled,
    OperationState::RecoveryRequired,
];
const INPUT: &str = r#"{"version":1,"paths":["world"],"value":"sentinel-input"}"#;

#[test]
fn activity_projects_typed_inputs_with_current_capabilities_and_purge() {
    let f = Fixture::open();
    let generation = "00112233445566778899aabbccddeeff";
    // Use the exact canonical order emitted by GameCommand (struct order, not map order).
    let raw = format!(
        "{{\"expected_generation\":\"{generation}\",\"line\":{}}}",
        serde_json::to_string("say Hello, players!").unwrap()
    );
    f.db.write(|c| {
        c.execute(
            "UPDATE operations SET type='game.console',input_json=?,payload_hash=? WHERE id=?",
            params![
                raw,
                hex::encode(Sha256::digest(raw.as_bytes())),
                id(5).as_str()
            ],
        )?;
        c.execute(
            "INSERT INTO user_minecraft_names(principal_id,name) VALUES (?,'PlayerOne')",
            [f.viewer.principal.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    let (old, details) = f
        .operations
        .activity_get(&f.viewer, &id(5))
        .unwrap()
        .unwrap();
    assert_eq!(Some(old), f.operations.get(&f.viewer, &id(5)).unwrap());
    let value = serde_json::to_value(details).unwrap();
    assert_eq!(value["kind"], "console.command");
    assert_eq!(value["actor"]["display_name"], "PlayerOne");
    assert_eq!(value["actor"]["device_name"], "Fixture");
    assert_eq!(value["actor"]["principal_id"], f.viewer.principal.as_str());
    assert_eq!(value["summary"]["command"], "say Hello, players!");
    assert_eq!(value["outcome"], "stdin_written");
    let value = serde_json::to_value(
        f.operations
            .activity_get(&f.other, &id(5))
            .unwrap()
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(
        value["summary"],
        serde_json::json!({"command_redacted":true})
    );
    // A different enrolled device of the same principal retains the issuer exception.
    let value = serde_json::to_value(
        f.operations
            .activity_get(&f.second_device, &id(5))
            .unwrap()
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(value["summary"]["command"], "say Hello, players!");
    f.db.write(|c| {
        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES ('activity-read',?,'console.read','*','allow','fixture',?)",params![f.other.principal.as_str(),NOW])?;
        Ok(())
    }).unwrap();
    let value = serde_json::to_value(
        f.operations
            .activity_get(&f.other, &id(5))
            .unwrap()
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(value["summary"]["command"], "say Hello, players!");
    f.db.write(|c| {
        c.execute(
            "UPDATE principal_overrides SET effect='deny' WHERE id='activity-read'",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    let value = serde_json::to_value(
        f.operations
            .activity_get(&f.other, &id(5))
            .unwrap()
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(
        value["summary"],
        serde_json::json!({"command_redacted":true})
    );
    f.db.write(|c| {
        c.execute(
            "UPDATE operations SET input_json=NULL,input_status='purged' WHERE id=?",
            [id(5).as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    let value = serde_json::to_value(
        f.operations
            .activity_get(&f.viewer, &id(5))
            .unwrap()
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(value["summary"], serde_json::Value::Null);
    assert_eq!(value["summary_purged"], true);
    assert_eq!(value["outcome"], "stdin_written");
    assert_eq!(
        f.operations
            .activity_list(&f.viewer, 0, 1, Some("console.command"), true)
            .unwrap()
            .len(),
        1
    );
    assert!(
        f.operations
            .activity_list(&f.other, 0, 10, Some("console.command"), true)
            .unwrap()
            .is_empty()
    );
    assert!(
        f.operations
            .activity_list(&f.viewer, 0, 10, Some("arbitrary"), false)
            .is_err()
    );
    assert!(f.operations.activity_get(&f.ungranted, &id(5)).is_err());
    f.revoke();
    assert!(f.operations.activity_get(&f.viewer, &id(5)).is_err());
}

#[test]
fn activity_moderation_lifecycle_outcomes_and_legacy_are_allowlisted() {
    let f = Fixture::open();
    let raw = "{\"expected_generation\":\"00112233445566778899aabbccddeeff\",\"action\":{\"action\":\"ban\",\"player\":\"PlayerOne\",\"reason\":\"Griefing\",\"duration_seconds\":3600}}";
    f.db.write(|c| {
        c.execute(
            "UPDATE operations SET type='players.moderate',input_json=?,payload_hash=? WHERE id=?",
            params![
                raw,
                hex::encode(Sha256::digest(raw.as_bytes())),
                id(5).as_str()
            ],
        )?;
        Ok(())
    })
    .unwrap();
    let value = serde_json::to_value(
        f.operations
            .activity_get(&f.other, &id(5))
            .unwrap()
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(
        value["summary"],
        serde_json::json!({"action":"ban","player":"PlayerOne","reason":"Griefing","duration_seconds":3600})
    );
    f.db.write(|c| {
        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES ('activity-players',?,'players.view','*','deny','fixture',?)",params![f.viewer.principal.as_str(),NOW])?;
        Ok(())
    }).unwrap();
    let value = serde_json::to_value(
        f.operations
            .activity_get(&f.viewer, &id(5))
            .unwrap()
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(
        value["summary"],
        serde_json::json!({"details_redacted":true})
    );
    for (code, expected) in [
        ("game_stale_generation", "stale_generation"),
        ("game_stop_timeout", "timeout"),
        ("game_stdin_unavailable", "write_failed"),
        ("secret-exception-path", "failed"),
    ] {
        f.db.write(|c| {
            c.execute(
                "UPDATE operations SET error_code=? WHERE id=?",
                params![code, id(6).as_str()],
            )?;
            Ok(())
        })
        .unwrap();
        let value = serde_json::to_value(
            f.operations
                .activity_get(&f.viewer, &id(6))
                .unwrap()
                .unwrap()
                .1,
        )
        .unwrap();
        assert_eq!(value["outcome"], expected);
        assert_eq!(value["summary"], serde_json::Value::Null);
        assert_eq!(value["kind"], "other");
    }
    f.db.write(|c| {
        c.execute("UPDATE operations SET type='game.start',input_json='{}',payload_hash=?,result_json=? WHERE id=?",params![hex::encode(Sha256::digest(b"{}")),"{\"generation\":\"00112233445566778899aabbccddeeff\",\"private\":\"secret\"}",id(5).as_str()])?;
        Ok(())
    }).unwrap();
    let value = serde_json::to_value(
        f.operations
            .activity_get(&f.viewer, &id(5))
            .unwrap()
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(
        value["summary"],
        serde_json::json!({"target_generation":null,"result_generation":"00112233445566778899aabbccddeeff"})
    );
    assert_eq!(value["outcome"], "process_started");
    let value = serde_json::to_value(
        f.operations
            .activity_get(&f.viewer, &id(8))
            .unwrap()
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(value["outcome"], "recovery_required");
}

#[test]
fn activity_automation_identity_and_audit_player_targets_are_authorized() {
    use vesta_state::audit::{AuditActor, AuditRecord};
    let f = Fixture::open();
    f.db.write(|c| {
        c.execute("UPDATE devices SET public_key_sha256=? WHERE id=?",params![format!("{:x}",Sha256::digest(format!("vesta-automation-device:{}",f.viewer.principal.as_str()))),f.viewer.device.as_str()])?;
        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES ('activity-audit',?,'audit.view','*','allow','fixture',?)",params![f.other.principal.as_str(),NOW])?;
        c.execute("INSERT INTO moderation_actions(server_id,principal_id,device_id,operation_id,action,player,created_at,outcome,expiry_state) VALUES (?,?,?,?,'kick','PlayerOne',?,'succeeded','none')",params![f.db.server_id(),f.viewer.principal.as_str(),f.viewer.device.as_str(),id(5).as_str(),NOW])?;
        Ok(())
    }).unwrap();
    let details = serde_json::to_value(
        f.operations
            .activity_get(&f.other, &id(5))
            .unwrap()
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(
        details["actor"],
        serde_json::json!({"kind":"workflow","name":"Vesta automation"})
    );
    f.audit
        .append_record(
            &AuditRecord::new(
                AuditActor::system("operation-kernel".into()).unwrap(),
                "operation.finish".into(),
                "operation:observed-5".into(),
                "ok".into(),
                Some(id(5).as_str().into()),
                [],
            )
            .unwrap(),
        )
        .unwrap();
    let notice = f
        .operations
        .audit_notices(&f.other, 0, 1)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(notice.target.as_deref(), Some("PlayerOne"));
    f.db.write(|c| {
        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES ('activity-player-target',?,'players.view','*','deny','fixture',?)",params![f.other.principal.as_str(),NOW])?;
        Ok(())
    }).unwrap();
    assert!(
        f.operations.audit_notices(&f.other, 0, 1).unwrap()[0]
            .target
            .is_none()
    );
    assert!(f.operations.audit_notices(&f.viewer, 0, 1).is_err());
}

struct FixedClock;
impl Clock for FixedClock {
    fn millis(&self) -> i64 {
        NOW
    }
}

fn subject() -> Subject {
    Subject {
        principal: PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).unwrap(),
        device: DeviceId::parse(&uuid::Uuid::new_v4().to_string()).unwrap(),
    }
}
fn id(index: usize) -> OperationId {
    OperationId::parse(&format!("observed-{index}")).unwrap()
}

struct Fixture {
    _root: TempDir,
    db: StateDatabase,
    audit: AuditLog,
    events: EventLog,
    operations: Operations,
    viewer: Subject,
    other: Subject,
    second_device: Subject,
    ungranted: Subject,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.db
            .close()
            .expect("close shared SQLite state before temporary directory removal");
    }
}
impl Fixture {
    fn open() -> Self {
        Self::open_with_clock(Arc::new(FixedClock))
    }
    fn open_with_clock(clock: Arc<dyn Clock>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let db = StateDatabase::open(
            StateDirectory::new(root.path().join("state"), None).unwrap(),
            clock,
        )
        .unwrap();
        // Seed built-in role definitions through the actual production constructor.
        let _identities = IdentityService::new(db.clone()).unwrap();
        let viewer = subject();
        let other = subject();
        let ungranted = subject();
        let second_device = Subject {
            principal: viewer.principal.clone(),
            device: subject().device,
        };
        db.write(|connection| {
            for actor in [&viewer, &other, &ungranted] {
                connection.execute(
                    "INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) \
                     VALUES (?,?,'Fixture','active',?,?)",
                    params![actor.principal.as_str(), db.server_id(), NOW, NOW],
                )?;
            }
            for (index, actor) in [&viewer, &other, &ungranted, &second_device].into_iter().enumerate() {
                connection.execute(
                    "INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) \
                     VALUES (?,?,?,'Fixture',?,?)",
                    params![actor.device.as_str(), db.server_id(), actor.principal.as_str(), format!("{index:064x}"), NOW],
                )?;
            }
            for actor in [&viewer, &other] {
                connection.execute(
                    "INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,?,'fixture',?)",
                    params![uuid::Uuid::new_v4().to_string(), actor.principal.as_str(), BuiltinRole::Viewer.id(), NOW],
                )?;
            }
            for (index, state) in STATES.into_iter().enumerate() {
                Self::insert(connection, &db, &viewer, index, state)?;
            }
            Ok(())
        }).unwrap();
        let audit = AuditLog::new(db.clone()).unwrap();
        let events = EventLog::new(db.clone());
        let operations = Operations::new(db.clone()).unwrap();
        Self {
            _root: root,
            db,
            audit,
            events,
            operations,
            viewer,
            other,
            second_device,
            ungranted,
        }
    }
    fn insert(
        connection: &rusqlite::Connection,
        db: &StateDatabase,
        actor: &Subject,
        index: usize,
        state: OperationState,
    ) -> vesta_state::Result<()> {
        let claimed = matches!(
            state,
            OperationState::Validating | OperationState::Running | OperationState::Cancelling
        );
        let started = matches!(
            state,
            OperationState::Running
                | OperationState::Cancelling
                | OperationState::Succeeded
                | OperationState::Failed
                | OperationState::RecoveryRequired
        );
        connection.execute(
            "INSERT INTO operations (id,server_id,actor_principal_id,device_id,source,type,target,payload_hash,\
             state,created_at,updated_at,finished_at,input_json,input_status,external_effect_started,owner_token,cancel_requested,requires_confirmation,external_effect_idempotent) \
             VALUES (?,?,?,?,'operation-request','legacy.unbound','sentinel-target',?,?,?,?,?,?,'available',?,?,?,0,0)",
            params![id(index).as_str(), db.server_id(), actor.principal.as_str(), actor.device.as_str(),
                hex::encode(Sha256::digest(INPUT.as_bytes())), state.as_str(), NOW, NOW + 1,
                state.is_terminal().then_some(NOW + 2), INPUT, started, claimed.then_some("sentinel-owner"),
                state == OperationState::Cancelling],
        )?;
        let resource = format!("fixture:resource-{index}");
        connection.execute(
            "INSERT INTO operation_resources (operation_id,resource) VALUES (?,?)",
            params![id(index).as_str(), resource],
        )?;
        if claimed || state == OperationState::RecoveryRequired {
            connection.execute(
                "INSERT INTO resource_locks (resource,operation_id,acquired_at) VALUES (?,?,?)",
                params![resource, id(index).as_str(), NOW],
            )?;
        }
        Ok(())
    }
    fn revoke(&self) {
        self.db
            .write(|connection| {
                assert_eq!(
                    connection.execute(
                        "UPDATE devices SET revoked_at=? WHERE id=?",
                        params![NOW, self.viewer.device.as_str()]
                    )?,
                    1
                );
                Ok(())
            })
            .unwrap();
    }
    fn operation_facts(&self) -> Vec<OperationFacts> {
        self.db.read(|connection| {
            let mut query = connection.prepare("SELECT id,state,cancel_requested,owner_token,finished_at FROM operations ORDER BY seq")?;
            Ok(query.query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        }).unwrap()
    }
    fn lock_facts(&self) -> Vec<(String, String)> {
        self.db
            .read(|connection| {
                let mut query = connection.prepare(
                    "SELECT resource,operation_id FROM resource_locks ORDER BY resource",
                )?;
                Ok(query
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .unwrap()
    }
    fn audit_count(&self) -> i64 {
        self.db
            .read(|connection| {
                Ok(connection
                    .query_row("SELECT count(*) FROM audit_events", [], |row| row.get(0))?)
            })
            .unwrap()
    }
}

// Selected lifecycle columns only; this is not a byte-for-byte whole-row snapshot.
type OperationFacts = (String, String, i64, Option<String>, Option<i64>);
type ClockGate = (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);
#[derive(Default)]
struct SnapshotClock(std::sync::Mutex<Option<ClockGate>>);
impl Clock for SnapshotClock {
    fn millis(&self) -> i64 {
        let gate = self.0.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            entered.send(()).unwrap();
            release
                .recv_timeout(std::time::Duration::from_secs(8))
                .expect("explicit release before clock timeout");
        }
        NOW
    }
}

#[test]
fn public_reads_observe_policy_and_task_in_one_pinned_snapshot() {
    use std::{sync::mpsc, thread, time::Duration};
    for use_list in [false, true] {
        let clock = Arc::new(SnapshotClock::default());
        let fixture = Fixture::open_with_clock(clock.clone());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        *clock.0.lock().unwrap() = Some((entered_tx, release_rx));
        let operations = fixture.operations.clone();
        let subject = fixture.viewer.clone();
        let reader = thread::spawn(move || {
            if use_list {
                operations.list(&subject, 0, 1).map(|page| page.items)
            } else {
                operations
                    .get(&subject, &id(0))
                    .map(|item| item.into_iter().collect())
            }
        });
        let entered = entered_rx.recv_timeout(Duration::from_secs(3));
        let db = fixture.db.clone();
        let device = fixture.viewer.device.clone();
        let (committed_tx, committed_rx) = mpsc::channel();
        let writer = thread::spawn(move || {
            let outcome = db.write(|connection| {
                assert_eq!(connection.execute("UPDATE devices SET revoked_at=? WHERE id=?", params![NOW, device.as_str()])?, 1);
                assert_eq!(connection.execute("UPDATE operations SET state='succeeded',finished_at=?,updated_at=? WHERE id=?", params![NOW + 3, NOW + 3, id(0).as_str()])?, 1);
                Ok(())
            });
            committed_tx.send(outcome).unwrap();
        });
        let committed = committed_rx.recv_timeout(Duration::from_secs(3));
        let released = release_tx.send(());
        let read_outcome = reader.join();
        let writer_outcome = writer.join();
        entered.expect("public read reached its clock after snapshot pinning");
        committed
            .expect("revocation and lifecycle committed before read resumed")
            .unwrap();
        released.expect("reader still waited for explicit release");
        writer_outcome.unwrap();
        let items = read_outcome
            .unwrap()
            .expect("old snapshot retains old authorization");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].state, OperationState::Queued);
        assert_eq!(items[0].finished_at_ms, None);
        assert_eq!(items[0].updated_at_ms, NOW + 1);
        let result = if use_list {
            fixture.operations.list(&fixture.viewer, 0, 1).map(|_| ())
        } else {
            fixture.operations.get(&fixture.viewer, &id(0)).map(|_| ())
        };
        assert_eq!(
            rejection(result),
            (OperationErrorCode::Forbidden, Some(DenyReason::NoIdentity))
        );
    }
}

fn rejection<T>(result: OperationsResult<T>) -> (OperationErrorCode, Option<DenyReason>) {
    match result {
        Err(OperationError::Rejected { code, reason }) => (code, reason),
        Err(other) => panic!("expected a fixed domain refusal, got {other:?}"),
        Ok(_) => panic!("expected refusal"),
    }
}

#[test]
fn operation_ids_admit_exact_boundaries_and_refuse_invalid_grammar() {
    for accepted in [
        "a".to_owned(),
        "a".repeat(64),
        "Az09-_.".to_owned(),
        "retained-legacy-id".to_owned(),
    ] {
        assert_eq!(OperationId::parse(&accepted).unwrap().as_str(), accepted);
    }
    for refused in [
        String::new(),
        "a".repeat(65),
        "a b".into(),
        "a/b".into(),
        "a:b".into(),
        "a\nb".into(),
        "café".into(),
        "nul\0byte".into(),
        " leading".into(),
    ] {
        assert_eq!(
            rejection(OperationId::parse(&refused)),
            (OperationErrorCode::InvalidRequest, None)
        );
    }
}

#[test]
fn task_access_distinguishes_missing_ungranted_mismatched_and_denied_identities() {
    let fixture = Fixture::open();
    assert!(
        fixture
            .operations
            .get(&fixture.viewer, &id(0))
            .unwrap()
            .is_some()
    );
    let missing = subject();
    let mismatched = Subject {
        principal: fixture.other.principal.clone(),
        device: fixture.viewer.device.clone(),
    };
    for (actor, reason) in [
        (&missing, DenyReason::NoIdentity),
        (&mismatched, DenyReason::NoIdentity),
        (&fixture.ungranted, DenyReason::NotGranted),
    ] {
        assert_eq!(
            rejection(fixture.operations.get(actor, &id(0))),
            (OperationErrorCode::Forbidden, Some(reason))
        );
        assert_eq!(
            rejection(fixture.operations.list(actor, 0, 1)),
            (OperationErrorCode::Forbidden, Some(reason))
        );
        assert_eq!(
            rejection(fixture.operations.request_cancel(actor, &id(5))),
            (OperationErrorCode::Forbidden, Some(reason))
        );
    }
    fixture.db.write(|connection| {
        connection.execute("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) \
            VALUES (?,?,'tasks.view','*','deny','fixture',?)", params![uuid::Uuid::new_v4().to_string(), fixture.viewer.principal.as_str(), NOW])?;
        Ok(())
    }).unwrap();
    assert_eq!(
        rejection(fixture.operations.get(&fixture.viewer, &id(0))),
        (
            OperationErrorCode::Forbidden,
            Some(DenyReason::ExplicitDeny)
        )
    );
    assert_eq!(
        rejection(fixture.operations.list(&fixture.viewer, 0, 1)),
        (
            OperationErrorCode::Forbidden,
            Some(DenyReason::ExplicitDeny)
        )
    );
    assert_eq!(
        rejection(fixture.operations.request_cancel(&fixture.viewer, &id(5))),
        (
            OperationErrorCode::Forbidden,
            Some(DenyReason::ExplicitDeny)
        )
    );
}

#[test]
fn ordered_pages_are_bounded_and_resumable_without_clamping() {
    let fixture = Fixture::open();
    let first = fixture.operations.list(&fixture.viewer, 0, 3).unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>(),
        (0..3).map(id).collect::<Vec<_>>()
    );
    assert_eq!(first.next_after_seq, 3);
    let next = fixture
        .operations
        .list(&fixture.viewer, first.next_after_seq, 4)
        .unwrap();
    assert_eq!(
        next.items
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>(),
        (3..7).map(id).collect::<Vec<_>>()
    );
    assert_eq!(next.next_after_seq, 7);
    let tail = fixture
        .operations
        .list(&fixture.viewer, next.next_after_seq, 4)
        .unwrap();
    assert_eq!(
        tail.items
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>(),
        (7..9).map(id).collect::<Vec<_>>()
    );
    assert_eq!(tail.next_after_seq, 9);
    for after in [9, i64::MAX] {
        let empty = fixture.operations.list(&fixture.viewer, after, 1).unwrap();
        assert!(empty.items.is_empty());
        assert_eq!(empty.next_after_seq, after);
    }
    for (cursor, limit) in [
        (-1, 1),
        (i64::MIN, 1),
        (0, 0),
        (0, MAX_PAGE + 1),
        (0, usize::MAX),
    ] {
        assert_eq!(
            rejection(fixture.operations.list(&fixture.viewer, cursor, limit)),
            (OperationErrorCode::InvalidRequest, None)
        );
    }
    assert_eq!(
        fixture.operations.get(&fixture.viewer, &id(99)).unwrap(),
        None
    );
    fixture
        .db
        .write(|connection| {
            for index in 9..210 {
                Fixture::insert(
                    connection,
                    &fixture.db,
                    &fixture.viewer,
                    index,
                    OperationState::Queued,
                )?;
            }
            Ok(())
        })
        .unwrap();
    let bound = fixture
        .operations
        .list(&fixture.viewer, 0, MAX_PAGE)
        .unwrap();
    assert_eq!(MAX_PAGE, 200);
    assert_eq!(bound.items.len(), 200);
    assert_eq!(bound.next_after_seq, 200);
    assert_eq!(
        fixture
            .operations
            .list(&fixture.viewer, bound.next_after_seq, MAX_PAGE)
            .unwrap()
            .items
            .len(),
        10
    );
}

#[test]
fn schema_refuses_a_foreign_server_reference_without_creating_an_operation() {
    let fixture = Fixture::open();
    let before = fixture.operation_facts();
    fixture.db.write(|connection| {
        let error = connection.execute(
            "INSERT INTO operations (id,server_id,actor_principal_id,device_id,source,type,target,payload_hash,state,created_at,updated_at,requires_confirmation,external_effect_idempotent) \
             VALUES ('foreign-reference',?,?,?,'fixture','legacy.unbound','target',?,'queued',?,?,0,0)",
            params![uuid::Uuid::new_v4().to_string(), fixture.viewer.principal.as_str(), fixture.viewer.device.as_str(), "a".repeat(64), NOW, NOW],
        ).expect_err("foreign server reference must fail");
        match error {
            rusqlite::Error::SqliteFailure(native, _) => assert_eq!(native.extended_code, rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
            other => panic!("expected actual foreign-key rejection, got {other:?}"),
        }
        Ok(())
    }).unwrap();
    assert_eq!(fixture.operation_facts(), before);
    assert_eq!(
        fixture
            .operations
            .get(
                &fixture.viewer,
                &OperationId::parse("foreign-reference").unwrap()
            )
            .unwrap(),
        None
    );
    // This proves the schema constraint, not filtering of an actually stored foreign row.
}

#[test]
fn prior_successful_reads_never_cache_authority_after_device_revocation() {
    let fixture = Fixture::open();
    assert!(
        fixture
            .operations
            .get(&fixture.viewer, &id(0))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        fixture
            .operations
            .list(&fixture.viewer, 0, MAX_PAGE)
            .unwrap()
            .items
            .len(),
        9
    );
    fixture.revoke();
    for result in [
        fixture.operations.get(&fixture.viewer, &id(0)).map(|_| ()),
        fixture.operations.list(&fixture.viewer, 0, 1).map(|_| ()),
        fixture
            .operations
            .request_cancel(&fixture.viewer, &id(5))
            .map(|_| ()),
    ] {
        assert_eq!(
            rejection(result),
            (OperationErrorCode::Forbidden, Some(DenyReason::NoIdentity))
        );
    }
}

#[test]
fn closed_bindings_and_foreign_actors_cannot_cancel_but_refusals_are_audited() {
    let fixture = Fixture::open();
    let before = fixture.operation_facts();
    let locks = fixture.lock_facts();
    let events = fixture.events.read(0, 500).unwrap();
    let audits = fixture.audit_count();
    fixture
        .db
        .write(|connection| {
            // Even the private unit-test binding name must be unknown in a production library.
            connection.execute(
                "UPDATE operations SET type='fixture.write' WHERE id=?",
                [id(0).as_str()],
            )?;
            Ok(())
        })
        .unwrap();
    for index in [0, 1, 2, 3, 4] {
        assert_eq!(
            rejection(
                fixture
                    .operations
                    .request_cancel(&fixture.viewer, &id(index))
            ),
            (OperationErrorCode::UnknownType, None)
        );
    }
    assert_eq!(
        rejection(fixture.operations.request_cancel(&fixture.viewer, &id(8))),
        (OperationErrorCode::RecoveryRequired, None)
    );
    for actor in [&fixture.other, &fixture.second_device] {
        assert_eq!(
            rejection(fixture.operations.request_cancel(actor, &id(0))),
            (OperationErrorCode::ActorMismatch, None)
        );
        assert_eq!(
            rejection(fixture.operations.request_cancel(actor, &id(5))),
            (OperationErrorCode::ActorMismatch, None)
        );
    }
    assert_eq!(
        fixture.audit_count(),
        audits + 10,
        "cleanly rolled-back refusals commit separate audits"
    );
    assert_eq!(fixture.operation_facts(), before);
    assert_eq!(fixture.lock_facts(), locks);
    assert_eq!(fixture.events.read(0, 500).unwrap(), events);
    let terminal = fixture
        .operations
        .get(&fixture.viewer, &id(5))
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .operations
            .request_cancel(&fixture.viewer, &id(5))
            .unwrap(),
        CancelOutcome::AlreadyTerminal(terminal)
    );
    assert_eq!(
        fixture.audit_count(),
        audits + 10,
        "terminal no-op invents no mutation audit"
    );
    assert!(fixture.audit.verify_chain().unwrap().valid);
    fixture
        .db
        .read(|connection| {
            let mut query = connection.prepare(
                "SELECT action,source,outcome FROM audit_events WHERE seq>? ORDER BY seq",
            )?;
            for row in query.query_map([audits], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })? {
                assert_eq!(
                    row?,
                    (
                        "operation.cancel_request".to_owned(),
                        "operation-request".to_owned(),
                        "rejected".to_owned()
                    )
                );
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn summaries_project_only_fixed_lifecycle_facts_and_redact_free_form_headers() {
    let fixture = Fixture::open();
    fixture.db.write(|connection| {
        connection.execute("UPDATE operations SET actor_principal_id='sentinel-actor',device_id='sentinel-device',\
            source='sentinel-source',type='sentinel-type',target='sentinel-target',owner_token='sentinel-owner',\
            progress_message='sentinel-progress',result_json='sentinel-result',error_code='sentinel-error-code',\
            error_message='sentinel-error',progress_percent=73 WHERE id=?", [id(3).as_str()])?;
        Ok(())
    }).unwrap();
    let page = fixture
        .operations
        .list(&fixture.viewer, 0, MAX_PAGE)
        .unwrap();
    assert_eq!(page.items.len(), 9);
    for (index, item) in page.items.iter().enumerate() {
        assert_eq!(item.id, id(index));
        assert_eq!(item.seq, index as i64 + 1);
        assert_eq!(item.state, STATES[index]);
        assert_eq!(item.progress_percent, if index == 3 { 73 } else { 0 });
        assert_eq!(item.cancel_requested, index == 4);
        assert_eq!(item.created_at_ms, NOW);
        assert_eq!(item.updated_at_ms, NOW + 1);
        assert_eq!(
            item.finished_at_ms,
            STATES[index].is_terminal().then_some(NOW + 2)
        );
        let debug = format!("{item:?}");
        for secret in [
            "sentinel-input",
            "sentinel-actor",
            "sentinel-device",
            "sentinel-source",
            "sentinel-type",
            "sentinel-target",
            "sentinel-owner",
            "sentinel-progress",
            "sentinel-result",
            "sentinel-error",
            fixture.viewer.principal.as_str(),
            fixture.viewer.device.as_str(),
        ] {
            assert!(!debug.contains(secret), "summary leaked {secret}");
        }
        assert!(!debug.contains(&hex::encode(Sha256::digest(INPUT.as_bytes()))));
    }
    assert_eq!(
        fixture.operations.get(&fixture.viewer, &id(3)).unwrap(),
        Some(page.items[3].clone())
    );
    for state in STATES {
        assert_eq!(
            state.is_terminal(),
            matches!(
                state,
                OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled
            )
        );
    }
}
