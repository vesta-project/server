mod support;

use rusqlite::params;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use support::Fixture;
use vesta_state::events::{EventLog, MAX_PAYLOAD_CHARS};

#[test]
fn selected_corrupt_nul_payload_cannot_bypass_sql_projection_byte_bound() {
    let fixture = Fixture::open();
    let events = EventLog::new(fixture.db.clone());
    let corrupt = format!("x\0{}", "sensitive-tail".repeat(10_000));
    let cursor = fixture
        .db
        .write(|c| {
            c.execute(
            "INSERT INTO events (server_id,type,operation_id,at,payload_json) VALUES (?,?,?,?,?)",
            params![fixture.db.server_id(), "operation.status", "op", 1, corrupt],
        )?;
            Ok(c.last_insert_rowid())
        })
        .unwrap();
    let rows = fixture
        .db
        .read(|c| events.scan(c, 0, cursor, 1, &["operation.status"], 16))
        .unwrap();
    assert_eq!(rows.len(), 1);
    // Corrupt text is explicitly marked, not counted as its harmless prefix.
    assert_eq!(rows[0].payload_chars, -1);
    assert_eq!(rows[0].payload_json, None);
    assert_eq!(rows[0].cursor, cursor);
}

#[test]
fn small_nul_payload_is_not_projected_even_when_it_fits_the_byte_bound() {
    let fixture = Fixture::open();
    let events = EventLog::new(fixture.db.clone());
    let cursor = fixture
        .db
        .write(|c| {
            c.execute(
                "INSERT INTO events (server_id,type,at,payload_json) VALUES (?,?,?,?)",
                params![fixture.db.server_id(), "test.corrupt", 1, "x\0yz"],
            )?;
            Ok(c.last_insert_rowid())
        })
        .unwrap();
    let rows = fixture
        .db
        .read(|c| events.scan(c, 0, cursor, 1, &["test.corrupt"], 1))
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].payload_chars, -1);
    assert_eq!(rows[0].payload_json, None);
    assert_eq!(rows[0].cursor, cursor);
}

#[test]
fn append_rejects_embedded_nul_without_creating_an_event() {
    let fixture = Fixture::open();
    let events = EventLog::new(fixture.db.clone());
    let error = fixture
        .db
        .write(|c| events.append(c, "test.changed", None, Some("x\0yz")))
        .unwrap_err();
    assert_eq!(error.code(), "state_invalid_request");
    assert!(events.read(0, 10).unwrap().events.is_empty());
}

#[test]
fn append_limits_count_utf16_but_scan_projection_accepts_bounded_multibyte_payloads() {
    let fixture = Fixture::open();
    let events = EventLog::new(fixture.db.clone());
    let allowed = "😀".repeat(MAX_PAYLOAD_CHARS / 2);
    let cursor = fixture
        .db
        .write(|c| events.append(c, "emoji", None, Some(&allowed)))
        .unwrap();
    let oversized = format!("{allowed}😀");
    let error = fixture
        .db
        .write(|c| events.append(c, "emoji", None, Some(&oversized)))
        .unwrap_err();
    assert_eq!(error.code(), "state_invalid_request");
    let rows = fixture
        .db
        .read(|c| events.scan(c, 0, cursor, 1, &["emoji"], MAX_PAYLOAD_CHARS))
        .unwrap();
    assert_eq!(rows[0].payload_json.as_deref(), Some(allowed.as_str()));
    assert_eq!(rows[0].payload_chars, (MAX_PAYLOAD_CHARS / 2) as i64);
    assert_eq!(events.read(0, 500).unwrap().events.len(), 1);
}

#[test]
fn reader_snapshot_is_fixed_before_callback_and_does_not_block_a_wal_writer() {
    let fixture = Fixture::open();
    fixture
        .db
        .write(|c| {
            c.execute("INSERT INTO meta VALUES ('snapshot-value','before')", [])?;
            Ok(())
        })
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (continue_tx, continue_rx) = mpsc::channel();
    let reader_db = fixture.db.clone();
    let reader = thread::spawn(move || {
        reader_db.read(|c| {
            entered_tx.send(()).unwrap();
            continue_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("writer must complete while reader snapshot is open");
            Ok(c.query_row(
                "SELECT value FROM meta WHERE key='snapshot-value'",
                [],
                |row| row.get::<_, String>(0),
            )?)
        })
    });
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("reader enters its established snapshot");
    let write_result = fixture.db.write(|c| {
        c.execute(
            "UPDATE meta SET value='after' WHERE key='snapshot-value'",
            [],
        )?;
        Ok(())
    });
    continue_tx.send(()).unwrap();
    let old_value = reader.join().expect("reader thread exits").unwrap();
    write_result.unwrap();
    assert_eq!(old_value, "before");
    let new_value = fixture
        .db
        .read(|c| {
            Ok(c.query_row(
                "SELECT value FROM meta WHERE key='snapshot-value'",
                [],
                |row| row.get::<_, String>(0),
            )?)
        })
        .unwrap();
    assert_eq!(new_value, "after");
}

#[test]
fn runner_lock_is_lifetime_held_and_never_truncates_existing_bytes() {
    let fixture = Fixture::open();
    let lock_path = fixture.db.directory().path().join("operation-runner.lock");
    std::fs::write(&lock_path, "existing lock-file marker").unwrap();
    let first = fixture.db.directory().acquire_runner_lock().unwrap();
    let error = fixture.db.directory().acquire_runner_lock().unwrap_err();
    assert_eq!(error.code(), "state_runner_lock_held");
    // Windows byte-range locking denies data reads until release; metadata remains readable.
    assert_eq!(
        std::fs::metadata(&lock_path).unwrap().len(),
        "existing lock-file marker".len() as u64
    );
    drop(first);
    assert_eq!(
        std::fs::read_to_string(&lock_path).unwrap(),
        "existing lock-file marker"
    );
    let second = fixture.db.directory().acquire_runner_lock().unwrap();
    assert_eq!(
        std::fs::metadata(&lock_path).unwrap().len(),
        "existing lock-file marker".len() as u64
    );
    drop(second);
    assert_eq!(
        std::fs::read_to_string(&lock_path).unwrap(),
        "existing lock-file marker"
    );
    assert!(lock_path.is_file());
}
