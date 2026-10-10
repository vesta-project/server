mod support;

use rusqlite::{Connection, params};
use support::{DEFAULT_TEST_TIME_MS, Fixture};
use vesta_state::StateDatabase;

#[test]
fn populated_v7_migrates_to_v9_without_losing_rows_and_enforces_trash_constraints() {
    let fixture = Fixture::open();
    let directory = fixture.db.directory().clone();
    let server = fixture.db.server_id().to_owned();
    fixture.db.write(|c| {
        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES ('legacy-principal',?,'Preserved','active',?,?)", params![server, DEFAULT_TEST_TIME_MS, DEFAULT_TEST_TIME_MS])?;
        c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES ('legacy-role','legacy-principal','owner','fixture',?)", [DEFAULT_TEST_TIME_MS])?;
        Ok(())
    }).unwrap();
    fixture.db.close().unwrap();
    let offline = Connection::open(directory.database_file()).unwrap();
    offline
        .execute_batch("BEGIN IMMEDIATE; DROP TABLE file_uploads; DROP TABLE file_trash; PRAGMA user_version=7; COMMIT;")
        .unwrap();
    drop(offline);
    let migrated = StateDatabase::open(directory, fixture.clock.clone()).unwrap();
    assert_eq!(migrated.schema_version().unwrap(), 9);
    migrated.write(|c| {
        assert_eq!(c.query_row("SELECT display_name FROM principals WHERE id='legacy-principal'", [], |row| row.get::<_, String>(0))?, "Preserved");
        assert_eq!(c.query_row("SELECT role_id FROM role_assignments WHERE id='legacy-role'", [], |row| row.get::<_, String>(0))?, "owner");
        assert_eq!(c.query_row("SELECT count(*) FROM file_trash", [], |row| row.get::<_, i64>(0))?, 0);
        let insert = "INSERT INTO file_trash (id,server_id,path,sha256,size,reason,principal_id,device_id,status,effect_sha256,unix_mode,readonly,trashed_at,expires_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)";
        let valid_id = "a".repeat(32);
        let hash = "b".repeat(64);
        c.execute(insert, params![valid_id,server,"config/a.txt",hash,3,"replace","legacy-principal","fixture-device","pending",hash,0o640,0,100,200])?;
        c.execute("UPDATE file_trash SET status='complete' WHERE id=?", [&valid_id])?;
        for (column, value) in [
            ("id", "'bad'"),
            ("server_id", "'missing-server'"),
            ("path", "''"),
            ("sha256", "'wrong'"),
            ("size", "-1"),
            ("reason", "'unknown'"),
            ("status", "'unknown'"),
            ("effect_sha256", "'wrong'"),
            ("effect_sha256", "NULL"),
            ("unix_mode", "4096"),
            ("unix_mode", "-1"),
            ("readonly", "2"),
            ("expires_at", "100"),
        ] {
            assert!(c.execute(&format!("UPDATE file_trash SET {column}={value} WHERE id=?"), [&valid_id]).is_err(), "constraint {column}={value}");
        }
        assert!(c.execute("UPDATE file_trash SET reason='delete' WHERE id=?", [&valid_id]).is_err());
        c.execute("UPDATE file_trash SET reason='delete', effect_sha256=NULL,unix_mode=NULL,readonly=1,status='recovery-required' WHERE id=?", [&valid_id])?;
        assert_eq!(c.query_row("SELECT status FROM file_trash WHERE id=?", [&valid_id], |row| row.get::<_, String>(0))?, "recovery-required");
        let checks: i64 = c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| row.get(0))?;
        assert_eq!(checks, 0);
        Ok(())
    }).unwrap();
    migrated.close().unwrap();
}
