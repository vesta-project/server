mod support;

use rusqlite::{Connection, params};
use std::{fs, sync::Arc};
use support::TestClock;
use vesta_state::{Clock, StateDatabase, StateDirectory, audit::AuditLog};

const FIXED_TIME_MS: i64 = 1_791_115_200_000; // 2026-10-04T12:00:00Z
const SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";
const LEGACY_HASH: &str = "legacy-payload-hash";

fn legacy_v1_schema() -> &'static str {
    r#"
    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
    CREATE TABLE server_identity (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        server_id TEXT NOT NULL UNIQUE,
        created_at INTEGER NOT NULL
    ) STRICT;
    CREATE TABLE operations (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        id TEXT NOT NULL UNIQUE,
        server_id TEXT NOT NULL REFERENCES server_identity(server_id),
        actor_principal_id TEXT NOT NULL,
        device_id TEXT NOT NULL,
        source TEXT NOT NULL,
        type TEXT NOT NULL,
        target TEXT NOT NULL,
        payload_hash TEXT NOT NULL,
        idempotency_key TEXT,
        state TEXT NOT NULL CHECK (state IN ('queued','validating','awaiting_confirmation','running',
            'cancelling','succeeded','failed','cancelled','recovery_required')),
        requires_confirmation INTEGER NOT NULL CHECK (requires_confirmation IN (0,1)),
        progress_percent INTEGER NOT NULL DEFAULT 0 CHECK (progress_percent BETWEEN 0 AND 100),
        progress_message TEXT,
        cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0,1)),
        external_effect_started INTEGER NOT NULL DEFAULT 0 CHECK (external_effect_started IN (0,1)),
        external_effect_idempotent INTEGER NOT NULL CHECK (external_effect_idempotent IN (0,1)),
        result_json TEXT,
        error_code TEXT,
        error_message TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        finished_at INTEGER,
        UNIQUE (server_id, actor_principal_id, idempotency_key)
    ) STRICT;
    CREATE INDEX operations_state_seq ON operations(state, seq);
    CREATE TABLE operation_resources (
        operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
        resource TEXT NOT NULL,
        PRIMARY KEY (operation_id, resource)
    ) STRICT;
    CREATE TABLE resource_locks (
        resource TEXT PRIMARY KEY,
        operation_id TEXT NOT NULL REFERENCES operations(id),
        acquired_at INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX resource_locks_operation ON resource_locks(operation_id);
    CREATE TABLE confirmations (
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
    CREATE INDEX confirmations_operation ON confirmations(operation_id);
    CREATE TABLE audit_events (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        server_id TEXT NOT NULL REFERENCES server_identity(server_id),
        principal_id TEXT NOT NULL,
        device_id TEXT NOT NULL,
        source TEXT NOT NULL,
        action TEXT NOT NULL,
        target TEXT NOT NULL,
        outcome TEXT NOT NULL,
        operation_id TEXT,
        at INTEGER NOT NULL,
        metadata_json TEXT NOT NULL,
        prev_hash TEXT NOT NULL,
        hash TEXT NOT NULL UNIQUE
    ) STRICT;
    CREATE TRIGGER audit_events_no_update BEFORE UPDATE ON audit_events
    BEGIN SELECT RAISE(ABORT, 'audit_events is append-only'); END;
    CREATE TRIGGER audit_events_no_delete BEFORE DELETE ON audit_events
    BEGIN SELECT RAISE(ABORT, 'audit_events is append-only'); END;
    CREATE TABLE events (
        cursor INTEGER PRIMARY KEY AUTOINCREMENT,
        server_id TEXT NOT NULL REFERENCES server_identity(server_id),
        type TEXT NOT NULL,
        operation_id TEXT,
        at INTEGER NOT NULL,
        payload_json TEXT NOT NULL
    ) STRICT;
    CREATE INDEX events_at ON events(at);
    CREATE TABLE settings (
        server_id TEXT NOT NULL REFERENCES server_identity(server_id),
        key TEXT NOT NULL,
        desired_value TEXT NOT NULL,
        desired_revision INTEGER NOT NULL CHECK (desired_revision >= 1),
        apply_mode TEXT NOT NULL CHECK (apply_mode IN ('immediate','on_restart','manual')),
        effective_value TEXT,
        effective_revision INTEGER NOT NULL DEFAULT 0 CHECK (effective_revision >= 0),
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (server_id, key)
    ) STRICT;
    "#
}

fn create_legacy_database(directory: &StateDirectory, old_version: u32, now_ms: i64) -> String {
    directory.prepare().unwrap();
    let connection = Connection::open(directory.database_file()).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")
        .unwrap();
    connection.execute_batch(legacy_v1_schema()).unwrap();
    connection
        .execute(
            "INSERT INTO server_identity (singleton, server_id, created_at) VALUES (1, ?1, ?2)",
            params![SERVER_ID, now_ms],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meta (key, value) VALUES ('events_pruned_through', '0')",
            [],
        )
        .unwrap();
    if old_version == 2 {
        connection
            .execute_batch(
                "ALTER TABLE operations ADD COLUMN input_json TEXT \
                    CHECK (input_json IS NULL OR length(input_json) <= 65536);\
                 ALTER TABLE operations ADD COLUMN input_status TEXT NOT NULL DEFAULT 'unavailable_legacy' \
                    CHECK (input_status IN ('available','unavailable_legacy','purged'));\
                 ALTER TABLE operations ADD COLUMN owner_token TEXT;\
                 CREATE INDEX operations_input_status ON operations(input_status, state);",
            )
            .unwrap();
    }

    for (id, state, effect_started) in [
        ("legacy-queued", "queued", 0),
        ("legacy-safe", "running", 0),
        ("legacy-effect", "running", 1),
    ] {
        connection
            .execute(
                "INSERT INTO operations (
                    id,server_id,actor_principal_id,device_id,source,type,target,payload_hash,
                    state,requires_confirmation,external_effect_started,external_effect_idempotent,
                    created_at,updated_at
                 ) VALUES (?1,?2,'principal','device','https','fixture.write','config',?3,?4,0,?5,0,?6,?6)",
                params![id, SERVER_ID, LEGACY_HASH, state, effect_started, now_ms],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO operation_resources (operation_id, resource) VALUES ('legacy-effect','world')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO resource_locks (resource, operation_id, acquired_at) VALUES ('world','legacy-effect',?1)",
            [now_ms],
        )
        .unwrap();
    connection
        .pragma_update(None, "user_version", old_version)
        .unwrap();
    let server_id = connection
        .query_row("SELECT server_id FROM server_identity", [], |row| {
            row.get(0)
        })
        .unwrap();
    drop(connection);
    server_id
}

#[test]
fn migrating_v1_and_v2_hash_only_rows_never_fabricates_inputs_and_preserves_backups() {
    let clock = Arc::new(TestClock::new(FIXED_TIME_MS));
    let now_ms = clock.millis();

    for old_version in [1, 2] {
        let temp = tempfile::tempdir().expect("create local temporary test root");
        let directory = StateDirectory::new(temp.path().join("private state"), None).unwrap();
        let server_id = create_legacy_database(&directory, old_version, now_ms);

        let db = StateDatabase::open(directory.clone(), clock.clone()).unwrap();
        assert_eq!(
            db.schema_version().unwrap(),
            vesta_state::CURRENT_SCHEMA_VERSION
        );
        assert_eq!(db.server_id(), server_id);

        db.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT id,payload_hash,state,input_json,input_status FROM operations ORDER BY id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?;
            let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(rows.len(), 3);
            for (id, hash, state, input_json, input_status) in rows {
                assert_eq!(hash, LEGACY_HASH, "legacy payload hash for {id}");
                assert_eq!(input_json, None, "no fabricated input for {id}");
                assert_eq!(input_status, "unavailable_legacy", "legacy status for {id}");
                let expected_state = if id == "legacy-queued" { "queued" } else { "running" };
                assert_eq!(state, expected_state, "preserved state for {id}");
            }
            let lock_count: i64 = connection.query_row(
                "SELECT count(*) FROM resource_locks WHERE resource='world' AND operation_id='legacy-effect'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(lock_count, 1, "resource lock remains intact");
            let principal_table: i64 = connection.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='principals'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(principal_table, 1);
            Ok(())
        })
        .unwrap();
        assert!(
            AuditLog::new(db.clone())
                .unwrap()
                .verify_chain()
                .unwrap()
                .valid
        );
        db.close().unwrap();

        let backups: Vec<_> = fs::read_dir(directory.backups_dir())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(backups.len(), 1);
        let backup = Connection::open(&backups[0]).unwrap();
        let backup_version: i64 = backup
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(backup_version, i64::from(old_version));
        let backup_journal_mode: String = backup
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(backup_journal_mode, "delete");
        let integrity: String = backup
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        let backup_server_id: String = backup
            .query_row("SELECT server_id FROM server_identity", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(backup_server_id, server_id);
        let operation_count: i64 = backup
            .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(operation_count, 3);
        let principals_table: i64 = backup
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='principals'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(principals_table, 0);
    }
}

#[test]
fn failed_audit_insert_rolls_back_input_erasure_on_the_real_database_transaction() {
    let temp = tempfile::tempdir().expect("create local temporary test root");
    let clock = Arc::new(TestClock::new(FIXED_TIME_MS));
    let directory = StateDirectory::new(temp.path().join("private state"), None).unwrap();
    let db = StateDatabase::open(directory, clock.clone()).unwrap();
    let canonical_input = r#"{"value":"requested"}"#;

    db.write(|connection| {
        connection.execute(
            "INSERT INTO operations (
                id,server_id,actor_principal_id,device_id,source,type,target,payload_hash,idempotency_key,
                state,requires_confirmation,external_effect_idempotent,input_json,input_status,created_at,updated_at
             ) VALUES (?1,?2,'principal','device','https','fixture.write','config',?3,'purge',
                'succeeded',0,0,?4,'available',?5,?5)",
            params!["purge-operation", db.server_id(), LEGACY_HASH, canonical_input, clock.millis()],
        )?;
        connection.execute_batch(
            "CREATE TRIGGER audit_failure BEFORE INSERT ON audit_events \
             BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )?;
        Ok(())
    })
    .unwrap();

    let error = db
        .write(|connection| {
            connection.execute(
                "UPDATE operations SET input_json=NULL,input_status='purged' WHERE id='purge-operation'",
                [],
            )?;
            connection.execute(
                "INSERT INTO audit_events (
                    server_id,principal_id,device_id,source,action,target,outcome,at,metadata_json,prev_hash,hash
                 ) VALUES (?1,'principal','device','https','operation.input_purged','purge-operation','ok',?2,'{}','prev','hash')",
                params![db.server_id(), clock.millis()],
            )?;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.code(), "state_io");

    let persisted: (Option<String>, String, String) = db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT input_json,input_status,payload_hash FROM operations WHERE id='purge-operation'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(persisted.0.as_deref(), Some(canonical_input));
    assert_eq!(persisted.1, "available");
    assert_eq!(persisted.2, LEGACY_HASH);
}
