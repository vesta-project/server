mod support;

use rusqlite::{Connection, params};
use support::{DEFAULT_TEST_TIME_MS, Fixture};
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};

#[test]
fn v10_upgrade_preserves_pending_trash_and_adds_constrained_intents() {
    let f = Fixture::open();
    let directory = f.db.directory().clone();
    let server = f.db.server_id().to_owned();
    let trash = "a".repeat(32);
    f.db.write(|c| {
        c.execute("INSERT INTO file_trash VALUES (?,?,?, ?,3,'replace','principal','device','pending',?,NULL,0,?,?)", params![trash,server,"config/legacy.txt","b".repeat(64),"c".repeat(64),DEFAULT_TEST_TIME_MS,DEFAULT_TEST_TIME_MS+1000])?;
        Ok(())
    }).unwrap();
    f.db.close().unwrap();
    let old = Connection::open(directory.database_file()).unwrap();
    support::remove_v12_schema(&old);
    old.execute_batch("DROP TABLE file_intents; PRAGMA user_version=10;")
        .unwrap();
    drop(old);
    let upgraded = StateDatabase::open(directory.clone(), f.clock.clone()).unwrap();
    assert_eq!(upgraded.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(upgraded.server_id(), server);
    upgraded.write(|c| {
        assert_eq!(c.query_row("SELECT path,status FROM file_trash WHERE id=?", [&trash], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?, ("config/legacy.txt".into(),"pending".into()));
        c.execute("INSERT INTO file_intents VALUES (?,?, 'principal','device','rename','config/legacy.txt',?,'pending',?)", params!["d".repeat(32),server,r#"{"to":"config/moved.txt","previous":null,"effect":null,"stage":null,"trash":null,"upload":null,"size":3}"#,DEFAULT_TEST_TIME_MS])?;
        for (column, value) in [
            ("id", "'bad'"), ("server_id", "'missing'"), ("kind", "'unknown'"),
            ("path", "''"), ("details_json", "'not json'"), ("status", "'unknown'"),
        ] {
            assert!(c.execute(&format!("UPDATE file_intents SET {column}={value}"), []).is_err(), "{column}");
        }
        assert!(c.execute("UPDATE file_intents SET path=?", ["x".repeat(4097)]).is_err());
        assert!(c.execute("UPDATE file_intents SET details_json=?", [format!("\"{}\"", "x".repeat(32768))]).is_err());
        assert_eq!(c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_,i64>(0))?,0);
        Ok(())
    }).unwrap();
    upgraded.close().unwrap();
    let backup = Connection::open_with_flags(
        directory.backups_dir().join(format!(
            "pre-migration-v10-to-v{CURRENT_SCHEMA_VERSION}-{DEFAULT_TEST_TIME_MS}.db"
        )),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        backup
            .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        10
    );
    assert_eq!(
        backup
            .query_row(
                "SELECT count(*) FROM file_trash WHERE status='pending'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        backup
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='file_intents'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
