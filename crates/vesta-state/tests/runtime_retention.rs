mod support;

use rusqlite::OptionalExtension;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use support::{DEFAULT_TEST_TIME_MS, Fixture, TestClock};
use vesta_state::{Clock, RuntimeLease, StateDatabase, StateDirectory, StateError};

fn assert_error_code<T>(result: vesta_state::Result<T>, expected: &str) {
    match result {
        Ok(_) => panic!("expected state error {expected}"),
        Err(error) => assert_eq!(error.code(), expected),
    }
}

fn assert_is_send<T: Send>() {}

#[test]
fn cloned_handles_share_multiple_leases_and_busy_close_preserves_outcome_writes() {
    assert_is_send::<RuntimeLease>();

    let fixture = Fixture::open();
    let db = fixture.db.clone();
    let first = fixture.db.retain_runtime().unwrap();
    let second = db.retain_runtime().unwrap();

    let debug = format!("{first:?}");
    assert!(debug.starts_with("RuntimeLease"));
    assert!(!debug.contains(fixture.state_path().to_string_lossy().as_ref()));
    assert!(!debug.contains(fixture.db.server_id()));

    assert_error_code(fixture.db.close(), "state_runtime_busy");
    assert_eq!(
        fixture.db.schema_version().unwrap(),
        vesta_state::CURRENT_SCHEMA_VERSION
    );

    fixture
        .db
        .write(|connection| {
            connection.execute(
                "INSERT INTO operations (
                    id,server_id,actor_principal_id,device_id,source,type,target,payload_hash,
                    state,requires_confirmation,external_effect_idempotent,created_at,updated_at
                 ) VALUES ('runtime-outcome',?1,'fixture-principal','fixture-device','runtime',
                    'fixture.work','fixture-target','fixture-hash','running',0,0,?2,?2)",
                rusqlite::params![fixture.db.server_id(), fixture.clock.millis()],
            )?;
            Ok(())
        })
        .unwrap();
    fixture
        .db
        .write(|connection| {
            connection.execute(
                "UPDATE operations SET state='succeeded',result_json='runtime-ok',finished_at=?1,updated_at=?1 \
                 WHERE id='runtime-outcome'",
                [fixture.clock.millis()],
            )?;
            Ok(())
        })
        .unwrap();
    let outcome: (String, String, Option<String>) = fixture
        .db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT state,result_json,error_code FROM operations WHERE id='runtime-outcome'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(outcome, ("succeeded".into(), "runtime-ok".into(), None));

    drop(first);
    assert_error_code(fixture.db.close(), "state_runtime_busy");
    assert_eq!(
        fixture.db.schema_version().unwrap(),
        vesta_state::CURRENT_SCHEMA_VERSION
    );
    drop(second);
    fixture.db.close().unwrap();
    assert_error_code(fixture.db.read(|_| Ok(())), "state_closed");
}

#[test]
fn runtime_lease_can_move_to_another_thread_and_drop_there() {
    let fixture = Fixture::open();
    let lease = fixture.db.retain_runtime().unwrap();
    let worker_db = fixture.db.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _lease = lease;
        entered_tx.send(()).unwrap();
        release_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("main thread releases the moved runtime lease");
        worker_db
            .write(|connection| {
                connection.execute(
                    "INSERT INTO meta(key,value) VALUES ('cross-thread-runtime','finished')",
                    [],
                )?;
                Ok(())
            })
            .expect("lease owner can persist its final runtime state");
    });
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker owns the moved lease");

    assert_error_code(fixture.db.close(), "state_runtime_busy");
    fixture
        .db
        .read(|connection| {
            let value: String = connection
                .query_row(
                    "SELECT value FROM meta WHERE key='cross-thread-runtime'",
                    [],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or_default();
            assert_eq!(value, "");
            Ok(())
        })
        .unwrap();
    release_tx.send(()).unwrap();
    worker.join().expect("lease-owning worker completes");

    let persisted: String = fixture
        .db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT value FROM meta WHERE key='cross-thread-runtime'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(persisted, "finished");
    fixture.db.close().unwrap();
}

#[test]
fn unwinding_drops_the_lease_and_restores_ordinary_close() {
    let fixture = Fixture::open();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _lease = fixture.db.retain_runtime().unwrap();
        panic!("simulated runtime unwind");
    }));
    assert!(result.is_err());
    fixture.db.close().unwrap();
    assert_error_code(fixture.db.read(|_| Ok(())), "state_closed");
}

#[test]
fn retain_rejects_closed_and_nested_calls_and_closing_phase() {
    let fixture = Fixture::open();
    assert_error_code(
        fixture.db.read(|_| fixture.db.retain_runtime().map(drop)),
        "state_nested_call",
    );
    assert_error_code(
        fixture.db.write(|_| fixture.db.retain_runtime().map(drop)),
        "state_nested_call",
    );

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let reader_db = fixture.db.clone();
    let reader = thread::spawn(move || {
        reader_db.read(|_| {
            entered_tx.send(()).unwrap();
            release_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| StateError::new("state_fixture_release_timeout"))?;
            Ok(())
        })
    });
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("read callback is active before close begins");

    let close_db = fixture.db.clone();
    let (close_started_tx, close_started_rx) = mpsc::channel();
    let closer = thread::spawn(move || {
        close_started_tx.send(()).unwrap();
        close_db.close()
    });
    close_started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("closer is about to wait for the active read");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let closed_admission = fixture.db.read(|_| Ok(()));
        if let Err(error) = closed_admission {
            assert_eq!(error.code(), "state_closed");
            assert_error_code(fixture.db.retain_runtime(), "state_closed");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "close should enter Closing while the read remains active"
        );
        thread::yield_now();
    }

    release_tx.send(()).unwrap();
    reader
        .join()
        .expect("blocked reader thread completes")
        .unwrap();
    closer.join().expect("closer thread completes").unwrap();
    assert_error_code(fixture.db.retain_runtime(), "state_closed");
}

#[test]
fn close_and_retain_race_only_produces_the_two_valid_linearized_outcomes() {
    for _ in 0..32 {
        let temp = tempfile::tempdir().expect("create local temporary test root");
        let db = StateDatabase::open(
            StateDirectory::new(temp.path().join("state"), None).unwrap(),
            Arc::new(TestClock::new(DEFAULT_TEST_TIME_MS)),
        )
        .unwrap();
        let start = Arc::new(Barrier::new(3));
        let retain_db = db.clone();
        let retain_start = Arc::clone(&start);
        let retainer = thread::spawn(move || {
            retain_start.wait();
            retain_db.retain_runtime()
        });
        let close_db = db.clone();
        let close_start = Arc::clone(&start);
        let closer = thread::spawn(move || {
            close_start.wait();
            close_db.close()
        });
        start.wait();

        let retain_result = retainer.join().expect("retainer thread completes");
        let close_result = closer.join().expect("closer thread completes");
        match (retain_result, close_result) {
            (Ok(lease), Err(error)) => {
                assert_eq!(error.code(), "state_runtime_busy");
                assert_eq!(
                    db.schema_version().unwrap(),
                    vesta_state::CURRENT_SCHEMA_VERSION
                );
                drop(lease);
                db.close().unwrap();
            }
            (Err(error), Ok(())) => {
                assert_eq!(error.code(), "state_closed");
                assert_error_code(db.read(|_| Ok(())), "state_closed");
                db.close().unwrap();
            }
            (retain, close) => panic!(
                "unexpected close/retain race result: retain={retain:?}, close={:?}",
                close.map_err(|error| error.code().to_owned())
            ),
        }
    }
}
