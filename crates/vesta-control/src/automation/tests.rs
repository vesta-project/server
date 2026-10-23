use super::*;
use crate::identity::{DeviceId, PrincipalId, Subject};
use rusqlite::params;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
};
use tempfile::TempDir;
use vesta_state::{Clock, StateDatabase, StateDirectory};

const NOW: i64 = 1_791_115_200_000;
struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct FakePort {
    forbidden: AtomicBool,
    authorization_error: Mutex<Option<ActionError>>,
    begins: Mutex<Vec<String>>,
    handles: Mutex<BTreeMap<String, ActionHandle>>,
    polls: Mutex<Vec<String>>,
    status: Mutex<ActionStatus>,
    database: Mutex<Option<StateDatabase>>,
}
impl Default for FakePort {
    fn default() -> Self {
        Self {
            forbidden: AtomicBool::new(false),
            authorization_error: Mutex::new(None),
            begins: Mutex::new(Vec::new()),
            handles: Mutex::new(BTreeMap::new()),
            polls: Mutex::new(Vec::new()),
            status: Mutex::new(ActionStatus::Running),
            database: Mutex::new(None),
        }
    }
}
impl FakePort {
    fn access_database(&self) {
        if let Some(db) = self.database.lock().unwrap().as_ref() {
            db.write(|c| {
                c.execute(
                    "UPDATE workflow_schedule SET last_occurrence=last_occurrence",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        }
    }
}
impl ActionPort for FakePort {
    fn authorize(&self, _: &PrincipalId, _: &Action) -> std::result::Result<(), ActionError> {
        self.access_database();
        if self.forbidden.load(Ordering::SeqCst) {
            Err(ActionError::Forbidden)
        } else if let Some(error) = *self.authorization_error.lock().unwrap() {
            Err(error)
        } else {
            Ok(())
        }
    }
    fn begin(
        &self,
        _: &PrincipalId,
        _: &Action,
        key: &str,
    ) -> std::result::Result<ActionHandle, ActionError> {
        self.access_database();
        self.begins.lock().unwrap().push(key.into());
        Ok(self
            .handles
            .lock()
            .unwrap()
            .entry(key.into())
            .or_insert_with(|| ActionHandle(uuid::Uuid::new_v4().to_string()))
            .clone())
    }
    fn poll(&self, handle: &ActionHandle) -> std::result::Result<ActionStatus, ActionError> {
        self.access_database();
        self.polls.lock().unwrap().push(handle.0.clone());
        Ok(self.status.lock().unwrap().clone())
    }
}
struct Fixture {
    root: TempDir,
    clock: Arc<TestClock>,
    db: StateDatabase,
    port: Arc<FakePort>,
    automation: Automation,
    owner: Subject,
    moderator: Subject,
}
impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
        let db = StateDatabase::open(
            StateDirectory::new(root.path().join("state"), None).unwrap(),
            clock.clone(),
        )
        .unwrap();
        let owner = seed(&db, "owner-user", "owner", 'a');
        let moderator = seed(&db, "moderator-user", "moderator", 'b');
        let port = Arc::new(FakePort::default());
        let automation = Automation::new(db.clone(), port.clone()).unwrap();
        Self {
            root,
            clock,
            db,
            port,
            automation,
            owner,
            moderator,
        }
    }
    fn pulse(&self, delta: i64) {
        self.clock.0.store(NOW + delta, Ordering::SeqCst);
        self.automation.tick(NOW + delta).unwrap();
    }
    fn create(&self, definition: WorkflowDefinition) -> Workflow {
        self.automation
            .create(&self.owner, "Test", &self.owner.principal, definition, true)
            .unwrap()
    }
    fn manual(&self, w: &Workflow) -> String {
        self.automation
            .run_manual(&self.owner, &w.id)
            .unwrap()
            .unwrap()
    }
    fn run(&self, id: &str) -> Run {
        self.automation.get_run(&self.owner, id).unwrap()
    }
}
fn seed(db: &StateDatabase, id: &str, role: &str, hash: char) -> Subject {
    db.write(|c| {
        c.execute("INSERT INTO principals(id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,?,'active',?,?)",params![id,db.server_id(),id,NOW,NOW])?;
        c.execute("INSERT INTO devices(id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,?,?,?)",params![format!("{id}-device"),db.server_id(),id,id,hash.to_string().repeat(64),NOW])?;
        c.execute("INSERT INTO role_assignments(id,principal_id,role_id,granted_by,created_at) VALUES (?,?,?,'test',?)",params![format!("{id}-role"),id,role,NOW])?;Ok(())
    }).unwrap();
    Subject {
        principal: PrincipalId::parse(id).unwrap(),
        device: DeviceId::parse(&format!("{id}-device")).unwrap(),
    }
}
fn step(id: &str, action: Action) -> Step {
    Step {
        id: id.into(),
        action,
        timeout_secs: 120,
        retries: RetryPolicy::default(),
        on_success: SuccessBranch::Next,
        on_failure: FailureBranch::Fail,
    }
}
fn definition(trigger: Trigger, steps: Vec<Step>) -> WorkflowDefinition {
    WorkflowDefinition {
        trigger,
        missed_policy: MissedPolicy::SkipAndNotify,
        steps,
    }
}
fn notice() -> Action {
    Action::Notify {
        severity: Severity::Info,
        title: "Done".into(),
        body: "Workflow finished".into(),
    }
}
#[test]
fn manual_delay_notification_is_durable_and_duplicate_is_skipped() {
    let f = Fixture::new();
    let w = f.create(definition(
        Trigger::Manual,
        vec![
            step("delay", Action::Delay { secs: 60 }),
            step("notify", notice()),
        ],
    ));
    let id = f.manual(&w);
    assert!(f.automation.run_manual(&f.owner, &w.id).unwrap().is_none());
    f.pulse(0);
    assert_eq!(f.run(&id).next_wake_at, Some(NOW + 60000));
    f.pulse(59999);
    assert_eq!(f.run(&id).current_step, 0);
    f.pulse(60000);
    assert_eq!(f.run(&id).current_step, 1);
    f.pulse(60001);
    assert_eq!(f.run(&id).state, "succeeded");
    assert_eq!(f.automation.unread_count(&f.owner).unwrap(), 1);
    assert!(f.port.begins.lock().unwrap().is_empty());
    assert_eq!(
        f.automation
            .list_steps(&f.owner, &id, None, 100)
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn explicit_failure_retries_with_exponential_backoff_and_new_key() {
    let f = Fixture::new();
    let mut s = step(
        "backup",
        Action::Backup {
            profile: "daily".into(),
        },
    );
    s.retries = RetryPolicy {
        max_retries: 2,
        backoff_secs: 5,
    };
    let w = f.create(definition(Trigger::Manual, vec![s]));
    let id = f.manual(&w);
    f.pulse(0);
    f.pulse(1);
    *f.port.status.lock().unwrap() = ActionStatus::Failed {
        code: "backup_failed".into(),
    };
    f.pulse(2);
    assert_eq!(f.run(&id).attempt, 1);
    assert_eq!(f.run(&id).next_wake_at, Some(NOW + 5002));
    f.pulse(5001);
    assert_eq!(f.port.begins.lock().unwrap().len(), 1);
    f.pulse(5002);
    f.pulse(5003);
    f.pulse(5004);
    assert_eq!(f.run(&id).attempt, 2);
    assert_eq!(f.run(&id).next_wake_at, Some(NOW + 15004));
    f.pulse(15004);
    f.pulse(15005);
    *f.port.status.lock().unwrap() = ActionStatus::Succeeded {
        output: serde_json::json!({"verified":true}),
    };
    f.pulse(15006);
    assert_eq!(f.run(&id).state, "succeeded");
    let keys = f.port.begins.lock().unwrap();
    assert_eq!(keys.len(), 3);
    assert_ne!(keys[0], keys[1]);
}
#[test]
fn timeout_and_uncertain_never_retry() {
    for uncertain in [false, true] {
        let f = Fixture::new();
        let mut s = step("start", Action::Start);
        s.timeout_secs = 10;
        s.retries = RetryPolicy {
            max_retries: 5,
            backoff_secs: 1,
        };
        let w = f.create(definition(Trigger::Manual, vec![s]));
        let id = f.manual(&w);
        f.pulse(0);
        f.pulse(1);
        if uncertain {
            *f.port.status.lock().unwrap() = ActionStatus::Uncertain {
                code: "unknown".into(),
            };
            f.pulse(2);
        } else {
            f.pulse(10000);
        }
        assert_eq!(f.run(&id).state, "failed");
        assert_eq!(f.run(&id).attempt, 0);
        assert_eq!(f.port.begins.lock().unwrap().len(), 1);
        assert_eq!(
            f.run(&id).outcome_code.as_deref(),
            Some(if uncertain { "uncertain" } else { "timed_out" })
        );
    }
}
#[test]
fn revoked_authority_suspends_submitted_operation_and_notifies_once() {
    let f = Fixture::new();
    let w = f.create(definition(
        Trigger::Manual,
        vec![step("start", Action::Start), step("stop", Action::Stop)],
    ));
    let id = f.manual(&w);
    f.pulse(0);
    f.pulse(1);
    f.port.forbidden.store(true, Ordering::SeqCst);
    f.pulse(2);
    f.pulse(3);
    assert_eq!(f.run(&id).state, "suspended");
    assert_eq!(
        f.run(&id).outcome_code.as_deref(),
        Some("authority_revoked")
    );
    assert!(f.port.polls.lock().unwrap().is_empty());
    let notifications = f.automation.list_since(&f.owner, 0, 10).unwrap();
    assert_eq!(notifications.notifications.len(), 1);
    assert_eq!(notifications.notifications[0].severity, Severity::Critical);
    assert!(f.automation.run_manual(&f.owner, &w.id).unwrap().is_none());
    f.automation.discard_suspended(&f.owner, &id).unwrap();
    assert_eq!(f.run(&id).state, "failed");
}
#[test]
fn restart_reopens_database_and_only_polls_same_handle() {
    let f = Fixture::new();
    let w = f.create(definition(
        Trigger::Manual,
        vec![step(
            "backup",
            Action::Backup {
                profile: "daily".into(),
            },
        )],
    ));
    let id = f.manual(&w);
    f.pulse(0);
    f.pulse(1);
    let handle = f.automation.list_steps(&f.owner, &id, None, 10).unwrap()[0]
        .operation_reference
        .clone()
        .unwrap();
    let Fixture {
        root,
        clock,
        db,
        port,
        automation,
        owner,
        ..
    } = f;
    drop(automation);
    db.close().unwrap();
    drop(db);
    let db = StateDatabase::open(
        StateDirectory::new(root.path().join("state"), None).unwrap(),
        clock,
    )
    .unwrap();
    let automation = Automation::new(db, port.clone()).unwrap();
    *port.status.lock().unwrap() = ActionStatus::Succeeded {
        output: serde_json::Value::Null,
    };
    automation.tick(NOW + 2).unwrap();
    assert_eq!(port.begins.lock().unwrap().len(), 1);
    assert_eq!(*port.polls.lock().unwrap(), vec![handle]);
    assert_eq!(automation.get_run(&owner, &id).unwrap().state, "succeeded");
}
#[test]
fn persisted_submission_intent_reconciles_with_same_idempotency_key() {
    let f = Fixture::new();
    let w = f.create(definition(
        Trigger::Manual,
        vec![step("start", Action::Start)],
    ));
    let id = f.manual(&w);
    f.pulse(0);
    let key = format!("automation.{id}.0.0");
    let handle = f
        .port
        .begin(&f.owner.principal, &Action::Start, &key)
        .unwrap();
    // Simulates a crash after host acceptance and before handle persistence.
    let recovered = Automation::new(f.db.clone(), f.port.clone()).unwrap();
    recovered.tick(NOW + 1).unwrap();
    let saved = recovered.list_steps(&f.owner, &id, None, 10).unwrap();
    assert_eq!(saved[0].operation_reference.as_ref(), Some(&handle.0));
    assert_eq!(f.port.handles.lock().unwrap().len(), 1);
    assert_eq!(*f.port.begins.lock().unwrap(), vec![key.clone(), key]);
}
#[test]
fn schedules_apply_skip_latest_and_bounded_catchup_across_downtime() {
    for (policy, expected) in [
        (MissedPolicy::SkipAndNotify, 0),
        (MissedPolicy::RunLatest, 1),
        (MissedPolicy::CatchUp { max: 3 }, 3),
    ] {
        let f = Fixture::new();
        let mut d = definition(
            Trigger::Interval { seconds: 60 },
            vec![step("notify", notice())],
        );
        d.missed_policy = policy;
        let w = f.create(d);
        f.pulse(600000);
        let runs = f
            .automation
            .list_runs(&f.owner, Some(&w.id), None, 100)
            .unwrap();
        assert_eq!(runs.len(), expected);
        if expected == 0 {
            assert_eq!(f.automation.unread_count(&f.owner).unwrap(), 1);
        } else {
            assert!(
                runs.iter()
                    .all(|r| r.trigger["occurrence"].as_i64().unwrap() <= NOW + 600000)
            );
            f.pulse(600001);
            f.pulse(600002);
            assert!(
                f.automation
                    .list_runs(&f.owner, Some(&w.id), None, 100)
                    .unwrap()
                    .iter()
                    .all(|r| r.state == "succeeded")
            );
        }
        f.pulse(600003);
        assert_eq!(
            f.automation
                .list_runs(&f.owner, Some(&w.id), None, 100)
                .unwrap()
                .len(),
            expected
        );
    }
}
#[test]
fn ontime_interval_and_cron_fire_once() {
    for trigger in [
        Trigger::Interval { seconds: 60 },
        Trigger::Cron {
            expr: "* * * * *".into(),
            utc_offset_minutes: 0,
        },
    ] {
        let f = Fixture::new();
        let w = f.create(definition(trigger, vec![step("notify", notice())]));
        f.pulse(60000);
        f.pulse(60000);
        assert_eq!(
            f.automation
                .list_runs(&f.owner, Some(&w.id), None, 100)
                .unwrap()
                .len(),
            1
        );
    }
}
#[test]
fn event_profile_filters_and_busy_duplicate_rule() {
    let f = Fixture::new();
    let w = f.create(definition(
        Trigger::BackupCompleted {
            profile: Some("daily".into()),
        },
        vec![step("delay", Action::Delay { secs: 60 })],
    ));
    assert!(
        f.automation
            .publish(AutomationEvent::BackupCompleted {
                profile: "other".into()
            })
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.automation
            .publish(AutomationEvent::BackupCompleted {
                profile: "daily".into()
            })
            .unwrap()
            .len(),
        1
    );
    assert!(
        f.automation
            .publish(AutomationEvent::BackupCompleted {
                profile: "daily".into()
            })
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.automation
            .list_runs(&f.owner, Some(&w.id), None, 10)
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn forward_output_branch_skips_intermediate_step() {
    let f = Fixture::new();
    let mut s = step("backup", Action::Start);
    s.on_success = SuccessBranch::IfOutputEquals {
        key: "ok".into(),
        value: serde_json::json!(true),
        then_step: "done".into(),
        otherwise: None,
    };
    let w = f.create(definition(
        Trigger::Manual,
        vec![s, step("skip", Action::Stop), step("done", notice())],
    ));
    let id = f.manual(&w);
    f.pulse(0);
    f.pulse(1);
    *f.port.status.lock().unwrap() = ActionStatus::Succeeded {
        output: serde_json::json!({"ok":true}),
    };
    f.pulse(2);
    assert_eq!(f.run(&id).current_step, 2);
    f.pulse(3);
    assert_eq!(f.run(&id).state, "succeeded");
    assert_eq!(f.port.begins.lock().unwrap().len(), 1);
}
#[test]
fn workflow_edits_do_not_rewrite_run_revision_and_rank_checks_apply() {
    let f = Fixture::new();
    let w = f.create(definition(
        Trigger::Manual,
        vec![step("delay", Action::Delay { secs: 60 })],
    ));
    let id = f.manual(&w);
    let updated = f
        .automation
        .update(
            &f.owner,
            &w.id,
            1,
            "New",
            &f.owner.principal,
            definition(Trigger::Manual, vec![step("notify", notice())]),
        )
        .unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(f.run(&id).revision, 1);
    assert!(matches!(
        f.run(&id).definition.steps[0].action,
        Action::Delay { .. }
    ));
    assert!(f.automation.set_enabled(&f.owner, &w.id, 1, false).is_err());
    assert!(f.automation.get_workflow(&f.moderator, &w.id).is_err());
    assert!(f.automation.delete(&f.moderator, &w.id, 2).is_err());
    f.automation.delete(&f.owner, &w.id, 2).unwrap();
    f.pulse(0);
    f.pulse(60000);
    assert_eq!(f.run(&id).state, "succeeded");
}
#[test]
fn notification_cursors_and_read_state_shared_per_principal_only() {
    let f = Fixture::new();
    let first = f
        .automation
        .notify(Severity::Info, "One", "Body", "test")
        .unwrap();
    let second = f
        .automation
        .notify(Severity::Warning, "Two", "Body", "test")
        .unwrap();
    let page = f.automation.list_since(&f.owner, 0, 1).unwrap();
    assert_eq!(page.notifications[0].id, first);
    let next = f
        .automation
        .list_since(&f.owner, page.next_cursor, 1)
        .unwrap();
    assert_eq!(next.notifications[0].id, second);
    f.automation
        .mark_read(&f.owner, std::slice::from_ref(&first))
        .unwrap();
    f.automation
        .acknowledge(&f.owner, std::slice::from_ref(&second))
        .unwrap();
    assert_eq!(f.automation.unread_count(&f.owner).unwrap(), 0);
    assert_eq!(f.automation.unread_count(&f.moderator).unwrap(), 2);
    let items = f.automation.list_since(&f.owner, 0, 10).unwrap();
    assert!(items.notifications[1].acknowledged);
    assert!(items.notifications[1].read);
    assert!(
        f.automation
            .mark_read(&f.owner, &[first.clone(), "missing".into()])
            .is_err()
    );
    let other_device = Subject {
        principal: f.owner.principal.clone(),
        device: DeviceId::parse("owner-other").unwrap(),
    };
    f.db.write(|c| {c.execute("INSERT INTO devices(id,server_id,principal_id,label,public_key_sha256,created_at) VALUES ('owner-other',?,'owner-user','Other',?,?)",params![f.db.server_id(),"c".repeat(64),NOW])?;Ok(())}).unwrap();
    assert_eq!(f.automation.unread_count(&other_device).unwrap(), 0);
    f.db.write(|c| {
        c.execute("DELETE FROM notifications", [])?;
        Ok(())
    })
    .unwrap();
    let third = f
        .automation
        .notify(Severity::Info, "Three", "Body", "test")
        .unwrap();
    let latest = f
        .automation
        .list_since(&f.owner, next.next_cursor, 10)
        .unwrap();
    assert_eq!(latest.notifications[0].id, third);
    assert!(latest.notifications[0].cursor > next.next_cursor);
}
#[test]
fn revoked_reader_and_input_bounds_refuse_access() {
    let f = Fixture::new();
    assert!(f.automation.list_since(&f.owner, 0, 257).is_err());
    assert!(
        f.automation
            .notify(Severity::Info, "Title", &"x".repeat(8193), "test")
            .is_err()
    );
    f.db.write(|c| {
        c.execute(
            "UPDATE devices SET revoked_at=? WHERE id=?",
            params![NOW, f.owner.device.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(f.automation.unread_count(&f.owner).is_err());
    assert!(f.automation.list_since(&f.owner, 0, 10).is_err());
}

#[test]
fn host_callbacks_can_use_the_same_database_without_nested_transactions() {
    let f = Fixture::new();
    *f.port.database.lock().unwrap() = Some(f.db.clone());
    let w = f.create(definition(
        Trigger::Manual,
        vec![step("start", Action::Start)],
    ));
    let id = f.manual(&w);
    f.pulse(0);
    f.pulse(1);
    *f.port.status.lock().unwrap() = ActionStatus::Succeeded {
        output: serde_json::Value::Null,
    };
    f.pulse(2);
    assert_eq!(f.run(&id).state, "succeeded");
}

#[test]
fn simultaneous_tick_drivers_cas_progress_and_idempotently_submit() {
    let f = Fixture::new();
    let w = f.create(definition(
        Trigger::Manual,
        vec![step("start", Action::Start), step("done", notice())],
    ));
    let id = f.manual(&w);
    for delta in 0..2 {
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let automation = &f.automation;
                scope.spawn(move || automation.tick(NOW + delta).unwrap());
            }
        });
    }
    assert_eq!(f.port.handles.lock().unwrap().len(), 1);
    *f.port.status.lock().unwrap() = ActionStatus::Succeeded {
        output: serde_json::Value::Null,
    };
    std::thread::scope(|scope| {
        for _ in 0..2 {
            let automation = &f.automation;
            scope.spawn(move || automation.tick(NOW + 2).unwrap());
        }
    });
    f.pulse(3);
    assert_eq!(f.run(&id).state, "succeeded");
    assert_eq!(f.automation.unread_count(&f.owner).unwrap(), 1);
}

#[test]
fn catchup_and_definition_byte_bounds_are_validated_before_persistence() {
    let mut d = definition(Trigger::Manual, vec![step("start", Action::Start)]);
    for max in [0, 11, 32] {
        d.missed_policy = MissedPolicy::CatchUp { max };
        assert!(d.validate().is_err());
    }
    d.missed_policy = MissedPolicy::CatchUp { max: 10 };
    assert!(d.validate().is_ok());
    let bytes = serde_json::to_vec(&d).unwrap();
    assert_eq!(WorkflowDefinition::from_json(&bytes).unwrap(), d);
    assert!(WorkflowDefinition::from_json(&vec![b' '; 65_537]).is_err());
    d.steps = (0..32)
        .map(|i| {
            step(
                &format!("step-{i}"),
                Action::Notify {
                    severity: Severity::Info,
                    title: "Title".into(),
                    body: "x".repeat(4096),
                },
            )
        })
        .collect();
    assert!(d.validate().is_err());
    let f = Fixture::new();
    assert_eq!(
        f.automation
            .create(&f.owner, "Too large", &f.owner.principal, d, true)
            .unwrap_err()
            .code(),
        "invalid_request"
    );
}

#[test]
fn notification_retention_prunes_old_and_overflow_preserving_monotonic_cursor() {
    let f = Fixture::new();
    let first = f
        .automation
        .notify(Severity::Info, "Old", "Body", "test")
        .unwrap();
    f.automation
        .mark_read(&f.owner, std::slice::from_ref(&first))
        .unwrap();
    f.db.write(|c| {
        c.execute("UPDATE notifications SET created_at=0 WHERE id=?",[&first])?;
        for i in 0..4100 {
            c.execute("INSERT INTO notifications(id,created_at,severity,title,body,source) VALUES (?,?,'info','Test','','test')",params![format!("bulk-{i}"),NOW])?;
        }
        Ok(())
    }).unwrap();
    f.automation
        .notify(Severity::Info, "Latest", "Body", "test")
        .unwrap();
    f.db.read(|c| {
        assert_eq!(
            c.query_row("SELECT count(*) FROM notifications", [], |r| r
                .get::<_, i64>(0))?,
            4096
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM notification_state", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .unwrap();
    let page = f.automation.list_since(&f.owner, 0, 10).unwrap();
    assert!(page.oldest_cursor.unwrap() > 1);
}

#[test]
fn managers_cannot_target_peer_or_higher_principal_workflows() {
    let f = Fixture::new();
    let admin = seed(&f.db, "admin-user", "administrator", 'd');
    let peer = seed(&f.db, "peer-owner", "owner", 'e');
    let w = f.create(definition(
        Trigger::Manual,
        vec![step("start", Action::Start)],
    ));
    let d = definition(Trigger::Manual, vec![step("notify", notice())]);
    assert_eq!(
        f.automation
            .create(
                &admin,
                "Owner authority",
                &f.owner.principal,
                d.clone(),
                true
            )
            .unwrap_err()
            .reason(),
        Some(crate::identity::DenyReason::Rank)
    );
    assert_eq!(
        f.automation
            .update(&admin, &w.id, 1, "Take over", &admin.principal, d.clone())
            .unwrap_err()
            .reason(),
        Some(crate::identity::DenyReason::Rank)
    );
    assert_eq!(
        f.automation.delete(&peer, &w.id, 1).unwrap_err().reason(),
        Some(crate::identity::DenyReason::Rank)
    );
    assert!(
        f.automation
            .create(&f.owner, "Delegated", &f.moderator.principal, d, true)
            .is_ok()
    );
}

#[test]
fn surviving_deleted_workflow_runs_do_not_starve_current_workflows() {
    let f = Fixture::new();
    let w = f.create(definition(
        Trigger::Manual,
        vec![step("start", Action::Start)],
    ));
    let encoded = serde_json::to_string(&w.definition).unwrap();
    f.db.write(|c| {
        // Stand in for live snapshots of 128 previously deleted workflows.
        for i in 0..128 {
            c.execute("INSERT INTO workflow_runs(id,workflow_id,revision,definition_json,principal_id,trigger_json,state,current_step,attempt,next_wake_at,started_at) VALUES (?,?,1,?,?,'{}','running',0,0,?,?)",params![format!("old-run-{i}"),format!("old-workflow-{i}"),encoded,f.owner.principal.as_str(),NOW,NOW])?;
        }
        Ok(())
    }).unwrap();
    let id = f.manual(&w);
    f.pulse(0);
    assert_eq!(
        f.automation
            .list_steps(&f.owner, &id, None, 10)
            .unwrap()
            .len(),
        1
    );
}

fn create_mutation(f: &Fixture) -> Mutation {
    Mutation::Create {
        name: "Receipt workflow".into(),
        execution_principal: f.owner.principal.to_string(),
        definition: definition(Trigger::Manual, vec![step("notice", notice())]),
        enabled: true,
    }
}
#[test]
fn mutation_receipts_replay_after_reopen_and_reject_changed_intent() {
    let f = Fixture::new();
    let command = create_mutation(&f);
    let original = f
        .automation
        .apply_idempotent(&f.owner, "receipt-create-key-0001", command.clone())
        .unwrap();
    let Fixture {
        root,
        clock,
        db,
        port,
        automation,
        owner,
        ..
    } = f;
    drop(automation);
    db.close().unwrap();
    drop(db);
    let db = StateDatabase::open(
        StateDirectory::new(root.path().join("state"), None).unwrap(),
        clock,
    )
    .unwrap();
    let reopened = Automation::new(db, port).unwrap();
    assert_eq!(
        original,
        reopened
            .apply_idempotent(&owner, "receipt-create-key-0001", command.clone())
            .unwrap()
    );
    let mut changed = command;
    if let Mutation::Create { name, .. } = &mut changed {
        *name = "Different".into();
    }
    assert!(matches!(
        reopened.apply_idempotent(&owner, "receipt-create-key-0001", changed),
        Err(MutationError::IdempotencyConflict)
    ));
    assert_eq!(reopened.list_workflows(&owner, None, 100).unwrap().len(), 1);
}
#[test]
fn mutation_receipt_insert_failure_rolls_back_workflow_and_audit() {
    let f = Fixture::new();
    f.db.write(|c| { c.execute_batch("CREATE TRIGGER receipt_fail BEFORE INSERT ON management_permission_receipts BEGIN SELECT RAISE(ABORT,'receipt fail'); END;")?; Ok(()) }).unwrap();
    assert!(
        f.automation
            .apply_idempotent(&f.owner, "receipt-failing-key-0001", create_mutation(&f))
            .is_err()
    );
    assert!(
        f.automation
            .list_workflows(&f.owner, None, 100)
            .unwrap()
            .is_empty()
    );
    f.db.read(|c| {
        let count: i64 = c.query_row(
            "SELECT count(*) FROM audit_events WHERE action='automation.create'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 0);
        Ok(())
    })
    .unwrap();
    f.db.write(|c| {
        c.execute_batch("DROP TRIGGER receipt_fail")?;
        Ok(())
    })
    .unwrap();
    f.automation
        .apply_idempotent(&f.owner, "receipt-failing-key-0001", create_mutation(&f))
        .unwrap();
}
#[test]
fn mutation_receipts_manual_run_and_delete_replay_without_second_effect() {
    let f = Fixture::new();
    let w = f.create(definition(Trigger::Manual, vec![step("notice", notice())]));
    let command = Mutation::RunManual { id: w.id.clone() };
    let first = f
        .automation
        .apply_idempotent(&f.owner, "receipt-manual-key-0001", command.clone())
        .unwrap();
    f.pulse(0);
    f.pulse(1);
    assert_eq!(
        first,
        f.automation
            .apply_idempotent(&f.owner, "receipt-manual-key-0001", command)
            .unwrap()
    );
    assert_eq!(
        f.automation
            .list_runs(&f.owner, Some(&w.id), None, 100)
            .unwrap()
            .len(),
        1
    );
    let delete = Mutation::Delete {
        id: w.id,
        expected_revision: w.revision,
    };
    let first = f
        .automation
        .apply_idempotent(&f.owner, "receipt-delete-key-0001", delete.clone())
        .unwrap();
    assert_eq!(
        first,
        f.automation
            .apply_idempotent(&f.owner, "receipt-delete-key-0001", delete)
            .unwrap()
    );
}
#[test]
fn mutation_cached_receipt_rechecks_capability_and_target_rank() {
    let f = Fixture::new();
    let administrator = seed(&f.db, "receipt-administrator", "administrator", 'c');
    let mut command = create_mutation(&f);
    if let Mutation::Create {
        execution_principal,
        ..
    } = &mut command
    {
        *execution_principal = f.moderator.principal.to_string();
    }
    let first = f
        .automation
        .apply_idempotent(&administrator, "receipt-rank-key-0001", command.clone())
        .unwrap();
    f.db.write(|c| {
        c.execute(
            "UPDATE role_assignments SET role_id='administrator' WHERE principal_id=?",
            [f.moderator.principal.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        f.automation
            .apply_idempotent(&administrator, "receipt-rank-key-0001", command.clone()),
        Err(MutationError::Control(crate::ControlError::Rejected {
            code: crate::IdentityErrorCode::Forbidden,
            ..
        }))
    ));
    f.db.write(|c| {
        c.execute(
            "UPDATE role_assignments SET role_id='moderator' WHERE principal_id=?",
            [f.moderator.principal.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        first,
        f.automation
            .apply_idempotent(&administrator, "receipt-rank-key-0001", command.clone())
            .unwrap()
    );
    f.db.write(|c| {
        c.execute(
            "UPDATE role_assignments SET role_id='viewer' WHERE principal_id=?",
            [administrator.principal.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        f.automation
            .apply_idempotent(&administrator, "receipt-rank-key-0001", command),
        Err(MutationError::Control(crate::ControlError::Rejected {
            code: crate::IdentityErrorCode::Forbidden,
            ..
        }))
    ));
}
#[test]
fn mutation_receipts_separate_notification_domain_and_principal_state() {
    let f = Fixture::new();
    let id = f
        .automation
        .notify(Severity::Info, "Notice", "Body", "test")
        .unwrap();
    let command = Mutation::MarkRead { ids: vec![id] };
    f.automation
        .apply_idempotent(&f.owner, "same-domain-scoped-key-001", create_mutation(&f))
        .unwrap();
    f.automation
        .apply_idempotent(&f.owner, "same-domain-scoped-key-001", command.clone())
        .unwrap();
    assert_eq!(f.automation.unread_count(&f.owner).unwrap(), 0);
    assert_eq!(f.automation.unread_count(&f.moderator).unwrap(), 1);
    f.automation
        .apply_idempotent(&f.moderator, "same-domain-scoped-key-001", command)
        .unwrap();
    assert_eq!(f.automation.unread_count(&f.moderator).unwrap(), 0);
    let mut changed = create_mutation(&f);
    if let Mutation::Create { name, .. } = &mut changed {
        *name = "Second device".into();
    }
    f.db.write(|c| {c.execute("INSERT INTO devices(id,server_id,principal_id,label,public_key_sha256,created_at) VALUES ('owner-second-device',?,?,'second',?,?)",params![f.db.server_id(),f.owner.principal.as_str(),"d".repeat(64),NOW])?;Ok(())}).unwrap();
    let second = Subject {
        principal: f.owner.principal.clone(),
        device: DeviceId::parse("owner-second-device").unwrap(),
    };
    f.automation
        .apply_idempotent(&second, "same-domain-scoped-key-001", changed)
        .unwrap();
    assert_eq!(
        f.automation
            .list_workflows(&f.owner, None, 100)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn cached_create_rechecks_changed_workflow_execution_principal() {
    let f = Fixture::new();
    let administrator = seed(&f.db, "receipt-editor", "administrator", 'e');
    let mut command = create_mutation(&f);
    if let Mutation::Create {
        execution_principal,
        ..
    } = &mut command
    {
        *execution_principal = f.moderator.principal.to_string();
    }
    let outcome = f
        .automation
        .apply_idempotent(
            &administrator,
            "cached-create-editor-key-001",
            command.clone(),
        )
        .unwrap();
    let MutationOutcome::Workflow {
        workflow_id,
        revision,
    } = outcome
    else {
        panic!("wrong outcome")
    };
    f.automation
        .update(
            &f.owner,
            &workflow_id,
            revision,
            "Owner took over",
            &f.owner.principal,
            definition(Trigger::Manual, vec![step("notice", notice())]),
        )
        .unwrap();
    assert!(matches!(
        f.automation
            .apply_idempotent(&administrator, "cached-create-editor-key-001", command),
        Err(MutationError::Control(crate::ControlError::Rejected {
            code: crate::IdentityErrorCode::Forbidden,
            ..
        }))
    ));
}
#[test]
fn notification_receipt_quota_does_not_consume_workflow_domain_and_expired_rows_prune() {
    let f = Fixture::new();
    let id = f
        .automation
        .notify(Severity::Info, "Notice", "Body", "test")
        .unwrap();
    let command = Mutation::MarkRead { ids: vec![id] };
    for n in 0..256 {
        f.automation
            .apply_idempotent(
                &f.moderator,
                &format!("notification-quota-key-{n:04}"),
                command.clone(),
            )
            .unwrap();
    }
    assert!(matches!(
        f.automation.apply_idempotent(
            &f.moderator,
            "notification-quota-overflow-key",
            command.clone()
        ),
        Err(MutationError::Saturated)
    ));
    f.automation
        .apply_idempotent(
            &f.owner,
            "workflow-independent-domain-key",
            create_mutation(&f),
        )
        .unwrap();
    f.clock
        .0
        .store(NOW + 8 * 24 * 60 * 60 * 1000, Ordering::SeqCst);
    f.automation
        .apply_idempotent(&f.moderator, "notification-quota-after-retention", command)
        .unwrap();
    f.db.read(|c| {
        let count: i64 = c.query_row(
            "SELECT count(*) FROM management_permission_receipts",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 130);
        Ok(())
    })
    .unwrap();
}

#[test]
fn authorization_outages_remain_pending_without_spam_and_are_deadline_bounded() {
    for error in [ActionError::Unavailable, ActionError::Busy] {
        let f = Fixture::new();
        let mut action = step(
            "backup",
            Action::Backup {
                profile: "daily".into(),
            },
        );
        action.timeout_secs = 3;
        let workflow = f.create(definition(Trigger::Manual, vec![action]));
        let id = f.manual(&workflow);
        *f.port.authorization_error.lock().unwrap() = Some(error);
        for delta in [0, 1000, 2000] {
            f.pulse(delta);
            assert_eq!(f.run(&id).state, "running");
        }
        assert!(f.port.begins.lock().unwrap().is_empty());
        assert_eq!(f.automation.unread_count(&f.owner).unwrap(), 0);
        let history = f.automation.list_steps(&f.owner, &id, None, 10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].state, "submitting");
        f.pulse(3000);
        assert_eq!(f.run(&id).state, "failed");
        assert_eq!(
            f.automation.list_steps(&f.owner, &id, None, 10).unwrap()[0].state,
            "timed_out"
        );
        assert!(f.port.begins.lock().unwrap().is_empty());
    }
}
#[test]
fn authorization_outage_recovery_submits_once_and_keeps_existing_handle() {
    let f = Fixture::new();
    let workflow = f.create(definition(
        Trigger::Manual,
        vec![step(
            "backup",
            Action::Backup {
                profile: "daily".into(),
            },
        )],
    ));
    let id = f.manual(&workflow);
    *f.port.authorization_error.lock().unwrap() = Some(ActionError::Unavailable);
    f.pulse(0);
    f.pulse(1000);
    *f.port.authorization_error.lock().unwrap() = None;
    f.pulse(2000);
    assert_eq!(f.port.begins.lock().unwrap().len(), 1);
    let handle = f.automation.list_steps(&f.owner, &id, None, 10).unwrap()[0]
        .operation_reference
        .clone();
    *f.port.authorization_error.lock().unwrap() = Some(ActionError::Busy);
    f.pulse(3000);
    assert!(f.port.polls.lock().unwrap().is_empty());
    assert_eq!(
        f.automation.list_steps(&f.owner, &id, None, 10).unwrap()[0].operation_reference,
        handle
    );
    *f.port.authorization_error.lock().unwrap() = None;
    *f.port.status.lock().unwrap() = ActionStatus::Succeeded {
        output: serde_json::Value::Null,
    };
    f.pulse(4000);
    assert_eq!(f.run(&id).state, "succeeded");
    assert_eq!(f.port.begins.lock().unwrap().len(), 1);
    assert_eq!(f.port.polls.lock().unwrap().len(), 1);
    assert_eq!(f.automation.unread_count(&f.owner).unwrap(), 0);
}
#[test]
fn unsupported_or_invalid_authorization_uses_failure_branch_not_suspension() {
    for error in [ActionError::Unsupported, ActionError::Invalid] {
        let f = Fixture::new();
        let mut first = step(
            "backup",
            Action::Backup {
                profile: "daily".into(),
            },
        );
        first.on_failure = FailureBranch::Next;
        first.retries = RetryPolicy {
            max_retries: 5,
            backoff_secs: 1,
        };
        let workflow = f.create(definition(
            Trigger::Manual,
            vec![first, step("notice", notice())],
        ));
        let id = f.manual(&workflow);
        *f.port.authorization_error.lock().unwrap() = Some(error);
        f.pulse(0);
        assert_eq!(f.run(&id).current_step, 1);
        f.pulse(1);
        assert_eq!(f.run(&id).state, "succeeded");
        let history = f.automation.list_steps(&f.owner, &id, None, 10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].state, "failed");
        assert_eq!(history[0].attempt, 0);
        assert_eq!(history[0].error_code.as_deref(), Some(error.code()));
        assert!(f.port.begins.lock().unwrap().is_empty());
        assert_eq!(f.automation.unread_count(&f.owner).unwrap(), 1);
    }
}
#[test]
fn persisted_delay_obeys_its_timeout_before_wake_and_uses_failure_branch() {
    let f = Fixture::new();
    let mut delay = step("delay", Action::Delay { secs: 10 });
    delay.timeout_secs = 3;
    delay.on_failure = FailureBranch::Next;
    let workflow = f.create(definition(
        Trigger::Manual,
        vec![delay, step("notice", notice())],
    ));
    let id = f.manual(&workflow);
    f.pulse(0);
    f.pulse(2000);
    assert_eq!(f.run(&id).current_step, 0);
    f.pulse(3000);
    assert_eq!(f.run(&id).current_step, 1);
    f.pulse(3001);
    assert_eq!(f.run(&id).state, "succeeded");
    let history = f.automation.list_steps(&f.owner, &id, None, 10).unwrap();
    assert_eq!(history[0].state, "timed_out");
    assert_eq!(history[0].error_code.as_deref(), Some("timed_out"));
}

#[test]
fn newest_run_pages_use_time_and_id_ties_without_skipping() {
    let f = Fixture::new();
    let w = f.create(definition(Trigger::Manual, vec![step("notify", notice())]));
    let first = f.manual(&w);
    f.pulse(1);
    f.pulse(2);
    let second = f.manual(&w);
    f.clock.0.store(NOW + 3, Ordering::SeqCst);
    f.db.write(|c| {
        c.execute(
            "UPDATE workflow_runs SET started_at=? WHERE id=?",
            params![NOW + 3, second],
        )?;
        Ok(())
    })
    .unwrap();
    let page = f
        .automation
        .list_runs_newest(&f.owner, Some(&w.id), None, 1)
        .unwrap();
    assert_eq!(page[0].id, second);
    let page = f
        .automation
        .list_runs_newest(&f.owner, Some(&w.id), Some(&second), 1)
        .unwrap();
    assert_eq!(page[0].id, first);
    assert!(
        f.automation
            .list_runs_newest(&f.owner, None, Some(&first), 10)
            .unwrap()
            .is_empty()
    );
}
