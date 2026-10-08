mod support;

use rusqlite::params;
use support::Fixture;
use vesta_state::Clock;
use vesta_state::events::{Bounds, EventLog, MAX_PAGE, MAX_PAYLOAD_CHARS, ScanRow};

fn append(
    fixture: &Fixture,
    events: &EventLog,
    event_type: &str,
    operation_id: Option<&str>,
    payload: &str,
) {
    fixture
        .db
        .write(|connection| events.append(connection, event_type, operation_id, Some(payload)))
        .unwrap();
}

/// Bypass the normal payload-size validation to create genuine legacy/corrupt-row scan fixtures.
fn raw_append(fixture: &Fixture, event_type: &str, operation_id: Option<&str>, payload: &str) {
    fixture
        .db
        .write(|connection| {
            connection.execute(
                "INSERT INTO events (server_id, type, operation_id, at, payload_json) \
                 SELECT server_id, ?1, ?2, 0, ?3 FROM server_identity WHERE singleton = 1",
                params![event_type, operation_id, payload],
            )?;
            Ok(())
        })
        .unwrap();
}

fn scan(
    fixture: &Fixture,
    events: &EventLog,
    after: i64,
    through: i64,
    limit: usize,
    types: &[&str],
    max_payload_chars: usize,
) -> Vec<ScanRow> {
    fixture
        .db
        .read(|connection| events.scan(connection, after, through, limit, types, max_payload_chars))
        .unwrap()
}

fn cursors(rows: &[ScanRow]) -> Vec<i64> {
    rows.iter().map(|row| row.cursor).collect()
}

#[test]
fn bounds_describe_empty_retained_and_fully_pruned_journals() {
    let fixture = Fixture::open();
    let events = EventLog::new(fixture.db.clone());

    assert_eq!(
        Bounds {
            pruned_through: 0,
            oldest_retained: 0,
            latest: 0,
        },
        fixture
            .db
            .read(|connection| events.bounds(connection))
            .unwrap()
    );

    for _ in 0..3 {
        append(&fixture, &events, "a.kind", None, "{}");
    }
    assert_eq!(
        Bounds {
            pruned_through: 0,
            oldest_retained: 1,
            latest: 3,
        },
        fixture
            .db
            .read(|connection| events.bounds(connection))
            .unwrap()
    );

    assert_eq!(
        3,
        events
            .prune_older_than(fixture.clock.millis() + 1_000)
            .unwrap()
    );
    assert_eq!(
        Bounds {
            pruned_through: 3,
            oldest_retained: 0,
            latest: 3,
        },
        fixture
            .db
            .read(|connection| events.bounds(connection))
            .unwrap()
    );
    let persisted_watermark: String = fixture
        .db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT value FROM meta WHERE key = 'events_pruned_through'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!("3", persisted_watermark);

    append(&fixture, &events, "a.kind", None, "{}");
    assert_eq!(
        Bounds {
            pruned_through: 3,
            oldest_retained: 4,
            latest: 4,
        },
        fixture
            .db
            .read(|connection| events.bounds(connection))
            .unwrap()
    );
}

#[test]
fn scan_projects_selected_types_with_payload_caps_and_truncated_type_headers() {
    let fixture = Fixture::open();
    let events = EventLog::new(fixture.db.clone());

    append(&fixture, &events, "wanted.kind", Some("operation-1"), "{}");
    append(
        &fixture,
        &events,
        "other.kind",
        Some("operation-2"),
        "{\"secret\":\"never-projected\"}",
    );
    raw_append(
        &fixture,
        "wanted.kind",
        Some("oversized-operation"),
        &"x".repeat(5_000),
    );
    raw_append(&fixture, &"y".repeat(100), None, "{}");

    let rows = scan(&fixture, &events, 0, 100, 10, &["wanted.kind"], 4);
    assert_eq!(4, rows.len());
    assert_eq!(Some("{}"), rows[0].payload_json.as_deref());
    assert_eq!(2, rows[0].payload_chars);
    assert_eq!(Some("operation-1"), rows[0].operation_id.as_deref());
    assert_eq!(None, rows[1].payload_json.as_deref());
    assert_eq!(0, rows[1].payload_chars);
    assert_eq!(None, rows[2].payload_json.as_deref());
    assert_eq!(5_000, rows[2].payload_chars);
    assert_eq!(65, rows[3].event_type.chars().count());
    assert_eq!(None, rows[3].payload_json.as_deref());
    assert!(!format!("{rows:?}").contains("never-projected"));
}

#[test]
fn scan_does_not_project_a_large_unrelated_payload() {
    let fixture = Fixture::open();
    let events = EventLog::new(fixture.db.clone());
    let unrelated_payload = format!(
        "{{\"secret\":\"large-unrelated\",\"padding\":\"{}\"}}",
        "z".repeat(100_000)
    );

    raw_append(&fixture, "unrelated.kind", None, &unrelated_payload);
    append(&fixture, &events, "wanted.kind", None, "{}");

    let rows = scan(&fixture, &events, 0, 2, 10, &["wanted.kind"], 4);
    assert_eq!(2, rows.len());
    assert_eq!(None, rows[0].payload_json.as_deref());
    assert_eq!(0, rows[0].payload_chars);
    assert_eq!(Some("{}"), rows[1].payload_json.as_deref());
    assert_eq!(2, rows[1].payload_chars);
    assert!(!format!("{rows:?}").contains("large-unrelated"));
}

#[test]
fn scan_truncates_long_operation_id_headers() {
    let fixture = Fixture::open();
    let events = EventLog::new(fixture.db.clone());
    let operation_id = "operation-".repeat(10);
    append(&fixture, &events, "wanted.kind", Some(&operation_id), "{}");

    let rows = scan(&fixture, &events, 0, 1, 10, &["wanted.kind"], 4);
    assert_eq!(1, rows.len());
    let projected_operation_id = rows[0].operation_id.as_deref().unwrap();
    assert_eq!(65, projected_operation_id.chars().count());
    assert_eq!(&operation_id[..65], projected_operation_id);
}

#[test]
fn scan_range_is_exclusive_after_inclusive_through_and_limited() {
    let fixture = Fixture::open();
    let events = EventLog::new(fixture.db.clone());
    for _ in 0..6 {
        append(&fixture, &events, "a.kind", None, "{}");
    }

    assert_eq!(
        vec![3, 4],
        cursors(&scan(&fixture, &events, 2, 4, 10, &["a.kind"], 100))
    );
    assert_eq!(
        vec![1, 2],
        cursors(&scan(&fixture, &events, 0, 6, 2, &["a.kind"], 100))
    );
    assert!(scan(&fixture, &events, 6, 6, 10, &["a.kind"], 100).is_empty());
    assert!(scan(&fixture, &events, 5, 3, 10, &["a.kind"], 100).is_empty());
}

#[test]
fn invalid_scan_requests_are_rejected() {
    let fixture = Fixture::open();
    let events = EventLog::new(fixture.db.clone());
    let nine_types: Vec<String> = (0..9).map(|index| format!("t{index}")).collect();
    let nine_types: Vec<&str> = nine_types.iter().map(String::as_str).collect();

    let invalid = [
        fixture
            .db
            .read(|connection| events.scan(connection, 0, 1, 0, &["a"], 10)),
        fixture
            .db
            .read(|connection| events.scan(connection, 0, 1, MAX_PAGE + 1, &["a"], 10)),
        fixture
            .db
            .read(|connection| events.scan(connection, 0, 1, 1, &[], 10)),
        fixture
            .db
            .read(|connection| events.scan(connection, 0, 1, 1, &nine_types, 10)),
        fixture
            .db
            .read(|connection| events.scan(connection, 0, 1, 1, &["a"], MAX_PAYLOAD_CHARS + 1)),
    ];
    for result in invalid {
        assert_eq!("state_invalid_request", result.unwrap_err().code());
    }
}

#[test]
fn scan_accepts_the_maximum_page_and_payload_cap() {
    let fixture = Fixture::open();
    let events = EventLog::new(fixture.db.clone());
    append(&fixture, &events, "a.kind", None, "{}");

    let rows = scan(
        &fixture,
        &events,
        0,
        1,
        MAX_PAGE,
        &["a.kind"],
        MAX_PAYLOAD_CHARS,
    );
    assert_eq!(1, rows.len());
    assert_eq!(Some("{}"), rows[0].payload_json.as_deref());
}

#[test]
fn existing_trusted_read_still_behaves_identically() {
    let fixture = Fixture::open();
    let events = EventLog::new(fixture.db.clone());
    append(&fixture, &events, "a.kind", None, "{}");
    append(&fixture, &events, "a.kind", None, "{}");

    let page = events.read(0, 1).unwrap();
    assert_eq!(1, page.events.len());
    assert_eq!(1, page.next_cursor);
    assert!(!page.gap);
    assert_eq!(1, page.oldest_available_cursor);
}
