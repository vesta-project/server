mod support;

use std::{fs, sync::mpsc, thread, time::Duration};
use support::{Fixture, create_directory_link, remove_directory_link};
use vesta_state::{StateError, TransactionError};

#[test]
fn successful_repository_work_is_rolled_back_when_precommit_paths_become_linked() {
    let fixture = Fixture::open();
    let backups = fixture.db.directory().backups_dir().to_path_buf();
    let saved = fixture.root.path().join("original-backups");
    let outside = fixture.root.path().join("untrusted-backups");
    fs::create_dir(&outside).unwrap();
    assert!(fs::read_dir(&backups).unwrap().next().is_none());

    let result = fixture
        .db
        .write_with(|transaction| -> Result<(), StateError> {
            transaction.connection().execute(
                "INSERT INTO meta VALUES ('precommit-refusal', 'must-not-persist')",
                [],
            )?;
            fs::rename(&backups, &saved)?;
            create_directory_link(&backups, &outside)?;
            Ok(())
        });

    // Restore fixture paths before assertions, including when link creation failed.
    if saved.exists() {
        if fs::symlink_metadata(&backups).is_ok() {
            remove_directory_link(&backups).unwrap();
        }
        fs::rename(&saved, &backups).unwrap();
    }
    match result {
        Err(TransactionError::State(error)) => assert_eq!(error.code(), "state_file_linked"),
        other => panic!("expected precommit path refusal, got {other:?}"),
    }
    let count: i64 = fixture
        .db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM meta WHERE key='precommit-refusal'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(count, 0);
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
    fixture
        .db
        .write(|connection| {
            connection.execute("INSERT INTO meta VALUES ('after-path-refusal', 'ok')", [])?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn typed_snapshot_is_fixed_before_repository_queries_and_does_not_block_writer() {
    let fixture = Fixture::open();
    fixture
        .db
        .write(|connection| {
            connection.execute("INSERT INTO meta VALUES ('typed-snapshot', 'before')", [])?;
            Ok(())
        })
        .unwrap();
    let reader_db = fixture.db.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        reader_db.read_with(|snapshot| -> Result<String, StateError> {
            // No repository SELECT has executed when the writer is admitted.
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(format!("{snapshot:?}"), "ReadTransaction { .. }");
            Ok(snapshot.connection().query_row(
                "SELECT value FROM meta WHERE key='typed-snapshot'",
                [],
                |row| row.get(0),
            )?)
        })
    });
    if entered_rx.recv_timeout(Duration::from_secs(10)).is_err() {
        let _ = release_tx.send(());
        let _ = reader.join();
        panic!("reader did not enter its established snapshot");
    }
    let write_result = fixture
        .db
        .write_with(|transaction| -> Result<(), StateError> {
            assert_eq!(format!("{transaction:?}"), "WriteTransaction { .. }");
            transaction.connection().execute(
                "UPDATE meta SET value='after' WHERE key='typed-snapshot'",
                [],
            )?;
            Ok(())
        });
    let _ = release_tx.send(());
    let old_value = reader.join().expect("reader completes").unwrap();
    write_result.unwrap();
    assert_eq!(old_value, "before");
    let current: String = fixture
        .db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT value FROM meta WHERE key='typed-snapshot'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(current, "after");
}

#[test]
fn ending_a_typed_read_transaction_inside_repository_is_rejected() {
    let fixture = Fixture::open();
    let result = fixture.db.read_with(|snapshot| -> Result<(), StateError> {
        // Deliberate fault injection, never a supported repository operation.
        snapshot.connection().execute_batch("ROLLBACK")?;
        Ok(())
    });
    match result {
        Err(TransactionError::State(error)) => assert_eq!(error.code(), "state_transaction_ended"),
        other => panic!("expected ended-transaction refusal, got {other:?}"),
    }
    assert_eq!(
        fixture.db.schema_version().unwrap(),
        vesta_state::CURRENT_SCHEMA_VERSION
    );
    fixture.db.close().unwrap();
}
