mod support;

use rusqlite::Connection;
use std::error::Error;
use std::fs;
use std::path::Path;
#[cfg(windows)]
use std::process::Command;
use std::sync::Arc;
use support::{Fixture, TestClock, create_directory_link, remove_directory_link};
use vesta_state::{
    CURRENT_SCHEMA_VERSION, Clock, Result as StateResult, StateDatabase, StateDirectory, StateError,
};

fn assert_error_code<T>(result: StateResult<T>, expected: &str) {
    match result {
        Ok(_) => panic!("expected state error {expected}"),
        Err(error) => assert_eq!(error.code(), expected),
    }
}

fn standalone_directory(root: &Path, name: &str) -> StateDirectory {
    StateDirectory::new(root.join(name), None).expect("valid test state path")
}

#[test]
fn real_wal_database_has_stable_server_identity_and_connection_invariants() {
    let fixture = Fixture::open();
    let server_id = fixture.db.server_id().to_owned();
    assert!(uuid::Uuid::parse_str(&server_id).is_ok());
    assert_eq!(fixture.db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);

    fixture
        .db
        .read(|connection| {
            for pragma in ["foreign_keys", "query_only"] {
                let value: i64 =
                    connection.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))?;
                assert_eq!(value, 1, "{pragma}");
            }
            let journal_mode: String =
                connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
            assert_eq!(journal_mode.to_lowercase(), "wal");
            let busy_timeout: i64 =
                connection.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
            assert!(busy_timeout > 0);

            // The read connection must be query-only, independently of the write API.
            assert!(
                connection
                    .execute("INSERT INTO meta VALUES ('read-write', 'no')", [])
                    .is_err()
            );
            Ok(())
        })
        .unwrap();

    let foreign_key_result = fixture.db.write(|connection| {
        connection.execute(
            "INSERT INTO operation_resources VALUES ('missing', 'world')",
            [],
        )?;
        Ok(())
    });
    assert!(foreign_key_result.is_err(), "writes enforce foreign keys");

    fixture.db.close().unwrap();
    let directory = StateDirectory::new(fixture.state_path(), Some(&fixture.game_path())).unwrap();
    let reopened = StateDatabase::open(directory, fixture.clock.clone()).unwrap();
    assert_eq!(reopened.server_id(), server_id);
    reopened.close().unwrap();
}

#[test]
fn failed_and_nested_writes_roll_back_without_partial_commit() {
    let fixture = Fixture::open();

    let injected = fixture.db.write(|connection| -> StateResult<()> {
        connection.execute("INSERT INTO meta VALUES ('must-rollback', 'x')", [])?;
        Err(StateError::new("state_fixture_injected_failure"))
    });
    assert_error_code(injected, "state_fixture_injected_failure");

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fixture.db.write(|connection| -> StateResult<()> {
            connection.execute("INSERT INTO meta VALUES ('must-panic', 'x')", [])?;
            panic!("injected callback panic");
        })
    }));
    assert!(panicked.is_err(), "callback panic propagates");

    let nested = fixture.db.write(|_| fixture.db.write(|_| Ok(())));
    assert!(
        nested.is_err(),
        "nested writes must fail rather than partially commit"
    );

    let count: i64 = fixture
        .db
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM meta WHERE key IN ('must-rollback', 'must-panic')",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(count, 0);

    fixture.db.close().unwrap();
    assert_error_code(fixture.db.read(|_| Ok(())), "state_closed");
    assert_error_code(fixture.db.write(|_| Ok(())), "state_closed");
}

#[test]
fn pre_migration_backup_is_a_verified_sqlite_snapshot_of_the_original_schema() {
    let temp = tempfile::tempdir().expect("create local temporary test root");
    let state_path = temp.path().join("private state");
    let game_path = temp.path().join("game root");
    let directory = StateDirectory::new(&state_path, Some(&game_path)).unwrap();
    directory.prepare().unwrap();
    let clock = Arc::new(TestClock::new(1_790_107_200_000));
    let database_file = directory.database_file();
    let original = Connection::open(database_file).expect("open original SQLite file");
    original
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE original (value TEXT);
             INSERT INTO original VALUES ('before migration');",
        )
        .unwrap();

    let migrated = StateDatabase::open(directory, clock).unwrap();
    assert_eq!(migrated.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    migrated.close().unwrap();

    let backups: Vec<_> = fs::read_dir(state_path.join("migration-backups"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    let backup = Connection::open(&backups[0]).expect("open SQLite backup");
    let version: i64 = backup
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 0);
    let value: String = backup
        .query_row("SELECT value FROM original", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "before migration");
    let integrity: String = backup
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    drop(backup);
    drop(original);
}

#[test]
fn failed_pre_migration_backup_neither_overwrites_artifact_nor_migrates_database() {
    let temp = tempfile::tempdir().expect("create local temporary test root");
    let clock = Arc::new(TestClock::new(1_790_150_400_000));
    let directory = standalone_directory(temp.path(), "old-state");
    let current = StateDatabase::open(directory.clone(), clock.clone()).unwrap();
    current.close().unwrap();
    let database_file = directory.database_file().to_path_buf();
    let original = Connection::open(&database_file).expect("reopen schema database");
    original
        .execute_batch(
            "DROP TABLE pack_passwords;
             INSERT INTO meta VALUES ('old-marker', 'preserved');
             PRAGMA user_version = 3;",
        )
        .unwrap();
    drop(original);

    let collision = directory.backups_dir().join(format!(
        "pre-migration-v3-to-v{CURRENT_SCHEMA_VERSION}-{}.db",
        clock.millis()
    ));
    fs::write(&collision, "outside-owned-marker").unwrap();
    assert_error_code(StateDatabase::open(directory, clock), "state_backup_failed");
    assert_eq!(
        fs::read_to_string(&collision).unwrap(),
        "outside-owned-marker"
    );

    let unchanged = Connection::open(&database_file).expect("reopen unchanged database");
    let version: i64 = unchanged
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let marker: String = unchanged
        .query_row("SELECT value FROM meta WHERE key='old-marker'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(marker, "preserved");
    let schema_four_tables: i64 = unchanged
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='pack_passwords'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(schema_four_tables, 0);
}

#[test]
fn corrupt_or_newer_database_fails_closed() {
    let temp = tempfile::tempdir().expect("create local temporary test root");
    let clock = Arc::new(TestClock::new(1_790_150_400_000));

    let corrupt = standalone_directory(temp.path(), "corrupt-state");
    corrupt.prepare().unwrap();
    fs::write(corrupt.database_file(), "this is not SQLite").unwrap();
    assert!(StateDatabase::open(corrupt, clock.clone()).is_err());

    let newer = standalone_directory(temp.path(), "newer-state");
    newer.prepare().unwrap();
    let raw = Connection::open(newer.database_file()).unwrap();
    raw.execute_batch("PRAGMA user_version = 999;").unwrap();
    drop(raw);
    assert_error_code(StateDatabase::open(newer, clock), "state_schema_too_new");
}

#[test]
fn database_with_foreign_key_violation_is_rejected_on_reopen() {
    let fixture = Fixture::open();
    let state_path = fixture.state_path();
    let game_path = fixture.game_path();
    fixture.db.close().unwrap();

    let raw = Connection::open(state_path.join("vesta-state.db")).unwrap();
    // Deliberately create a corrupt fixture, independent of the bundled SQLite default.
    raw.pragma_update(None, "foreign_keys", "OFF").unwrap();
    let enforcement: i64 = raw
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(enforcement, 0);
    raw.execute(
        "INSERT INTO operation_resources VALUES ('missing', 'world')",
        [],
    )
    .unwrap();
    drop(raw);

    let directory = StateDirectory::new(state_path, Some(&game_path)).unwrap();
    assert_error_code(
        StateDatabase::open(directory, fixture.clock.clone()),
        "state_integrity_failed",
    );
}

#[test]
fn linked_backups_directory_is_rejected_without_touching_external_files()
-> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let directory = standalone_directory(temp.path(), "state");
    fs::create_dir_all(directory.path())?;
    let outside = temp.path().join("external");
    fs::create_dir(&outside)?;
    fs::write(outside.join("sentinel"), "unchanged")?;
    create_directory_link(directory.backups_dir(), &outside)?;

    assert_error_code(
        StateDatabase::open(directory, Arc::new(TestClock::new(1_790_150_400_000))),
        "state_file_linked",
    );
    assert_eq!(fs::read_to_string(outside.join("sentinel"))?, "unchanged");
    assert_eq!(fs::read_dir(&outside)?.count(), 1);
    assert!(!temp.path().join("state/vesta-state.db").exists());
    remove_directory_link(&temp.path().join("state/migration-backups"))?;
    Ok(())
}

#[test]
fn existing_connections_reject_a_replaced_backup_directory() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let directory = standalone_directory(temp.path(), "state");
    let db = StateDatabase::open(directory, Arc::new(TestClock::new(1_790_150_400_000)))?;
    let backups = db.directory().backups_dir();
    fs::remove_dir(backups)?;
    let outside = temp.path().join("external");
    fs::create_dir(&outside)?;
    create_directory_link(backups, &outside)?;

    assert_error_code(db.read(|_| Ok(())), "state_file_linked");
    assert!(db.write(|_| Ok(())).is_err());

    remove_directory_link(backups)?;
    fs::create_dir(backups)?;
    db.close()?;
    Ok(())
}

#[test]
fn state_must_not_overlap_game_root_and_uses_owner_only_local_permissions() {
    let temp = tempfile::tempdir().expect("create local temporary test root");
    let game = temp.path().join("game");
    fs::create_dir_all(&game).unwrap();

    assert!(StateDirectory::new(game.join("state"), Some(&game)).is_err());
    assert!(StateDirectory::new(temp.path(), Some(&game)).is_err());
    assert!(StateDirectory::new(&game, Some(&game)).is_err());

    let fixture = Fixture::open();
    let paths = [
        fixture.db.directory().path().to_path_buf(),
        fixture.db.directory().backups_dir().to_path_buf(),
        fixture.db.directory().database_file().to_path_buf(),
    ];

    // These checks are same-account storage hygiene; they do not isolate a game process
    // that runs as the same operating-system account.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in paths {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            let expected = if path.is_dir() { 0o700 } else { 0o600 };
            assert_eq!(
                mode,
                expected,
                "owner-only permissions for {}",
                path.display()
            );
        }
    }

    #[cfg(windows)]
    {
        for path in paths {
            assert_owner_only_acl(&path);
        }
    }
}

#[cfg(windows)]
fn assert_owner_only_acl(path: &Path) {
    let path = path.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$acl = Get-Acl -LiteralPath '{path}'; \
         if ($acl.Access.Count -gt 0 -and \
             @($acl.Access | Where-Object {{ $_.IdentityReference.Value -ne $acl.Owner -or $_.AccessControlType -ne 'Allow' }}).Count -eq 0) \
         {{ 'owner-only' }} else {{ exit 1 }}"
    );
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .expect("run Windows ACL inspection");
    assert!(
        output.status.success(),
        "ACL must contain only owner ALLOW entries for {}: {}",
        path,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "owner-only",
        "non-empty owner-only ACL for {path}"
    );
}
