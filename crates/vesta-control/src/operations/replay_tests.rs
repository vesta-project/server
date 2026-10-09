//! Focused real-SQLite replay tests. All lifecycle fixtures are trusted SQL facts;
//! no authorizer/registry injection, executable handler or real domain effect.
use super::*;
use crate::identity::{Capability, DeviceId, PrincipalId};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use tempfile::TempDir;
use vesta_state::{Clock, StateDatabase, StateDirectory, events::EventLog};

const INITIAL_MS: i64 = 1_791_115_200_000;
const SECRET: &str = "REPLAY-PRIVATE-MARKER";

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Fixture {
    _root: TempDir,
    db: StateDatabase,
    ops: Operations,
    subject: Subject,
    clock: Arc<TestClock>,
}
impl Fixture {
    fn open() -> Self {
        let root = tempfile::tempdir().expect("replay test root");
        let clock = Arc::new(TestClock(AtomicI64::new(INITIAL_MS)));
        let directory = StateDirectory::new(root.path().join("private replay state"), None)
            .expect("protected test directory");
        let db = StateDatabase::open(directory, clock.clone()).expect("actual SQLite state");
        let subject = seed_subject(&db, "owner");
        let ops = Operations::new(db.clone()).expect("actual operation facade");
        Self {
            _root: root,
            db,
            ops,
            subject,
            clock,
        }
    }
    fn sql(&self, work: impl FnOnce(&Connection) -> vesta_state::Result<()>) {
        self.db.write(work).expect("trusted fixture SQL");
    }
    fn operation(&self) -> OperationId {
        let id =
            OperationId::parse(&uuid::Uuid::new_v4().to_string()).expect("fixture operation id");
        self.sql(|c| {
            c.execute(
                "INSERT INTO operations (id,server_id,actor_principal_id,device_id,source,type,target,payload_hash,state,requires_confirmation,external_effect_idempotent,created_at,updated_at) \
                 VALUES (?,?,?,?,?,'legacy.private_type',?,'legacy-private-hash','queued',0,0,?,?)",
                params![id.as_str(),self.db.server_id(),self.subject.principal.as_str(),self.subject.device.as_str(),
                    SECRET,SECRET,self.clock.millis(),self.clock.millis()],
            )?;
            Ok(())
        });
        id
    }
    fn raw(&self, kind: &str, id: Option<&str>, payload: &str) -> i64 {
        self.db.write(|c| {
            c.execute(
                "INSERT INTO events (server_id,type,operation_id,at,payload_json) VALUES (?,?,?,?,?)",
                params![self.db.server_id(),kind,id,self.clock.millis(),payload],
            )?;
            Ok(c.last_insert_rowid())
        }).expect("trusted event row")
    }
    fn event(&self, id: &OperationId, kind: &str, state: OperationState, progress: u8) -> i64 {
        self.raw(kind, Some(id.as_str()), &payload(id, state, progress))
    }
    fn page(&self, limit: usize) -> ReplayPage {
        self.ops
            .replay_open(&self.subject, None, limit)
            .expect("authorized first page")
    }
    fn counts(&self) -> Vec<i64> {
        self.db
            .read(|c| {
                [
                    "operations",
                    "resource_locks",
                    "events",
                    "audit_events",
                    "principals",
                    "devices",
                    "role_assignments",
                    "confirmations",
                ]
                .iter()
                .map(|table| {
                    Ok(
                        c.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                            row.get(0)
                        })?,
                    )
                })
                .collect()
            })
            .expect("read-only row counts")
    }
    fn role(&self, role: &str) {
        self.sql(|c| {
            c.execute(
                "UPDATE role_assignments SET role_id=? WHERE principal_id=?",
                params![role, self.subject.principal.as_str()],
            )?;
            Ok(())
        });
    }
    fn deny_view(&self) {
        self.sql(|c| {
            c.execute(
                "INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?,?,'','deny','fixture',?)",
                params![uuid::Uuid::new_v4().to_string(),self.subject.principal.as_str(),Capability::TasksView.id(),self.clock.millis()],
            )?;
            Ok(())
        });
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let closed = self.db.close();
        if !std::thread::panicking() {
            closed.expect("close replay fixture");
        }
    }
}
fn seed_subject(db: &StateDatabase, role: &str) -> Subject {
    let principal = PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).expect("principal");
    let device = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).expect("device");
    db.write(|c| {
        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Replay fixture','active',?,?)",
            params![principal.as_str(),db.server_id(),db.clock().millis(),db.clock().millis()])?;
        c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Replay fixture',?,?)",
            params![device.as_str(),db.server_id(),principal.as_str(),hex::encode(Sha256::digest(device.as_str().as_bytes())),db.clock().millis()])?;
        c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,?,'fixture',?)",
            params![uuid::Uuid::new_v4().to_string(),principal.as_str(),role,db.clock().millis()])?;
        Ok(())
    }).expect("trusted identity facts");
    Subject { principal, device }
}
fn second_device(f: &Fixture) -> Subject {
    let device = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).expect("second device");
    f.sql(|c| {
        c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Second replay device',?,?)",
            params![device.as_str(),f.db.server_id(),f.subject.principal.as_str(),hex::encode(Sha256::digest(device.as_str().as_bytes())),f.clock.millis()])?;
        Ok(())
    });
    Subject {
        principal: f.subject.principal.clone(),
        device,
    }
}
fn payload(id: &OperationId, state: OperationState, progress: u8) -> String {
    serde_json::json!({"operationId":id.as_str(),"state":state.as_str(),"progress":progress})
        .to_string()
}
fn cursor(page: &ReplayPage) -> &ReplayCursor {
    page.next_cursor.as_ref().expect("ordinary continuation")
}
fn ids(page: &ReplayPage) -> Vec<i64> {
    page.events.iter().map(|event| event.event_cursor).collect()
}
fn code<T: fmt::Debug>(result: OperationsResult<T>, expected: &str) -> OperationError {
    let error = result.expect_err("replay must refuse");
    assert_eq!(error.code(), expected);
    assert!(!format!("{error:?} {error}").contains(SECRET));
    error
}

#[test]
fn replay_returns_historical_fixed_lifecycle_without_stored_or_actor_secrets() {
    let f = Fixture::open();
    let id = f.operation();
    f.event(&id, "operation.created", OperationState::Queued, 0);
    f.event(&id, "operation.state", OperationState::Running, 0);
    f.event(&id, "operation.progress", OperationState::Running, 40);
    f.event(&id, "operation.state", OperationState::Succeeded, 100);
    f.sql(|c| {
        c.execute("UPDATE operations SET state='succeeded',finished_at=?,progress_percent=100,progress_message=?,error_message=?,result_json=?,input_json=? WHERE id=?",
            params![f.clock.millis(),SECRET,SECRET,SECRET,SECRET,id.as_str()])?;
        Ok(())
    });
    let before = f.counts();
    let page = f.page(100);
    assert!(page.complete);
    assert_eq!(page.status, ReplayStatus::Ok);
    assert_eq!(ids(&page), [1, 2, 3, 4]);
    assert_eq!(
        page.events
            .iter()
            .map(|event| event.state)
            .collect::<Vec<_>>(),
        [
            OperationState::Queued,
            OperationState::Running,
            OperationState::Running,
            OperationState::Succeeded
        ]
    );
    assert_eq!(
        page.events
            .iter()
            .map(|event| event.progress_percent)
            .collect::<Vec<_>>(),
        [0, 0, 40, 100]
    );
    assert!(page.events.iter().all(|event| event.operation_id == id));
    assert_eq!(page.events[2].kind, ReplayEventKind::Progress);
    let rendered = format!("{page:?}");
    for private in [
        SECRET,
        "legacy.private_type",
        "legacy-private-hash",
        f.subject.principal.as_str(),
        f.subject.device.as_str(),
        f.db.server_id(),
    ] {
        assert!(!rendered.contains(private), "must redact {private}");
    }
    assert_eq!(before, f.counts());
}

#[test]
fn replay_requires_tasks_view_not_action_authority_binding_or_initiating_actor_status() {
    let f = Fixture::open();
    let id = f.operation();
    f.event(&id, "operation.created", OperationState::Queued, 0);
    let observer = seed_subject(&f.db, "viewer");
    f.sql(|c| {
        c.execute(
            "UPDATE principals SET status='disabled' WHERE id=?",
            [f.subject.principal.as_str()],
        )?;
        c.execute(
            "UPDATE operations SET input_json=NULL,input_status='purged' WHERE id=?",
            [id.as_str()],
        )?;
        Ok(())
    });
    let page = f
        .ops
        .replay_open(&observer, None, 10)
        .expect("viewer fixed lifecycle access");
    assert_eq!(ids(&page), [1]);
    assert_eq!(page.events[0].operation_id, id);
    let checkpoint = f
        .ops
        .replay_checkpoint(&observer)
        .expect("viewer checkpoint");
    assert!(
        f.ops
            .replay_page(&observer, &checkpoint, 10)
            .expect("empty viewer page")
            .complete
    );
}

#[test]
fn empty_complete_windows_checkpoints_and_reopening_are_read_only() {
    let f = Fixture::open();
    let before = f.counts();
    let empty = f.page(10);
    assert!(empty.complete && empty.events.is_empty());
    assert_eq!(empty.pruned_through, 0);
    let checkpoint = f
        .ops
        .replay_checkpoint(&f.subject)
        .expect("empty checkpoint");
    assert_eq!(cursor(&empty), &checkpoint);
    assert!(
        f.ops
            .replay_page(&f.subject, &checkpoint, 10)
            .expect("empty continuation")
            .complete
    );
    assert_eq!(before, f.counts());
    let id = f.operation();
    f.event(&id, "operation.created", OperationState::Queued, 0);
    assert!(
        f.ops
            .replay_page(&f.subject, &checkpoint, 10)
            .expect("fixed empty horizon")
            .events
            .is_empty()
    );
    assert_eq!(
        ids(&f
            .ops
            .replay_open(&f.subject, Some(&checkpoint), 10)
            .expect("new horizon")),
        [1]
    );
}

#[test]
fn output_limit_resumes_without_skips_duplicates_or_chasing_new_appends() {
    let f = Fixture::open();
    let id = f.operation();
    for n in 0..5 {
        f.event(&id, "operation.progress", OperationState::Running, n);
    }
    let one = f.page(2);
    assert_eq!(ids(&one), [1, 2]);
    assert!(!one.complete);
    f.event(&id, "operation.progress", OperationState::Running, 5);
    let two = f
        .ops
        .replay_page(&f.subject, cursor(&one), 2)
        .expect("second page");
    assert_eq!(ids(&two), [3, 4]);
    let three = f
        .ops
        .replay_page(&f.subject, cursor(&two), 2)
        .expect("third page");
    assert_eq!(ids(&three), [5]);
    assert!(three.complete);
    let again = f
        .ops
        .replay_page(&f.subject, cursor(&three), 2)
        .expect("repeat terminal cursor");
    assert!(again.events.is_empty() && again.complete);
    let next = f
        .ops
        .replay_open(&f.subject, Some(cursor(&three)), 2)
        .expect("new window");
    assert_eq!(ids(&next), [6]);
}

#[test]
fn unknown_huge_payloads_missing_tasks_and_unknown_types_advance_after_output_limit() {
    let f = Fixture::open();
    let id = f.operation();
    let missing = OperationId::parse("missing-task").expect("missing id");
    f.event(&id, "operation.created", OperationState::Queued, 0);
    f.raw("settings.changed", None, &"x".repeat(2_000_000));
    f.raw("operation.custom", Some(id.as_str()), SECRET);
    f.raw("operation.state", Some(missing.as_str()), SECRET);
    f.event(&id, "operation.progress", OperationState::Running, 1);
    let one = f.page(1);
    assert_eq!(ids(&one), [1]);
    assert_eq!(cursor(&one).position, 4);
    let two = f
        .ops
        .replay_page(&f.subject, cursor(&one), 1)
        .expect("tail page");
    assert_eq!(ids(&two), [5]);
    assert!(two.complete);
    assert!(!format!("{one:?} {two:?}").contains(SECRET));
}

#[test]
fn independent_scan_budget_advances_on_unrelated_and_unavailable_rows() {
    let f = Fixture::open();
    f.sql(|c| {
        let mut statement = c.prepare("INSERT INTO events (server_id,type,operation_id,at,payload_json) VALUES (?,'operation.state','missing',0,?)")?;
        for _ in 0..520 { statement.execute(params![f.db.server_id(),SECRET])?; }
        Ok(())
    });
    let id = f.operation();
    f.event(&id, "operation.state", OperationState::Running, 9);
    let one = f.page(1);
    assert!(one.events.is_empty() && !one.complete);
    assert_eq!(cursor(&one).position, MAX_REPLAY_SCAN as i64);
    let two = f
        .ops
        .replay_page(&f.subject, cursor(&one), 1)
        .expect("scan continuation");
    assert_eq!(ids(&two), [521]);
    assert!(two.complete);
}

#[test]
fn exact_scan_budget_and_journal_holes_finish_without_repeat_or_stall() {
    let f = Fixture::open();
    f.sql(|c| {
        let mut statement = c.prepare(
            "INSERT INTO events (server_id,type,at,payload_json) VALUES (?,'unrelated',0,'{}')",
        )?;
        for _ in 0..MAX_REPLAY_SCAN {
            statement.execute([f.db.server_id()])?;
        }
        Ok(())
    });
    let page = f.page(1);
    assert!(page.complete && page.events.is_empty());
    let id = f.operation();
    let first = f.event(&id, "operation.progress", OperationState::Running, 1);
    let hole = f.event(&id, "operation.progress", OperationState::Running, 2);
    let last = f.event(&id, "operation.progress", OperationState::Running, 3);
    f.sql(|c| {
        c.execute("DELETE FROM events WHERE cursor=?", [hole])?;
        Ok(())
    });
    let tail = f
        .ops
        .replay_open(&f.subject, Some(cursor(&page)), 10)
        .expect("holes window");
    assert_eq!(ids(&tail), [first, last]);
    assert!(tail.complete);
}

#[test]
fn clock_rollback_never_changes_journal_order() {
    let f = Fixture::open();
    let id = f.operation();
    f.event(&id, "operation.created", OperationState::Queued, 0);
    f.clock.0.store(INITIAL_MS - 10_000, Ordering::SeqCst);
    f.event(&id, "operation.state", OperationState::Running, 1);
    let page = f.page(10);
    assert_eq!(ids(&page), [1, 2]);
    assert!(page.events[1].at_ms < page.events[0].at_ms);
}

#[test]
fn pruning_mid_window_requires_explicit_resync_without_silent_skip() {
    let f = Fixture::open();
    let id = f.operation();
    f.event(&id, "operation.created", OperationState::Queued, 0);
    f.event(&id, "operation.state", OperationState::Running, 0);
    f.clock.0.store(INITIAL_MS + 10_000, Ordering::SeqCst);
    f.event(&id, "operation.progress", OperationState::Running, 1);
    let one = f.page(1);
    assert_eq!(
        EventLog::new(f.db.clone())
            .prune_older_than(INITIAL_MS + 5_000)
            .expect("prune prefix"),
        2
    );
    let stale = f
        .ops
        .replay_page(&f.subject, cursor(&one), 10)
        .expect("explicit history gap");
    assert_eq!(stale.status, ReplayStatus::ResyncRequired);
    assert!(stale.events.is_empty() && stale.next_cursor.is_none() && !stale.complete);
    assert_eq!(stale.pruned_through, 2);
    assert_eq!(f.page(10).status, ReplayStatus::ResyncRequired);
    // At the exact retained floor there is no gap, not an off-by-one resync.
    let at_floor = f.ops.replay_cursor(&f.subject, 2, 3);
    let rest = f
        .ops
        .replay_page(&f.subject, &at_floor, 10)
        .expect("floor boundary");
    assert_eq!(ids(&rest), [3]);
}

#[test]
fn full_prune_keeps_watermark_and_head_new_events_resume_after_checkpoint() {
    let f = Fixture::open();
    let id = f.operation();
    f.event(&id, "operation.created", OperationState::Queued, 0);
    f.event(&id, "operation.state", OperationState::Running, 0);
    let one = f.page(1);
    assert_eq!(
        EventLog::new(f.db.clone())
            .prune_older_than(INITIAL_MS + 1)
            .expect("prune all"),
        2
    );
    let stale = f
        .ops
        .replay_page(&f.subject, cursor(&one), 10)
        .expect("gap after full prune");
    assert_eq!(stale.status, ReplayStatus::ResyncRequired);
    let head = f
        .ops
        .replay_checkpoint(&f.subject)
        .expect("watermarked head");
    assert_eq!(head.position, 2);
    assert!(
        f.ops
            .replay_open(&f.subject, Some(&head), 10)
            .expect("empty retained journal")
            .complete
    );
    f.event(&id, "operation.progress", OperationState::Running, 20);
    assert_eq!(
        ids(&f
            .ops
            .replay_open(&f.subject, Some(&head), 10)
            .expect("post prune append")),
        [3]
    );
}

#[test]
fn cursor_is_exact_subject_device_and_server_bound_but_not_authority() {
    let f = Fixture::open();
    let id = f.operation();
    f.event(&id, "operation.created", OperationState::Queued, 0);
    let cursor = f.ops.replay_checkpoint(&f.subject).expect("bound cursor");
    let other = seed_subject(&f.db, "owner");
    let device = second_device(&f);
    for subject in [&other, &device] {
        code(
            f.ops.replay_page(subject, &cursor, 10),
            "state_operation_cursor_invalid",
        );
        code(
            f.ops.replay_open(subject, Some(&cursor), 10),
            "state_operation_cursor_invalid",
        );
    }
    let mut foreign = cursor.clone();
    foreign.server = uuid::Uuid::new_v4().to_string();
    code(
        f.ops.replay_page(&f.subject, &foreign, 10),
        "state_operation_cursor_invalid",
    );
    let rendered = format!("{cursor:?}");
    assert!(!rendered.contains(f.subject.principal.as_str()));
    assert!(!rendered.contains(f.subject.device.as_str()));
    assert!(!rendered.contains(f.db.server_id()));
    let before = f.counts();
    f.role("moderator");
    code(f.ops.replay_page(&f.subject, &foreign, 10), "forbidden");
    code(
        f.ops.replay_open(&f.subject, Some(&foreign), 10),
        "forbidden",
    );
    code(f.ops.replay_checkpoint(&f.subject), "forbidden");
    assert_eq!(before, f.counts());
}

#[test]
fn each_call_rechecks_current_policy_even_for_complete_cursor_or_invalid_cursor() {
    for change in [
        "deny",
        "disabled",
        "revoked-device",
        "expired-role",
        "revoked-role",
        "downgraded",
    ] {
        let f = Fixture::open();
        let id = f.operation();
        f.event(&id, "operation.created", OperationState::Queued, 0);
        let head = f
            .ops
            .replay_checkpoint(&f.subject)
            .expect("authorized cursor");
        match change {
            "deny" => f.deny_view(),
            "downgraded" => f.role("moderator"),
            _ => f.sql(|c| {
                match change {
                    "disabled" => {
                        c.execute("UPDATE principals SET status='disabled'", [])?;
                    }
                    "revoked-device" => {
                        c.execute("UPDATE devices SET revoked_at=?", [f.clock.millis()])?;
                    }
                    "expired-role" => {
                        c.execute(
                            "UPDATE role_assignments SET expires_at=?",
                            [f.clock.millis()],
                        )?;
                    }
                    "revoked-role" => {
                        c.execute(
                            "UPDATE role_assignments SET revoked_at=?",
                            [f.clock.millis()],
                        )?;
                    }
                    _ => unreachable!(),
                }
                Ok(())
            }),
        }
        let before = f.counts();
        code(f.ops.replay_open(&f.subject, None, 10), "forbidden");
        code(f.ops.replay_page(&f.subject, &head, 10), "forbidden");
        code(f.ops.replay_checkpoint(&f.subject), "forbidden");
        let mut invalid = head.clone();
        invalid.position = i64::MAX;
        code(f.ops.replay_page(&f.subject, &invalid, 10), "forbidden");
        assert_eq!(before, f.counts(), "read-only refusal: {change}");
    }
}

#[test]
fn permission_restoration_continues_original_cursor_without_consuming_rows_on_refusal() {
    let f = Fixture::open();
    let id = f.operation();
    for n in 0..3 {
        f.event(&id, "operation.progress", OperationState::Running, n);
    }
    let one = f.page(1);
    f.role("moderator");
    code(f.ops.replay_page(&f.subject, cursor(&one), 1), "forbidden");
    f.role("viewer");
    let two = f
        .ops
        .replay_page(&f.subject, cursor(&one), 1)
        .expect("restored observer");
    assert_eq!(ids(&two), [2]);
}

#[test]
fn invalid_limits_are_bounded_before_database_read() {
    let f = Fixture::open();
    let head = f.ops.replay_checkpoint(&f.subject).expect("checkpoint");
    f.db.close().expect("close database");
    for limit in [0, MAX_REPLAY_RETURN + 1, usize::MAX] {
        code(
            f.ops.replay_open(&f.subject, None, limit),
            "invalid_request",
        );
        code(
            f.ops.replay_page(&f.subject, &head, limit),
            "invalid_request",
        );
    }
    code(f.ops.replay_open(&f.subject, None, 1), "state_closed");
    code(f.ops.replay_checkpoint(&f.subject), "state_closed");
}

#[test]
fn internal_invalid_or_lost_tail_cursor_fails_instead_of_silently_restarting() {
    let f = Fixture::open();
    let id = f.operation();
    for n in 0..3 {
        f.event(&id, "operation.progress", OperationState::Running, n);
    }
    let head = f.ops.replay_checkpoint(&f.subject).expect("checkpoint");
    for (position, through) in [(-1, 3), (0, -1), (4, 4), (3, 2), (0, 4)] {
        let forged = f.ops.replay_cursor(&f.subject, position, through);
        code(
            f.ops.replay_page(&f.subject, &forged, 10),
            "state_operation_cursor_invalid",
        );
    }
    f.sql(|c| {
        c.execute("DELETE FROM events WHERE cursor=3", [])?;
        Ok(())
    });
    code(
        f.ops.replay_page(&f.subject, &head, 10),
        "state_operation_cursor_invalid",
    );
    code(
        f.ops.replay_open(&f.subject, Some(&head), 10),
        "state_operation_cursor_invalid",
    );
}

#[test]
fn malformed_recognized_existing_task_payload_fails_whole_page_and_redacts_details() {
    let templates = [
        "{}",
        "[]",
        "null",
        "\"REPLAY-PRIVATE-MARKER\"",
        r#"{"operationId":"%ID%","state":"running"}"#,
        r#"{"operationId":"%ID%","state":"running","progress":1,"secret":"REPLAY-PRIVATE-MARKER"}"#,
        r#"{"operationId":"%ID%","state":"running","state":"failed","progress":1}"#,
        r#"{"operationId":"%ID%","operationId":"%ID%","state":"running","progress":1}"#,
        r#"{"operationId":"%ID%","state":"running","progress":1,"progress":2}"#,
        r#"{"operationId":"%ID%","state":"running","progress":1} {}"#,
        r#"{"operationId":"%ID%","state":"running","progress":1}x"#,
        r#"{"operationId":"%ID%","state":"running","progress":50.5}"#,
        r#"{"operationId":"%ID%","state":"running","progress":1.0}"#,
        r#"{"operationId":"%ID%","state":"running","progress":1e1}"#,
        r#"{"operationId":"%ID%","state":"running","progress":-1}"#,
        r#"{"operationId":"%ID%","state":"running","progress":101}"#,
        r#"{"operationId":"%ID%","state":"running","progress":"50"}"#,
        r#"{"operationId":"%ID%","state":"running","progress":050}"#,
        r#"{"operationId":"%ID%","state":"running","progress":true}"#,
        r#"{"operationId":"%ID%","state":"REPLAY-PRIVATE-MARKER","progress":1}"#,
        r#"{"operationId":"%ID%","state":5,"progress":1}"#,
        r#"{"operationId":null,"state":"running","progress":1}"#,
        r#"{"operationId":"other-id","state":"running","progress":1}"#,
    ];
    for template in templates {
        let f = Fixture::open();
        let id = f.operation();
        f.event(&id, "operation.created", OperationState::Queued, 0);
        f.raw(
            "operation.state",
            Some(id.as_str()),
            &template.replace("%ID%", id.as_str()),
        );
        f.event(&id, "operation.progress", OperationState::Running, 2);
        let before = f.counts();
        code(
            f.ops.replay_open(&f.subject, None, 10),
            "state_operation_event_malformed",
        );
        assert_eq!(before, f.counts());
        let zero = f.ops.replay_cursor(&f.subject, 0, 3);
        code(
            f.ops.replay_page(&f.subject, &zero, 10),
            "state_operation_event_malformed",
        );
        // Already returned valid events need not fail because of a later page's
        // malformed payload, but it must not be skipped or silently consumed.
        let first = f.page(1);
        assert_eq!(ids(&first), [1]);
        code(
            f.ops.replay_page(&f.subject, cursor(&first), 1),
            "state_operation_event_malformed",
        );
    }
}

#[test]
fn recognized_header_oversize_payload_and_nul_faults_are_fixed_redacted_failures() {
    for fault in [
        "missing-id",
        "bad-id",
        "oversize-id",
        "oversize-payload",
        "nul-payload",
    ] {
        let f = Fixture::open();
        let id = f.operation();
        let good = payload(&id, OperationState::Running, 1);
        match fault {
            "missing-id" => {
                f.raw("operation.state", None, &good);
            }
            "bad-id" => {
                f.raw("operation.state", Some("bad\nidentifier"), &good);
            }
            "oversize-id" => {
                f.raw("operation.state", Some(&"x".repeat(1000)), &good);
            }
            "oversize-payload" => {
                f.raw("operation.state", Some(id.as_str()), &"x".repeat(100_000));
            }
            "nul-payload" => {
                f.raw(
                    "operation.state",
                    Some(id.as_str()),
                    &format!("{good}\0{SECRET}"),
                );
            }
            _ => unreachable!(),
        }
        code(
            f.ops.replay_open(&f.subject, None, 10),
            "state_operation_event_malformed",
        );
    }
}

#[test]
fn maximum_page_rendering_is_bounded_and_all_fixed_lifecycle_states_validate() {
    let f = Fixture::open();
    let id = f.operation();
    let states = [
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
    for n in 0..MAX_REPLAY_RETURN {
        f.event(&id, "operation.state", states[n % states.len()], 100);
    }
    let page = f.page(MAX_REPLAY_RETURN);
    assert_eq!(page.events.len(), MAX_REPLAY_RETURN);
    assert!(page.complete);
    assert!(format!("{page:?}").len() < 64 * 1024);
    for kind in [
        ReplayEventKind::Created,
        ReplayEventKind::State,
        ReplayEventKind::Progress,
    ] {
        assert_eq!(ReplayEventKind::stored(kind.as_str()), Some(kind));
    }
}

#[test]
fn corrupted_watermark_sql_faults_and_corrupt_summary_fail_without_stored_text() {
    let f = Fixture::open();
    let id = f.operation();
    f.event(&id, "operation.created", OperationState::Queued, 0);
    f.sql(|c| {
        c.execute(
            "UPDATE meta SET value=? WHERE key='events_pruned_through'",
            [SECRET],
        )?;
        Ok(())
    });
    code(
        f.ops.replay_open(&f.subject, None, 10),
        "state_integrity_failed",
    );
    f.sql(|c| {
        c.execute(
            "UPDATE meta SET value='-1' WHERE key='events_pruned_through'",
            [],
        )?;
        Ok(())
    });
    code(
        f.ops.replay_checkpoint(&f.subject),
        "state_integrity_failed",
    );
    f.sql(|c| {
        c.execute(
            "UPDATE meta SET value='0' WHERE key='events_pruned_through'",
            [],
        )?;
        c.execute_batch("PRAGMA ignore_check_constraints=ON")?;
        c.execute(
            "UPDATE operations SET progress_percent=101 WHERE id=?",
            [id.as_str()],
        )?;
        Ok(())
    });
    code(
        f.ops.replay_open(&f.subject, None, 10),
        "state_operation_corrupt",
    );
    f.sql(|c| {
        c.execute("ALTER TABLE events RENAME TO replay_private_marker", [])?;
        Ok(())
    });
    code(f.ops.replay_open(&f.subject, None, 10), "state_io");
}

#[test]
fn policy_rows_head_and_projection_share_the_admitted_snapshot_under_writer_race() {
    let f = Fixture::open();
    let id = f.operation();
    f.event(&id, "operation.created", OperationState::Queued, 0);
    let writer_db = f.db.clone();
    let writer_subject = f.subject.clone();
    let writer_id = id.clone();
    // read_with establishes SQLite's snapshot before this callback. A writer
    // completes while it is held, changing policy, task state and journal head.
    let old =
        f.db.read_with(|snapshot| {
            std::thread::spawn(move || {
                writer_db
                    .write(|c| {
                        c.execute(
                            "UPDATE role_assignments SET role_id='moderator' WHERE principal_id=?",
                            [writer_subject.principal.as_str()],
                        )?;
                        c.execute(
                            "UPDATE operations SET state='running' WHERE id=?",
                            [writer_id.as_str()],
                        )?;
                        EventLog::new(writer_db.clone()).append(
                            c,
                            "operation.state",
                            Some(writer_id.as_str()),
                            Some(&payload(&writer_id, OperationState::Running, 1)),
                        )?;
                        Ok(())
                    })
                    .expect("racing transaction");
            })
            .join()
            .expect("writer joins");
            let repo = Repository::new(snapshot.connection(), f.db.server_id(), f.clock.millis());
            f.ops.replay_in(&repo, &f.subject, None, true, 10)
        })
        .expect("old admitted snapshot");
    assert_eq!(ids(&old), [1]);
    assert_eq!(cursor(&old).through, 1);
    assert_eq!(old.events[0].state, OperationState::Queued);
    code(f.ops.replay_page(&f.subject, cursor(&old), 10), "forbidden");
    f.role("viewer");
    assert!(
        f.ops
            .replay_page(&f.subject, cursor(&old), 10)
            .expect("old fixed horizon")
            .events
            .is_empty()
    );
    assert_eq!(
        ids(&f
            .ops
            .replay_open(&f.subject, Some(cursor(&old)), 10)
            .expect("new current horizon")),
        [2]
    );
}

#[test]
fn unknown_or_mismatched_subjects_and_pack_access_never_gain_replay_authority() {
    let f = Fixture::open();
    let head = f
        .ops
        .replay_checkpoint(&f.subject)
        .expect("authorized head");
    let unknown_principal =
        PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).expect("unknown principal");
    let unknown_device =
        DeviceId::parse(&uuid::Uuid::new_v4().to_string()).expect("unknown device");
    let subjects = [
        Subject {
            principal: unknown_principal.clone(),
            device: unknown_device.clone(),
        },
        Subject {
            principal: unknown_principal,
            device: f.subject.device.clone(),
        },
        Subject {
            principal: f.subject.principal.clone(),
            device: unknown_device,
        },
        seed_subject(&f.db, "pack-user"),
    ];
    let before = f.counts();
    for subject in &subjects {
        code(f.ops.replay_open(subject, None, 10), "forbidden");
        code(f.ops.replay_page(subject, &head, 10), "forbidden");
        code(f.ops.replay_checkpoint(subject), "forbidden");
    }
    assert_eq!(before, f.counts());
}

#[test]
fn bounded_concurrent_appends_and_reopening_preserve_every_journal_position() {
    let f = Fixture::open();
    let id = f.operation();
    let writer_db = f.db.clone();
    let writer = std::thread::spawn(move || {
        for n in 0..40 {
            writer_db
                .write(|c| {
                    EventLog::new(writer_db.clone()).append(
                        c,
                        "operation.progress",
                        Some(id.as_str()),
                        Some(&payload(&id, OperationState::Running, n)),
                    )?;
                    Ok(())
                })
                .expect("bounded writer append");
        }
    });
    let mut checkpoint = None;
    let mut seen = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while seen.len() < 40 && std::time::Instant::now() < deadline {
        let mut page = f
            .ops
            .replay_open(&f.subject, checkpoint.as_ref(), 3)
            .expect("race window");
        loop {
            seen.extend(ids(&page));
            checkpoint = page.next_cursor.clone();
            if page.complete {
                break;
            }
            page = f
                .ops
                .replay_page(&f.subject, cursor(&page), 3)
                .expect("race continuation");
        }
        std::thread::yield_now();
    }
    writer.join().expect("writer finishes");
    assert_eq!(seen, (1..=40).collect::<Vec<_>>());
}
