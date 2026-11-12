mod support;

use support::{Fixture, TestClock};
use vesta_state::{
    StateDatabase, StateDirectory,
    audit::{AuditActor, AuditLog, AuditRecord},
    events::EventLog,
    settings::{ApplyMode, SettingsStore},
};

const FIXED_TIME_MS: i64 = 1_791_115_200_000; // 2026-10-04T12:00:00Z

fn actor() -> AuditActor {
    AuditActor::new("owner".into(), "device".into(), "local-cli".into()).unwrap()
}

fn fixture_at_fixed_time() -> Fixture {
    Fixture::with_time(FIXED_TIME_MS)
}

fn reopen(fixture: &Fixture) -> StateDatabase {
    fixture.db.close().unwrap();
    let directory = StateDirectory::new(fixture.state_path(), Some(&fixture.game_path())).unwrap();
    StateDatabase::open(directory, fixture.clock.clone()).unwrap()
}

#[test]
fn audit_is_append_only_and_modified_record_is_detected_including_on_service_open() {
    let fixture = fixture_at_fixed_time();
    let audit = AuditLog::new(fixture.db.clone()).unwrap();
    audit
        .append_record(
            &AuditRecord::new(
                actor(),
                "files.write".into(),
                "config.txt".into(),
                "ok".into(),
                None,
                [("revision".into(), Some("1".into()))],
            )
            .unwrap(),
        )
        .unwrap();
    audit
        .append_record(
            &AuditRecord::of(
                actor(),
                "files.read".into(),
                "config.txt".into(),
                "ok".into(),
                None,
            )
            .unwrap(),
        )
        .unwrap();

    let valid = audit.verify_chain().unwrap();
    assert!(valid.valid);
    assert_eq!(valid.checked, 2);
    assert_eq!(audit.page(0, 1).unwrap().len(), 1);
    assert_eq!(audit.page(1, 1).unwrap().len(), 1);

    let update_error = fixture
        .db
        .write(|c| {
            c.execute(
                "UPDATE audit_events SET outcome = 'changed' WHERE seq = 1",
                [],
            )?;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(update_error.code(), "state_io");

    let delete_error = fixture
        .db
        .write(|c| {
            c.execute("DELETE FROM audit_events WHERE seq = 1", [])?;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(delete_error.code(), "state_io");

    // Simulate a local administrator bypassing the append-only trigger, not a remote API.
    fixture
        .db
        .write(|c| {
            c.execute("DROP TRIGGER audit_events_no_update", [])?;
            c.execute(
                "UPDATE audit_events SET outcome = 'changed' WHERE seq = 1",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let tampered = audit.verify_chain().unwrap();
    assert!(!tampered.valid);
    assert_eq!(tampered.first_broken_seq, Some(1));

    let startup_error = AuditLog::new(fixture.db.clone()).err().unwrap();
    assert_eq!(startup_error.code(), "state_audit_chain_corrupt");
}

#[test]
fn accidental_secret_metadata_keys_are_rejected() {
    let a = actor();
    for key in [
        "password",
        "apiToken",
        "privateKey",
        "secret",
        "credential",
        "cookie",
        "authorization",
        "command",
    ] {
        let error = AuditRecord::new(
            a.clone(),
            "change".into(),
            "server".into(),
            "ok".into(),
            None,
            [(key.into(), Some("secret-value".into()))],
        )
        .err()
        .unwrap();
        assert_eq!(error.code(), "state_invalid_request", "key {key:?}");
    }

    // The forbidden fragments are case-insensitive and match inside larger keys.
    let error = AuditRecord::new(
        a,
        "change".into(),
        "server".into(),
        "ok".into(),
        None,
        [("myAuthorizationHeader".into(), Some("x".into()))],
    )
    .err()
    .unwrap();
    assert_eq!(error.code(), "state_invalid_request");
}

#[test]
fn audit_metadata_bounds_and_canonical_encoding_are_enforced() {
    let a = actor();
    let left = vec![
        ("z".to_owned(), Some("last".to_owned())),
        ("a".to_owned(), Some("first".to_owned())),
    ];
    let right = vec![
        ("a".to_owned(), Some("first".to_owned())),
        ("z".to_owned(), Some("last".to_owned())),
    ];
    let left_record = AuditRecord::new(
        a.clone(),
        "change".into(),
        "server".into(),
        "ok".into(),
        None,
        left.clone(),
    )
    .unwrap();
    let right_record = AuditRecord::new(
        a.clone(),
        "change".into(),
        "server".into(),
        "ok".into(),
        None,
        right,
    )
    .unwrap();
    assert_eq!(left_record.metadata_json(), right_record.metadata_json());
    assert_eq!(left_record.metadata_json(), r#"{"a":"first","z":"last"}"#);

    // Null metadata values follow the Rust API's documented normalization to empty text.
    let null_value = AuditRecord::new(
        a.clone(),
        "change".into(),
        "server".into(),
        "ok".into(),
        None,
        [("empty".into(), None)],
    )
    .unwrap();
    assert_eq!(null_value.metadata_json(), r#"{"empty":""}"#);

    let max_entries = (0..16).map(|index| (format!("k{index}"), Some(String::new())));
    assert!(
        AuditRecord::new(
            a.clone(),
            "change".into(),
            "server".into(),
            "ok".into(),
            None,
            max_entries,
        )
        .is_ok()
    );
    let too_many_entries = (0..17).map(|index| (format!("k{index}"), Some(String::new())));
    assert_eq!(
        AuditRecord::new(
            a.clone(),
            "change".into(),
            "server".into(),
            "ok".into(),
            None,
            too_many_entries,
        )
        .err()
        .unwrap()
        .code(),
        "state_invalid_request"
    );

    let max_key = "k".repeat(64);
    assert!(
        AuditRecord::new(
            a.clone(),
            "change".into(),
            "server".into(),
            "ok".into(),
            None,
            [(max_key, Some("v".into()))],
        )
        .is_ok()
    );
    assert_eq!(
        AuditRecord::new(
            a.clone(),
            "change".into(),
            "server".into(),
            "ok".into(),
            None,
            [("k".repeat(65), Some("v".into()))],
        )
        .err()
        .unwrap()
        .code(),
        "state_invalid_request"
    );

    let max_value = "v".repeat(256);
    assert!(
        AuditRecord::new(
            a.clone(),
            "change".into(),
            "server".into(),
            "ok".into(),
            None,
            [("value".into(), Some(max_value))],
        )
        .is_ok()
    );
    assert_eq!(
        AuditRecord::new(
            a.clone(),
            "change".into(),
            "server".into(),
            "ok".into(),
            None,
            [("value".into(), Some("v".repeat(257)))],
        )
        .err()
        .unwrap()
        .code(),
        "state_invalid_request"
    );
}

#[test]
fn audit_append_and_caller_mutation_roll_back_together() {
    let fixture = fixture_at_fixed_time();
    let audit = AuditLog::new(fixture.db.clone()).unwrap();
    fixture
        .db
        .write(|c| {
            c.execute("CREATE TABLE caller_mutation (value TEXT NOT NULL)", [])?;
            Ok(())
        })
        .unwrap();

    let rollback = fixture
        .db
        .write(|c| {
            c.execute("INSERT INTO caller_mutation(value) VALUES ('changed')", [])?;
            audit.append(
                c,
                &AuditRecord::of(actor(), "change".into(), "server".into(), "ok".into(), None)
                    .unwrap(),
            )?;
            Err::<(), _>(vesta_state::StateError::new("state_test_rollback"))
        })
        .unwrap_err();
    assert_eq!(rollback.code(), "state_test_rollback");
    assert!(audit.page(0, 10).unwrap().is_empty());
    let mutation_count: i64 = fixture
        .db
        .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM caller_mutation", [], |row| row.get(0))?))
        .unwrap();
    assert_eq!(mutation_count, 0);
}

#[test]
fn desired_and_effective_revisions_require_explicit_acknowledgement() {
    let fixture = fixture_at_fixed_time();
    let settings = SettingsStore::new(fixture.db.clone()).unwrap();

    let first = settings
        .set_desired(&actor(), "game.memory", "4G", ApplyMode::OnRestart)
        .unwrap();
    assert!(first.pending());
    assert_eq!(first.effective_value, None);
    assert_eq!(first.effective_revision, 0);

    let applied = settings
        .mark_effective(&actor(), &first.key, first.desired_revision)
        .unwrap();
    assert!(!applied.pending());

    let next = settings
        .set_desired(&actor(), &first.key, "6G", ApplyMode::Manual)
        .unwrap();
    assert_eq!(next.desired_revision, 2);
    assert_eq!(next.effective_value.as_deref(), Some("4G"));
    assert!(next.pending());

    let stale = settings
        .mark_effective(&actor(), &next.key, 1)
        .err()
        .unwrap();
    assert_eq!(stale.code(), "state_stale_revision");
    assert_eq!(settings.pending().unwrap().len(), 1);

    let immediate = settings
        .set_desired(&actor(), "console.visible", "true", ApplyMode::Immediate)
        .unwrap();
    assert!(
        immediate.pending(),
        "IMMEDIATE still requires the adapter to acknowledge application"
    );

    let reopened_db = reopen(&fixture);
    let persisted = SettingsStore::new(reopened_db.clone())
        .unwrap()
        .get("game.memory")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.desired_value, "6G");
    assert_eq!(persisted.effective_value.as_deref(), Some("4G"));
    reopened_db.close().unwrap();
}

#[test]
fn settings_constructor_binds_each_store_to_its_own_database_and_audit_chain() {
    let first = fixture_at_fixed_time();
    let second = fixture_at_fixed_time();
    assert_ne!(first.db.server_id(), second.db.server_id());
    let constructor: fn(StateDatabase) -> vesta_state::Result<SettingsStore> = SettingsStore::new;
    let first_store = constructor(first.db.clone()).unwrap();
    let second_store = constructor(second.db.clone()).unwrap();
    first_store
        .set_desired(&actor(), "fixture.value", "first", ApplyMode::Manual)
        .unwrap();
    second_store
        .set_desired(&actor(), "fixture.value", "second", ApplyMode::Manual)
        .unwrap();
    assert_eq!(
        first_store
            .get("fixture.value")
            .unwrap()
            .unwrap()
            .desired_value,
        "first"
    );
    assert_eq!(
        second_store
            .get("fixture.value")
            .unwrap()
            .unwrap()
            .desired_value,
        "second"
    );
    for fixture in [&first, &second] {
        let audit = AuditLog::new(fixture.db.clone()).unwrap();
        let entries = audit.page(0, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].server_id, fixture.db.server_id());
        assert!(audit.verify_chain().unwrap().valid);
    }
    let missing = first_store
        .mark_effective(&actor(), "missing", 1)
        .unwrap_err();
    assert_eq!(missing.code(), "state_not_found");
    assert_eq!(
        AuditLog::new(first.db.clone())
            .unwrap()
            .page(0, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn setting_mutation_fails_closed_when_audit_storage_fails() {
    let fixture = fixture_at_fixed_time();
    let settings = SettingsStore::new(fixture.db.clone()).unwrap();
    fixture
        .db
        .write(|c| {
            c.execute_batch(
                "CREATE TRIGGER reject_audit BEFORE INSERT ON audit_events \
                 BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
            )?;
            Ok(())
        })
        .unwrap();

    let error = settings
        .set_desired(&actor(), "game.memory", "4G", ApplyMode::OnRestart)
        .err()
        .unwrap();
    assert_eq!(error.code(), "state_io");
    assert!(settings.get("game.memory").unwrap().is_none());
}

#[test]
fn cursor_pagination_retention_gaps_and_empty_stream_watermark_are_durable() {
    let fixture = fixture_at_fixed_time();
    let events = EventLog::new(fixture.db.clone());
    for _ in 0..3 {
        fixture
            .db
            .write(|c| events.append(c, "test.changed", None, Some("{}")))
            .unwrap();
    }

    let first = events.read(0, 1).unwrap();
    assert_eq!(first.events.len(), 1);
    assert!(!first.gap);
    assert_eq!(first.events[0].server_id, fixture.db.server_id());
    assert_eq!(events.read(first.next_cursor, 500).unwrap().events.len(), 2);

    assert_eq!(events.prune_older_than(FIXED_TIME_MS + 1_000).unwrap(), 3);
    let gap = events.read(0, 10).unwrap();
    assert!(gap.gap);
    assert!(gap.events.is_empty());
    assert_eq!(gap.next_cursor, 3);
    assert_eq!(gap.oldest_available_cursor, 0);
    assert!(!events.read(3, 10).unwrap().gap);

    fixture
        .db
        .write(|c| events.append(c, "test.new", None, Some("{}")))
        .unwrap();
    assert_eq!(events.read(3, 10).unwrap().events[0].cursor, 4);

    let reopened_db = reopen(&fixture);
    assert!(EventLog::new(reopened_db.clone()).read(0, 10).unwrap().gap);
    reopened_db.close().unwrap();
}

#[test]
fn clock_rollback_does_not_prune_nonexpired_events_or_create_holes() {
    let fixture = Fixture::with_time(100);
    let events = EventLog::new(fixture.db.clone());
    for _ in 0..3 {
        fixture
            .db
            .write(|c| events.append(c, "test.changed", None, Some("{}")))
            .unwrap();
    }
    fixture
        .db
        .write(|c| {
            c.execute("UPDATE events SET at = 100 WHERE cursor IN (1, 3)", [])?;
            c.execute("UPDATE events SET at = 1000 WHERE cursor = 2", [])?;
            Ok(())
        })
        .unwrap();

    assert_eq!(events.prune_older_than(500).unwrap(), 1);
    let page = events.read(0, 10).unwrap();
    assert!(page.gap);
    assert_eq!(
        page.events
            .iter()
            .map(|event| event.cursor)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
}

// Keep the shared mutable clock API exercised here because event pruning is
// intentionally driven by an explicit cutoff rather than the wall clock.
#[test]
fn event_timestamp_uses_the_shared_test_clock() {
    let fixture = Fixture::with_time(1);
    let clock: &TestClock = &fixture.clock;
    clock.set(42);
    let events = EventLog::new(fixture.db.clone());
    fixture
        .db
        .write(|c| events.append(c, "test.clock", None, None))
        .unwrap();
    assert_eq!(events.read(0, 1).unwrap().events[0].at_ms, 42);
}

#[test]
fn configuration_editor_label_and_time_survive_account_rename_and_reopen() {
    let fixture = fixture_at_fixed_time();
    fixture.db.write(|c| {
        c.execute("INSERT INTO principals(id,server_id,display_name,status,created_at,updated_at) VALUES('owner',?,'First editor','active',0,0)", [fixture.db.server_id()])?;
        Ok(())
    }).unwrap();
    let audit = AuditLog::new(fixture.db.clone()).unwrap();
    audit
        .append_record(
            &AuditRecord::of(
                actor(),
                "management.permissions.mutate".into(),
                "server".into(),
                "success".into(),
                None,
            )
            .unwrap(),
        )
        .unwrap();
    fixture
        .db
        .write(|c| {
            c.execute(
                "UPDATE principals SET display_name='Renamed editor' WHERE id='owner'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    drop(audit);
    let db = reopen(&fixture);
    let audit = AuditLog::new(db).unwrap();
    let rows = audit.page(0, 10).unwrap();
    let row = rows
        .iter()
        .find(|row| row.action == "management.permissions.mutate")
        .unwrap();
    assert_eq!(row.at_ms, FIXED_TIME_MS);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&row.metadata_json).unwrap()["editor_display_name"],
        "First editor"
    );
    assert!(audit.verify_chain().unwrap().valid);
}
