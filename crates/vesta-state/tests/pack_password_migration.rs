mod support;

use rusqlite::{Connection, params};
use std::{fs, sync::Arc};
use support::TestClock;
use vesta_state::{CURRENT_SCHEMA_VERSION, Clock, StateDatabase, StateDirectory, audit::AuditLog};

const FIXED_TIME_MS: i64 = 1_791_201_600_000; // 2026-10-05T12:00:00Z
const SERVER_ID: &str = "00000000-0000-4000-8000-000000000004";

/// A real schema-three SQLite fixture containing the tables read by the assertions and migration.
/// Its schema version is three, and pack_passwords is deliberately absent.
fn create_schema_three_fixture(directory: &StateDirectory, now_ms: i64) -> String {
    directory.prepare().unwrap();
    let connection = Connection::open(directory.database_file()).unwrap();
    connection
        .execute_batch(
            r#"
            PRAGMA foreign_keys=ON;
            PRAGMA journal_mode=WAL;
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
            CREATE TABLE principals (
                id TEXT PRIMARY KEY,
                server_id TEXT NOT NULL REFERENCES server_identity(server_id),
                display_name TEXT NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('active','disabled')),
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            ) STRICT;
            CREATE TABLE devices (
                id TEXT PRIMARY KEY,
                server_id TEXT NOT NULL REFERENCES server_identity(server_id),
                principal_id TEXT NOT NULL REFERENCES principals(id),
                label TEXT NOT NULL,
                public_key_sha256 TEXT NOT NULL UNIQUE CHECK (length(public_key_sha256) = 64),
                created_at INTEGER NOT NULL,
                revoked_at INTEGER,
                revoked_by TEXT
            ) STRICT;
            CREATE INDEX devices_principal ON devices(principal_id);
            CREATE TABLE roles (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                rank INTEGER NOT NULL CHECK (rank BETWEEN 1 AND 100),
                builtin INTEGER NOT NULL CHECK (builtin IN (0,1)),
                created_by TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                CHECK (builtin = 1 OR rank < 80)
            ) STRICT;
            CREATE TABLE role_capabilities (
                role_id TEXT NOT NULL REFERENCES roles(id),
                capability TEXT NOT NULL,
                scope TEXT NOT NULL,
                PRIMARY KEY (role_id, capability, scope)
            ) STRICT;
            CREATE TABLE role_assignments (
                id TEXT PRIMARY KEY,
                principal_id TEXT NOT NULL REFERENCES principals(id),
                role_id TEXT NOT NULL REFERENCES roles(id),
                granted_by TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER,
                revoked_at INTEGER,
                revoked_by TEXT,
                expiry_recorded_at INTEGER
            ) STRICT;
            CREATE INDEX role_assignments_principal ON role_assignments(principal_id);
            CREATE TABLE principal_overrides (
                id TEXT PRIMARY KEY,
                principal_id TEXT NOT NULL REFERENCES principals(id),
                capability TEXT NOT NULL,
                scope TEXT NOT NULL,
                effect TEXT NOT NULL CHECK (effect IN ('allow','deny')),
                granted_by TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER,
                revoked_at INTEGER,
                revoked_by TEXT,
                expiry_recorded_at INTEGER
            ) STRICT;
            CREATE INDEX principal_overrides_principal ON principal_overrides(principal_id);
            CREATE TABLE bootstrap_codes (
                id TEXT PRIMARY KEY,
                server_id TEXT NOT NULL REFERENCES server_identity(server_id),
                purpose TEXT NOT NULL CHECK (purpose IN ('owner_bootstrap','owner_recovery')),
                code_hash TEXT NOT NULL UNIQUE CHECK (length(code_hash) = 64),
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                consumed_at INTEGER,
                invalidated_at INTEGER
            ) STRICT;
            CREATE TABLE local_authority_uses (
                nonce_hash TEXT PRIMARY KEY CHECK (length(nonce_hash) = 64),
                purpose TEXT NOT NULL,
                used_at INTEGER NOT NULL
            ) STRICT;
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
            "#,
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO server_identity (singleton, server_id, created_at) VALUES (1, ?1, ?2)",
            params![SERVER_ID, now_ms],
        )
        .unwrap();
    connection
        .execute_batch(
            "INSERT INTO meta (key, value) VALUES ('events_pruned_through', '0');\
             INSERT INTO meta (key, value) VALUES ('old-marker', 'preserved');",
        )
        .unwrap();
    for (id, name, rank) in [
        ("owner", "Owner", 100),
        ("administrator", "Administrator", 80),
        ("maintainer", "Maintainer", 60),
        ("moderator", "Moderator", 40),
        ("viewer", "Viewer", 20),
        ("pack-user", "Pack User", 10),
    ] {
        connection
            .execute(
                "INSERT INTO roles (id,name,rank,builtin,created_by,created_at) VALUES (?1,?2,?3,1,'migration',?4)",
                params![id, name, rank, now_ms],
            )
            .unwrap();
    }
    connection
        .execute_batch(
            r#"
            CREATE TRIGGER roles_builtin_no_update BEFORE UPDATE ON roles WHEN OLD.builtin = 1 OR NEW.builtin = 1
            BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
            CREATE TRIGGER roles_builtin_no_delete BEFORE DELETE ON roles WHEN OLD.builtin = 1
            BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
            CREATE TRIGGER roles_builtin_no_insert BEFORE INSERT ON roles WHEN NEW.builtin = 1
            BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
            CREATE TRIGGER role_capabilities_builtin_no_insert BEFORE INSERT ON role_capabilities
            WHEN (SELECT builtin FROM roles WHERE id = NEW.role_id) = 1
            BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
            CREATE TRIGGER role_capabilities_builtin_no_update BEFORE UPDATE ON role_capabilities
            WHEN (SELECT builtin FROM roles WHERE id = OLD.role_id) = 1
              OR (SELECT builtin FROM roles WHERE id = NEW.role_id) = 1
            BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
            CREATE TRIGGER role_capabilities_builtin_no_delete BEFORE DELETE ON role_capabilities
            WHEN (SELECT builtin FROM roles WHERE id = OLD.role_id) = 1
            BEGIN SELECT RAISE(ABORT, 'builtin roles are immutable'); END;
            CREATE TRIGGER local_authority_uses_no_update BEFORE UPDATE ON local_authority_uses
            BEGIN SELECT RAISE(ABORT, 'local_authority_uses is append-only'); END;
            CREATE TRIGGER local_authority_uses_no_delete BEFORE DELETE ON local_authority_uses
            BEGIN SELECT RAISE(ABORT, 'local_authority_uses is append-only'); END;
            "#,
        )
        .unwrap();
    connection.pragma_update(None, "user_version", 3).unwrap();
    drop(connection);
    SERVER_ID.to_owned()
}

fn open_schema_three(temp: &tempfile::TempDir, now_ms: i64) -> (StateDirectory, String) {
    let directory = StateDirectory::new(temp.path().join("private state"), None).unwrap();
    let server_id = create_schema_three_fixture(&directory, now_ms);
    (directory, server_id)
}

#[test]
fn schema_three_migration_preserves_identity_roles_and_original_verified_backup() {
    let temp = tempfile::tempdir().expect("create local temporary test root");
    let clock = Arc::new(TestClock::new(FIXED_TIME_MS));
    let now_ms = clock.millis();
    let (directory, server_id) = open_schema_three(&temp, now_ms);

    let db = StateDatabase::open(directory.clone(), clock.clone()).unwrap();
    assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(db.server_id(), server_id);
    db.read(|connection| {
        let marker: String =
            connection.query_row("SELECT value FROM meta WHERE key='old-marker'", [], |row| {
                row.get(0)
            })?;
        assert_eq!(marker, "preserved");
        let role_count: i64 =
            connection.query_row("SELECT count(*) FROM roles WHERE builtin=1", [], |row| {
                row.get(0)
            })?;
        assert_eq!(role_count, 6);
        let pack_password_count: i64 =
            connection.query_row("SELECT count(*) FROM pack_passwords", [], |row| row.get(0))?;
        assert_eq!(pack_password_count, 0);
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
    assert_eq!(backup_version, 3);
    let integrity: String = backup
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let backup_journal_mode: String = backup
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(backup_journal_mode, "delete");
    let backup_server_id: String = backup
        .query_row("SELECT server_id FROM server_identity", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(backup_server_id, server_id);
    let pack_table_count: i64 = backup
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='pack_passwords'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pack_table_count, 0);
    drop(backup);

    let reopened = StateDatabase::open(directory.clone(), clock).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(reopened.server_id(), server_id);
    reopened.close().unwrap();
    assert_eq!(fs::read_dir(directory.backups_dir()).unwrap().count(), 1);
}

#[test]
fn fresh_schema_enforces_pack_password_singleton_foreign_key_and_tombstone_constraints() {
    let temp = tempfile::tempdir().expect("create local temporary test root");
    let clock = Arc::new(TestClock::new(FIXED_TIME_MS));
    let directory = StateDirectory::new(temp.path().join("private state"), None).unwrap();
    let db = StateDatabase::open(directory.clone(), clock).unwrap();
    assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);

    db.write(|connection| {
        connection.execute(
            "INSERT INTO pack_passwords VALUES (?1,1,'bounded-phc',1,1,NULL)",
            [db.server_id()],
        )?;
        Ok(())
    })
    .unwrap();

    for invalid in [
        "INSERT INTO pack_passwords VALUES ('other-server',1,'x',1,1,NULL)",
        "INSERT INTO pack_passwords SELECT * FROM pack_passwords",
        "UPDATE pack_passwords SET revision=0",
        "UPDATE pack_passwords SET revision=-1",
        "UPDATE pack_passwords SET phc=NULL",
        "UPDATE pack_passwords SET phc=''",
        "UPDATE pack_passwords SET revoked_at=1",
        "UPDATE pack_passwords SET revision='not-integer'",
    ] {
        let result = db.write(|connection| {
            connection.execute(invalid, [])?;
            Ok(())
        });
        assert!(result.is_err(), "invalid SQL must fail: {invalid}");
    }
    let too_long_phc = "x".repeat(257);
    let result = db.write(|connection| {
        connection.execute("UPDATE pack_passwords SET phc=?1", [&too_long_phc])?;
        Ok(())
    });
    assert!(
        result.is_err(),
        "PHC values longer than 256 characters fail"
    );

    db.write(|connection| {
        connection.execute(
            "UPDATE pack_passwords SET phc=NULL,revoked_at=2,revision=2",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    let reactivation = db.write(|connection| {
        connection.execute("UPDATE pack_passwords SET phc='x'", [])?;
        Ok(())
    });
    assert!(
        reactivation.is_err(),
        "revoked rows cannot regain a PHC without clearing revocation"
    );
    let tombstone: (i64, Option<String>, Option<i64>) = db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT revision,phc,revoked_at FROM pack_passwords",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(tombstone, (2, None, Some(2)));
    db.close().unwrap();
    assert_eq!(fs::read_dir(directory.backups_dir()).unwrap().count(), 0);
}

#[test]
fn failed_schema_four_migration_rolls_back_and_retains_schema_three_backup() {
    let temp = tempfile::tempdir().expect("create local temporary test root");
    let clock = Arc::new(TestClock::new(FIXED_TIME_MS));
    let (directory, server_id) = open_schema_three(&temp, clock.millis());
    let raw = Connection::open(directory.database_file()).unwrap();
    raw.execute_batch(
        "CREATE TABLE pack_passwords (sentinel TEXT); INSERT INTO pack_passwords VALUES ('untouched');",
    )
    .unwrap();
    drop(raw);

    let error = StateDatabase::open(directory.clone(), clock.clone()).unwrap_err();
    assert_eq!(error.code(), "state_migration_failed");
    let unchanged = Connection::open(directory.database_file()).unwrap();
    let version: i64 = unchanged
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let sentinel: String = unchanged
        .query_row("SELECT sentinel FROM pack_passwords", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sentinel, "untouched");
    let retained_id: String = unchanged
        .query_row("SELECT server_id FROM server_identity", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(retained_id, server_id);
    drop(unchanged);
    assert_eq!(fs::read_dir(directory.backups_dir()).unwrap().count(), 1);
}

#[test]
fn failed_pre_migration_backup_preserves_existing_artifact_and_leaves_schema_three_unchanged() {
    let temp = tempfile::tempdir().expect("create local temporary test root");
    let clock = Arc::new(TestClock::new(FIXED_TIME_MS));
    let (directory, _) = open_schema_three(&temp, clock.millis());
    let collision = directory.backups_dir().join(format!(
        "pre-migration-v3-to-v{CURRENT_SCHEMA_VERSION}-{}.db",
        clock.millis()
    ));
    fs::write(&collision, "outside-owned-marker").unwrap();

    let error = StateDatabase::open(directory.clone(), clock).unwrap_err();
    assert_eq!(error.code(), "state_backup_failed");
    assert_eq!(
        fs::read_to_string(&collision).unwrap(),
        "outside-owned-marker"
    );
    let unchanged = Connection::open(directory.database_file()).unwrap();
    let version: i64 = unchanged
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let pack_table_count: i64 = unchanged
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='pack_passwords'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pack_table_count, 0);
}
