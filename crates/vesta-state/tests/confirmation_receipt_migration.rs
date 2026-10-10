mod support;

use rusqlite::{Connection, params};
use std::fs;
use support::{DEFAULT_TEST_TIME_MS, Fixture};
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};

type LegacyConfirmationRow = (String, String, String, i64, i64, Option<i64>);

fn legacy_confirmation_rows(
    connection: &Connection,
) -> rusqlite::Result<Vec<LegacyConfirmationRow>> {
    let mut statement = connection.prepare(
        "SELECT id,operation_id,token_hash,created_at,expires_at,consumed_at \
             FROM confirmations ORDER BY id",
    )?;
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?
        .collect()
}

fn has_column(connection: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(columns.iter().any(|name| name == column))
}

#[derive(Clone, Copy)]
struct ReceiptTimes {
    created_at: i64,
    expires_at: i64,
    consumed_at: Option<i64>,
    accepted_at: Option<i64>,
}

fn insert_confirmation(
    connection: &Connection,
    id: &str,
    operation_id: &str,
    token_hash: &str,
    times: ReceiptTimes,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO confirmations (
            id,server_id,operation_id,actor_principal_id,device_id,action,target,payload_hash,
            source_revision,token_hash,created_at,expires_at,consumed_at,accepted_at
         ) VALUES (?1,?2,?3,'fixture-principal','fixture-device','fixture.action','fixture-target',
            'fixture-payload','fixture-revision',?4,?5,?6,?7,?8)",
        params![
            id,
            connection.query_row(
                "SELECT server_id FROM server_identity WHERE singleton=1",
                [],
                |row| row.get::<_, String>(0),
            )?,
            operation_id,
            token_hash,
            times.created_at,
            times.expires_at,
            times.consumed_at,
            times.accepted_at,
        ],
    )?;
    Ok(())
}

fn assert_state_error<T>(result: vesta_state::Result<T>, expected: &str) {
    match result {
        Ok(_) => panic!("expected state error {expected}"),
        Err(error) => assert_eq!(error.code(), expected),
    }
}

#[test]
fn genuine_v5_migration_preserves_confirmation_rows_without_inventing_receipts() {
    let fixture = Fixture::with_time(DEFAULT_TEST_TIME_MS);
    let directory = fixture.db.directory().clone();

    fixture
        .db
        .write(|connection| {
            for (id, state, created_at) in [
                ("legacy-consumed-op", "running", 90_i64),
                ("legacy-refused-op", "failed", 95_i64),
                ("legacy-pending-op", "awaiting_confirmation", 99_i64),
            ] {
                connection.execute(
                    "INSERT INTO operations (
                        id,server_id,actor_principal_id,device_id,source,type,target,payload_hash,
                        state,requires_confirmation,external_effect_idempotent,created_at,updated_at
                     ) VALUES (?1,?2,'fixture-principal','fixture-device','https','fixture.action',
                        'fixture-target','fixture-payload',?3,1,0,?4,?4)",
                    params![id, fixture.db.server_id(), state, created_at],
                )?;
            }
            connection.execute(
                "INSERT INTO confirmations (
                    id,server_id,operation_id,actor_principal_id,device_id,action,target,payload_hash,
                    source_revision,token_hash,created_at,expires_at,consumed_at
                 ) VALUES ('legacy-consumed',?1,'legacy-consumed-op','fixture-principal','fixture-device',
                    'fixture.action','fixture-target','fixture-payload','fixture-revision',
                    'token-hash-consumed',100,200,150)",
                [fixture.db.server_id()],
            )?;
            connection.execute(
                "INSERT INTO confirmations (
                    id,server_id,operation_id,actor_principal_id,device_id,action,target,payload_hash,
                    source_revision,token_hash,created_at,expires_at,consumed_at
                 ) VALUES ('legacy-refused',?1,'legacy-refused-op','fixture-principal','fixture-device',
                    'fixture.action','fixture-target','fixture-payload','fixture-revision',
                    'token-hash-refused',110,210,180)",
                [fixture.db.server_id()],
            )?;
            connection.execute(
                "INSERT INTO confirmations (
                    id,server_id,operation_id,actor_principal_id,device_id,action,target,payload_hash,
                    source_revision,token_hash,created_at,expires_at,consumed_at
                 ) VALUES ('legacy-pending',?1,'legacy-pending-op','fixture-principal','fixture-device',
                    'fixture.action','fixture-target','fixture-payload','fixture-revision',
                    'token-hash-pending',120,220,NULL)",
                [fixture.db.server_id()],
            )?;
            Ok(())
        })
        .unwrap();

    let expected_rows = fixture
        .db
        .read(|connection| Ok(legacy_confirmation_rows(connection)?))
        .unwrap();
    assert_eq!(expected_rows.len(), 3);
    assert_eq!(expected_rows[0].5, Some(150));
    assert_eq!(expected_rows[1].5, None);
    assert_eq!(expected_rows[2].5, Some(180));

    // A real v5 fixture is derived from this owned database, offline, by changing only the
    // migration-added column and the SQLite schema marker.
    fixture.db.close().unwrap();
    let offline_fixture = Connection::open(directory.database_file()).unwrap();
    assert!(has_column(&offline_fixture, "confirmations", "accepted_at").unwrap());
    offline_fixture
        .execute_batch(
            r#"
            BEGIN IMMEDIATE;
            DROP TABLE file_intents; DROP TABLE file_uploads; DROP TABLE file_trash;
            DROP TABLE pack_releases;
            CREATE TABLE confirmations_v5 (
                id TEXT PRIMARY KEY,
                server_id TEXT NOT NULL REFERENCES server_identity(server_id),
                operation_id TEXT NOT NULL REFERENCES operations(id),
                actor_principal_id TEXT NOT NULL,
                device_id TEXT NOT NULL,
                action TEXT NOT NULL,
                target TEXT NOT NULL,
                payload_hash TEXT NOT NULL,
                source_revision TEXT NOT NULL,
                token_hash TEXT NOT NULL UNIQUE,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                consumed_at INTEGER
            ) STRICT;
            INSERT INTO confirmations_v5 (
                id,server_id,operation_id,actor_principal_id,device_id,action,target,payload_hash,
                source_revision,token_hash,created_at,expires_at,consumed_at
            ) SELECT id,server_id,operation_id,actor_principal_id,device_id,action,target,payload_hash,
                source_revision,token_hash,created_at,expires_at,consumed_at FROM confirmations;
            DROP TABLE confirmations;
            ALTER TABLE confirmations_v5 RENAME TO confirmations;
            CREATE INDEX confirmations_operation ON confirmations(operation_id);
            PRAGMA user_version=5;
            COMMIT;
            "#,
        )
        .unwrap();
    assert!(!has_column(&offline_fixture, "confirmations", "accepted_at").unwrap());
    assert_eq!(
        offline_fixture
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        5
    );
    assert_eq!(
        legacy_confirmation_rows(&offline_fixture).unwrap(),
        expected_rows
    );
    drop(offline_fixture);

    let migrated = StateDatabase::open(directory.clone(), fixture.clock.clone()).unwrap();
    assert_eq!(migrated.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    let (migrated_rows, accepted_values) = migrated
        .read(|connection| {
            let mut statement = connection.prepare(
                "SELECT id,operation_id,token_hash,created_at,expires_at,consumed_at,accepted_at \
                 FROM confirmations ORDER BY id",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        (
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, Option<i64>>(5)?,
                        ),
                        row.get::<_, Option<i64>>(6)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((
                rows.iter().map(|(row, _)| row.clone()).collect::<Vec<_>>(),
                rows.into_iter()
                    .map(|(_, accepted_at)| accepted_at)
                    .collect::<Vec<_>>(),
            ))
        })
        .unwrap();
    assert_eq!(migrated_rows, expected_rows);
    assert_eq!(accepted_values, vec![None, None, None]);

    migrated.close().unwrap();
    let backups = fs::read_dir(directory.backups_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    let backup = Connection::open(&backups[0]).unwrap();
    assert_eq!(
        backup
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        5
    );
    assert_eq!(
        backup
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(!has_column(&backup, "confirmations", "accepted_at").unwrap());
    assert_eq!(legacy_confirmation_rows(&backup).unwrap(), expected_rows);
}

#[test]
fn accepted_at_check_constraints_and_operation_outcome_share_real_transactions() {
    let fixture = Fixture::open();
    let db = &fixture.db;
    db.write(|connection| {
        connection.execute(
            "INSERT INTO operations (
                id,server_id,actor_principal_id,device_id,source,type,target,payload_hash,
                state,requires_confirmation,external_effect_idempotent,created_at,updated_at
             ) VALUES ('receipt-operation',?1,'fixture-principal','fixture-device','https',
                'fixture.action','fixture-target','fixture-payload','awaiting_confirmation',1,0,90,90)",
            [db.server_id()],
        )?;
        Ok(())
    })
    .unwrap();

    for (id, token, times) in [
        (
            "receipt-no-consume",
            "token-no-consume",
            ReceiptTimes {
                created_at: 100,
                expires_at: 200,
                consumed_at: None,
                accepted_at: Some(150),
            },
        ),
        (
            "receipt-mismatch",
            "token-mismatch",
            ReceiptTimes {
                created_at: 100,
                expires_at: 200,
                consumed_at: Some(151),
                accepted_at: Some(150),
            },
        ),
        (
            "receipt-before-created",
            "token-before",
            ReceiptTimes {
                created_at: 100,
                expires_at: 200,
                consumed_at: Some(99),
                accepted_at: Some(99),
            },
        ),
        (
            "receipt-at-expiry",
            "token-expiry",
            ReceiptTimes {
                created_at: 100,
                expires_at: 200,
                consumed_at: Some(200),
                accepted_at: Some(200),
            },
        ),
    ] {
        let result = db.write(|connection| {
            insert_confirmation(connection, id, "receipt-operation", token, times)?;
            Ok(())
        });
        assert_state_error(result, "state_io");
    }
    let accepted_count: i64 = db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM confirmations WHERE accepted_at IS NOT NULL",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(accepted_count, 0);

    // The invalid receipt check must roll back the preceding outcome mutation in the same write.
    let failed_transaction = db.write(|connection| {
        connection.execute(
            "UPDATE operations SET state='succeeded',result_json='must-rollback',finished_at=150 \
             WHERE id='receipt-operation'",
            [],
        )?;
        insert_confirmation(
            connection,
            "receipt-atomic-failure",
            "receipt-operation",
            "token-atomic-failure",
            ReceiptTimes {
                created_at: 100,
                expires_at: 200,
                consumed_at: None,
                accepted_at: Some(150),
            },
        )?;
        Ok(())
    });
    assert_state_error(failed_transaction, "state_io");
    let after_rollback: (String, Option<String>, i64) = db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT state,result_json,
                    (SELECT count(*) FROM confirmations WHERE id='receipt-atomic-failure') \
                 FROM operations WHERE id='receipt-operation'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(after_rollback, ("awaiting_confirmation".into(), None, 0));

    // Matching consumed/accepted timestamps inside [created_at, expires_at) commit together
    // with the operation outcome on the same SQLite transaction.
    db.write(|connection| {
        connection.execute(
            "UPDATE operations SET state='succeeded',result_json='accepted',finished_at=150,updated_at=150 \
             WHERE id='receipt-operation'",
            [],
        )?;
        insert_confirmation(
            connection,
            "receipt-valid",
            "receipt-operation",
            "token-valid",
            ReceiptTimes {
                created_at: 100,
                expires_at: 200,
                consumed_at: Some(150),
                accepted_at: Some(150),
            },
        )?;
        Ok(())
    })
    .unwrap();
    let committed: (String, Option<String>, Option<i64>, Option<i64>) = db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT operations.state,operations.result_json,confirmations.consumed_at,confirmations.accepted_at \
                 FROM operations JOIN confirmations ON confirmations.operation_id=operations.id \
                 WHERE operations.id='receipt-operation' AND confirmations.id='receipt-valid'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        committed,
        (
            "succeeded".into(),
            Some("accepted".into()),
            Some(150),
            Some(150)
        )
    );
}
