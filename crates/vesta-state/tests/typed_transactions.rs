mod support;

use std::fmt;
use support::Fixture;
use vesta_state::{
    StateDatabase, StateError, TransactionError,
    audit::{AuditActor, AuditLog, AuditRecord},
};

#[derive(Clone, Eq, PartialEq)]
enum DomainRefusal {
    Rejected(&'static str),
    Storage,
}

fn scalar(db: &StateDatabase, sql: &str) -> i64 {
    db.read(|connection| Ok(connection.query_row(sql, [], |row| row.get(0))?))
        .expect("read scalar from real SQLite database")
}

fn create_item_table(fixture: &Fixture) {
    fixture
        .db
        .write(|connection| {
            connection.execute(
                "CREATE TABLE typed_transaction_items (value TEXT PRIMARY KEY)",
                [],
            )?;
            Ok(())
        })
        .expect("create typed transaction test table");
}

fn audit_record(action: &str, target: &str) -> AuditRecord {
    AuditRecord::of(
        AuditActor::system("typed-transaction-test".into()).expect("valid system audit actor"),
        action.into(),
        target.into(),
        "ok".into(),
        None,
    )
    .expect("valid audit record")
}

#[test]
fn typed_write_preserves_non_state_refusal_and_rolls_back_mutation() {
    let fixture = Fixture::open();
    create_item_table(&fixture);

    let error = fixture
        .db
        .write_with(|transaction| -> Result<(), DomainRefusal> {
            transaction
                .connection()
                .execute(
                    "INSERT INTO typed_transaction_items(value) VALUES ('refused')",
                    [],
                )
                .map_err(|_| DomainRefusal::Storage)?;
            Err(DomainRefusal::Rejected("policy-conflict-17"))
        })
        .unwrap_err();
    assert!(matches!(
        error,
        TransactionError::Work(DomainRefusal::Rejected("policy-conflict-17"))
    ));
    assert_eq!(
        scalar(
            &fixture.db,
            "SELECT COUNT(*) FROM typed_transaction_items WHERE value = 'refused'"
        ),
        0,
        "the typed refusal must roll back its preceding mutation"
    );

    fixture
        .db
        .write_with(|transaction| -> Result<(), DomainRefusal> {
            transaction
                .connection()
                .execute(
                    "INSERT INTO typed_transaction_items(value) VALUES ('accepted')",
                    [],
                )
                .map_err(|_| DomainRefusal::Storage)?;
            Ok(())
        })
        .expect("a later FIFO-admitted writer remains usable");
    assert_eq!(
        scalar(
            &fixture.db,
            "SELECT COUNT(*) FROM typed_transaction_items WHERE value = 'accepted'"
        ),
        1
    );
}

#[test]
fn typed_read_preserves_repository_refusal_and_leaves_persisted_value_unchanged() {
    let fixture = Fixture::open();
    fixture
        .db
        .write(|connection| {
            connection.execute(
                "INSERT INTO meta(key, value) VALUES ('read-refusal', 'kept')",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let error = fixture
        .db
        .read_with(|snapshot| -> Result<String, DomainRefusal> {
            let value: String = snapshot
                .connection()
                .query_row(
                    "SELECT value FROM meta WHERE key = 'read-refusal'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|_| DomainRefusal::Storage)?;
            assert_eq!(value, "kept");
            Err(DomainRefusal::Rejected("typed-read-refusal"))
        })
        .unwrap_err();
    assert!(matches!(
        error,
        TransactionError::Work(DomainRefusal::Rejected("typed-read-refusal"))
    ));
    assert_eq!(
        scalar(
            &fixture.db,
            "SELECT COUNT(*) FROM meta WHERE key = 'read-refusal' AND value = 'kept'"
        ),
        1,
        "a read refusal does not disturb the committed row"
    );
}

#[test]
fn callback_panic_rolls_back_and_leaves_writer_and_close_usable() {
    let fixture = Fixture::open();
    create_item_table(&fixture);

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<(), TransactionError<DomainRefusal>> =
            fixture
                .db
                .write_with(|transaction| -> Result<(), DomainRefusal> {
                    transaction
                        .connection()
                        .execute(
                            "INSERT INTO typed_transaction_items(value) VALUES ('panic')",
                            [],
                        )
                        .unwrap();
                    panic!("injected typed-transaction callback panic");
                });
    }));
    assert!(
        panic.is_err(),
        "the callback panic must propagate to the caller"
    );
    assert_eq!(
        scalar(
            &fixture.db,
            "SELECT COUNT(*) FROM typed_transaction_items WHERE value = 'panic'"
        ),
        0,
        "the unwound transaction must not persist its mutation"
    );

    fixture
        .db
        .write_with(|transaction| -> Result<(), DomainRefusal> {
            transaction
                .connection()
                .execute(
                    "INSERT INTO typed_transaction_items(value) VALUES ('after-panic')",
                    [],
                )
                .map_err(|_| DomainRefusal::Storage)?;
            Ok(())
        })
        .expect("the FIFO writer gate and connection lifecycle recover after unwind");
    assert_eq!(
        scalar(
            &fixture.db,
            "SELECT COUNT(*) FROM typed_transaction_items WHERE value = 'after-panic'"
        ),
        1
    );
    fixture.db.close().expect("close after the unwound writer");
    assert!(matches!(
        fixture.db.write_with(|_| Ok::<(), DomainRefusal>(())),
        Err(TransactionError::State(error)) if error.code() == "state_closed"
    ));
}

#[test]
fn audit_append_shares_mutation_transaction_and_trigger_failure_rolls_both_back() {
    let fixture = Fixture::open();
    create_item_table(&fixture);
    let audit = AuditLog::new(fixture.db.clone()).expect("valid empty audit chain");

    fixture
        .db
        .write_with(|transaction| -> Result<(), StateError> {
            let connection = transaction.connection();
            connection.execute(
                "INSERT INTO typed_transaction_items(value) VALUES ('audited-success')",
                [],
            )?;
            audit.append(connection, &audit_record("typed.write", "audited-success"))?;
            Ok(())
        })
        .expect("mutation and audit append commit together");
    assert_eq!(
        scalar(
            &fixture.db,
            "SELECT COUNT(*) FROM typed_transaction_items WHERE value = 'audited-success'"
        ),
        1
    );
    assert_eq!(scalar(&fixture.db, "SELECT COUNT(*) FROM audit_events"), 1);
    assert!(audit.verify_chain().expect("verify committed audit").valid);

    fixture
        .db
        .write(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER reject_typed_audit BEFORE INSERT ON audit_events \
                 BEGIN SELECT RAISE(ABORT, 'injected audit insert failure'); END;",
            )?;
            Ok(())
        })
        .expect("install a real SQLite audit failure trigger");

    let failed = fixture
        .db
        .write_with(|transaction| -> Result<(), StateError> {
            let connection = transaction.connection();
            connection.execute(
                "INSERT INTO typed_transaction_items(value) VALUES ('audited-failure')",
                [],
            )?;
            audit.append(connection, &audit_record("typed.write", "audited-failure"))?;
            Ok(())
        });
    assert!(matches!(
        failed,
        Err(TransactionError::Work(error)) if error.code() == "state_io"
    ));
    assert_eq!(
        scalar(
            &fixture.db,
            "SELECT COUNT(*) FROM typed_transaction_items WHERE value = 'audited-failure'"
        ),
        0,
        "audit insertion failure rolls back the preceding mutation"
    );
    assert_eq!(scalar(&fixture.db, "SELECT COUNT(*) FROM audit_events"), 1);
    assert!(
        audit
            .verify_chain()
            .expect("verify retained audit row")
            .valid
    );
}

#[test]
fn deferred_foreign_key_commit_failure_rolls_back_and_allows_a_later_valid_write() {
    let fixture = Fixture::open();
    fixture
        .db
        .write(|connection| {
            connection.execute_batch(
                "CREATE TABLE typed_tx_parent (id INTEGER PRIMARY KEY);\
                 CREATE TABLE typed_tx_child (\
                     parent_id INTEGER NOT NULL,\
                     FOREIGN KEY(parent_id) REFERENCES typed_tx_parent(id)\
                         DEFERRABLE INITIALLY DEFERRED\
                 );",
            )?;
            Ok(())
        })
        .expect("create tables with a deferred foreign-key constraint");

    let failed = fixture
        .db
        .write_with(|transaction| -> Result<(), DomainRefusal> {
            transaction
                .connection()
                .execute("INSERT INTO typed_tx_child(parent_id) VALUES (404)", [])
                .map_err(|_| DomainRefusal::Storage)?;
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(
        failed,
        TransactionError::State(error) if error.code() == "state_commit_failed"
    ));
    assert_eq!(
        scalar(&fixture.db, "SELECT COUNT(*) FROM typed_tx_child"),
        0,
        "a deferred-FK COMMIT failure must be explicitly rolled back"
    );

    fixture
        .db
        .write_with(|transaction| -> Result<(), DomainRefusal> {
            transaction
                .connection()
                .execute("INSERT INTO typed_tx_parent(id) VALUES (404)", [])
                .map_err(|_| DomainRefusal::Storage)?;
            transaction
                .connection()
                .execute("INSERT INTO typed_tx_child(parent_id) VALUES (404)", [])
                .map_err(|_| DomainRefusal::Storage)?;
            Ok(())
        })
        .expect("a valid transaction succeeds after failed deferred-FK commit");
    assert_eq!(
        scalar(&fixture.db, "SELECT COUNT(*) FROM typed_tx_child"),
        1
    );
}

#[test]
fn rollback_failure_retains_typed_primary_error_and_safe_failure_code() {
    let fixture = Fixture::open();
    create_item_table(&fixture);

    // Fault injection only: repository callbacks must never issue transaction-control SQL.
    // Ending this transaction early makes the database-owned rollback fail deterministically.
    let error = fixture
        .db
        .write_with(|transaction| -> Result<(), DomainRefusal> {
            let connection = transaction.connection();
            connection
                .execute(
                    "INSERT INTO typed_transaction_items(value) VALUES ('rollback-fault')",
                    [],
                )
                .map_err(|_| DomainRefusal::Storage)?;
            connection
                .execute_batch("ROLLBACK")
                .map_err(|_| DomainRefusal::Storage)?;
            Err(DomainRefusal::Rejected("original-domain-refusal"))
        })
        .unwrap_err();

    match error {
        TransactionError::Rollback { primary, failure } => {
            assert!(matches!(
                *primary,
                TransactionError::Work(DomainRefusal::Rejected("original-domain-refusal"))
            ));
            assert_eq!(failure.code(), "state_rollback_failed");
        }
        _ => panic!("expected rollback failure to retain the primary work error"),
    }
    assert_eq!(
        scalar(
            &fixture.db,
            "SELECT COUNT(*) FROM typed_transaction_items WHERE value = 'rollback-fault'"
        ),
        0
    );
    fixture
        .db
        .write_with(|_| Ok::<(), DomainRefusal>(()))
        .expect("admission remains usable after rollback-failure composition");
}

struct PrivateRefusal(&'static str);

impl fmt::Debug for PrivateRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PrivateRefusal({})", self.0)
    }
}

impl fmt::Display for PrivateRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

#[test]
fn transaction_error_redacts_work_display_debug_and_source_chain() {
    const SECRET: &str = "private-domain-detail-7f1e9c";
    let work = TransactionError::Work(PrivateRefusal(SECRET));
    let work_text = format!("{work} {work:?}");
    assert!(!work_text.contains(SECRET));
    assert!(work_text.contains("<redacted>"));
    assert!(std::error::Error::source(&work).is_none());

    let rollback = TransactionError::Rollback {
        primary: Box::new(work),
        failure: StateError::new("state_rollback_failed"),
    };
    let rollback_text = format!("{rollback} {rollback:?}");
    assert!(!rollback_text.contains(SECRET));

    let mut source = std::error::Error::source(&rollback);
    assert!(
        source.is_some(),
        "the rollback retains its primary error as source"
    );
    let mut source_text = String::new();
    while let Some(error) = source {
        use fmt::Write as _;
        write!(&mut source_text, "{error} {error:?}").unwrap();
        source = error.source();
    }
    assert!(!source_text.contains(SECRET));
    assert!(source_text.contains("<redacted>"));
}

#[test]
fn typed_read_enforces_real_connection_pragmas_and_rejects_nested_calls() {
    let fixture = Fixture::open();

    fixture
        .db
        .read_with(|snapshot| -> Result<(), DomainRefusal> {
            let connection = snapshot.connection();
            let foreign_keys: i64 = connection
                .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                .map_err(|_| DomainRefusal::Storage)?;
            let query_only: i64 = connection
                .query_row("PRAGMA query_only", [], |row| row.get(0))
                .map_err(|_| DomainRefusal::Storage)?;
            let synchronous: i64 = connection
                .query_row("PRAGMA synchronous", [], |row| row.get(0))
                .map_err(|_| DomainRefusal::Storage)?;
            let busy_timeout: i64 = connection
                .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
                .map_err(|_| DomainRefusal::Storage)?;
            let journal_mode: String = connection
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .map_err(|_| DomainRefusal::Storage)?;
            assert_eq!(foreign_keys, 1);
            assert_eq!(query_only, 1);
            assert_eq!(synchronous, 2, "FULL synchronous mode");
            assert!(busy_timeout > 0);
            assert_eq!(journal_mode.to_lowercase(), "wal");

            assert!(
                connection
                    .execute(
                        "INSERT INTO meta(key, value) VALUES ('typed-read-write-attempt', 'blocked')",
                        [],
                    )
                    .is_err(),
                "the actual typed-read connection must be query-only"
            );
            let nested_write = fixture
                .db
                .write_with(|_| Ok::<(), DomainRefusal>(()));
            assert!(matches!(
                nested_write,
                Err(TransactionError::State(error)) if error.code() == "state_nested_call"
            ));
            let nested_close = fixture.db.close();
            assert!(matches!(
                nested_close,
                Err(error) if error.code() == "state_close_in_callback"
            ));
            Ok(())
        })
        .expect("query-only snapshot completes after rejecting nested operations");

    assert_eq!(
        scalar(
            &fixture.db,
            "SELECT COUNT(*) FROM meta WHERE key = 'typed-read-write-attempt'"
        ),
        0
    );
    fixture
        .db
        .write_with(|_| Ok::<(), DomainRefusal>(()))
        .expect("failed nested close did not close the database");
}
