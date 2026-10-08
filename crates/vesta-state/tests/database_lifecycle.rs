mod support;

use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::Duration;
use support::{DEFAULT_TEST_TIME_MS, TestClock};
use vesta_state::audit::{AuditActor, AuditLog, AuditRecord};
use vesta_state::{Result as StateResult, StateDatabase, StateDirectory, StateError};

fn assert_error_code<T>(result: StateResult<T>, expected: &str) {
    match result {
        Ok(_) => panic!("expected state error {expected}"),
        Err(error) => assert_eq!(error.code(), expected),
    }
}

#[test]
fn concurrent_connections_keep_companions_stable_and_reopen_with_all_audited_writes() {
    let temp = tempfile::tempdir().expect("create local temporary test root");
    let state_path = temp.path().join("private state");
    let game_path = temp.path().join("game root");
    let clock = Arc::new(TestClock::new(DEFAULT_TEST_TIME_MS));
    let audit_record = AuditRecord::of(
        AuditActor::system("fixture".to_owned()).unwrap(),
        "fixture.increment".to_owned(),
        "counter".to_owned(),
        "ok".to_owned(),
        None,
    )
    .unwrap();
    let mut stable_server_id: Option<String> = None;

    for round in 0..3 {
        let directory = StateDirectory::new(&state_path, Some(&game_path)).unwrap();
        let db = StateDatabase::open(directory, clock.clone()).unwrap();
        if let Some(expected) = &stable_server_id {
            assert_eq!(db.server_id(), expected);
        } else {
            stable_server_id = Some(db.server_id().to_owned());
        }
        let audit = AuditLog::new(db.clone()).unwrap();

        if round == 0 {
            db.write(|connection| {
                connection.execute_batch(
                    "CREATE TABLE fixture_counter (
                         id INTEGER PRIMARY KEY,
                         value INTEGER NOT NULL
                     ) STRICT;
                     INSERT INTO fixture_counter VALUES (1, 0);",
                )?;
                Ok(())
            })
            .unwrap();
        }

        let database_file = db.directory().database_file();
        let wal_path = database_file.with_file_name("vesta-state.db-wal");
        let shm_path = database_file.with_file_name("vesta-state.db-shm");
        let start = Arc::new(Barrier::new(7));

        thread::scope(|scope| {
            let mut workers = Vec::with_capacity(6);
            for worker in 0..6 {
                let worker_db = db.clone();
                let worker_start = Arc::clone(&start);
                let worker_audit = audit.clone();
                let worker_record = audit_record.clone();
                let wal_path = wal_path.clone();
                let shm_path = shm_path.clone();
                workers.push(scope.spawn(move || {
                    let writer = worker % 2 == 0;
                    worker_start.wait();
                    for _ in 0..60 {
                        if writer {
                            worker_db
                                .write(|connection| {
                                    connection.execute(
                                        "UPDATE fixture_counter SET value = value + 1 WHERE id = 1",
                                        [],
                                    )?;
                                    worker_audit.append(connection, &worker_record)?;
                                    Ok(())
                                })
                                .expect("atomic counter and audit write");
                        } else {
                            worker_db
                                .read(|connection| {
                                    let value: i64 = connection.query_row(
                                        "SELECT value FROM fixture_counter WHERE id = 1",
                                        [],
                                        |row| row.get(0),
                                    )?;
                                    assert!(value >= 0);
                                    Ok(())
                                })
                                .expect("concurrent counter snapshot");
                        }
                        assert!(wal_path.is_file(), "WAL remains while the anchor is open");
                        assert!(shm_path.is_file(), "SHM remains while the anchor is open");
                    }
                }));
            }
            start.wait();
            for worker in workers {
                worker.join().expect("concurrent database worker completed");
            }
        });

        let expected = i64::from((round + 1) * 180);
        let actual = db
            .read(|connection| {
                Ok(connection.query_row(
                    "SELECT value FROM fixture_counter WHERE id = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .unwrap();
        assert_eq!(actual, expected);
        let audited = db
            .read(|connection| {
                Ok(connection.query_row(
                    "SELECT COUNT(*) FROM audit_events WHERE action = 'fixture.increment'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .unwrap();
        assert_eq!(audited, expected);
        assert!(audit.verify_chain().unwrap().valid);

        db.close().unwrap();
        assert!(!wal_path.exists());
        assert!(!shm_path.exists());
    }
}

#[test]
fn close_waits_for_an_in_flight_snapshot_then_rejects_all_new_calls() {
    let temp = tempfile::tempdir().expect("create local temporary test root");
    let clock = Arc::new(TestClock::new(DEFAULT_TEST_TIME_MS));
    let db = StateDatabase::open(
        StateDirectory::new(temp.path().join("state"), None).unwrap(),
        clock,
    )
    .unwrap();
    let expected_server_id = db.server_id().to_owned();

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (reader_tx, reader_rx) = mpsc::channel();
    let reader_db = db.clone();
    let reader = thread::spawn(move || {
        let result = reader_db.read(|connection| {
            let server_id: String =
                connection.query_row("SELECT server_id FROM server_identity", [], |row| {
                    row.get(0)
                })?;
            entered_tx
                .send(())
                .map_err(|_| StateError::new("state_fixture_reader_signal_failed"))?;
            release_rx
                .recv_timeout(Duration::from_secs(8))
                .map_err(|_| StateError::new("state_fixture_reader_release_timeout"))?;
            Ok(server_id)
        });
        let _ = reader_tx.send(result);
    });

    if entered_rx.recv_timeout(Duration::from_secs(5)).is_err() {
        let _ = release_tx.send(());
        let _ = reader.join();
        panic!("read callback did not enter within five seconds");
    }

    let (close_started_tx, close_started_rx) = mpsc::channel();
    let (close_done_tx, close_done_rx) = mpsc::channel();
    let close_db = db.clone();
    let closer = thread::spawn(move || {
        close_started_tx.send(()).unwrap();
        let closed = close_db.close().is_ok();
        close_done_tx.send(closed).unwrap();
    });
    assert!(
        close_started_rx
            .recv_timeout(Duration::from_secs(5))
            .is_ok()
    );
    let close_before_release = close_done_rx.recv_timeout(Duration::from_millis(150));

    // Always release the callback before asserting so a failed expectation cannot
    // strand either worker or leave the temporary database open.
    let _ = release_tx.send(());
    let reader_result = reader_rx.recv_timeout(Duration::from_secs(5));
    let close_after_release = close_done_rx.recv_timeout(Duration::from_secs(5));
    reader.join().expect("reader thread completed");
    closer.join().expect("close thread completed");

    assert!(
        matches!(close_before_release, Err(mpsc::RecvTimeoutError::Timeout)),
        "close must wait while a read callback is running"
    );
    assert_eq!(reader_result.unwrap().unwrap(), expected_server_id);
    assert!(
        close_after_release.unwrap(),
        "close completes after the read exits"
    );
    assert_error_code(db.read(|_| Ok(())), "state_closed");
    assert_error_code(db.write(|_| Ok(())), "state_closed");
    db.close().unwrap();
}

#[test]
fn closing_inside_read_or_write_callback_is_rejected_without_closing_database() {
    let temp = tempfile::tempdir().expect("create local temporary test root");
    let db = StateDatabase::open(
        StateDirectory::new(temp.path().join("state"), None).unwrap(),
        Arc::new(TestClock::new(DEFAULT_TEST_TIME_MS)),
    )
    .unwrap();

    assert_error_code(db.read(|_| db.close()), "state_close_in_callback");
    assert_error_code(db.write(|_| db.close()), "state_close_in_callback");
    assert_eq!(
        db.schema_version().unwrap(),
        vesta_state::CURRENT_SCHEMA_VERSION
    );
    let server_id = db.server_id().to_owned();
    let read_server_id = db
        .read(|connection| {
            connection
                .query_row("SELECT server_id FROM server_identity", [], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(read_server_id, server_id);
    db.close().unwrap();
}
