mod support;

use rusqlite::params;
use support::{DEFAULT_TEST_TIME_MS, Fixture};
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};

const WORKFLOW: &str =
    "INSERT INTO workflows VALUES('workflow','Daily',1,'principal','{}',10,10,1)";
const RUN: &str = "INSERT INTO workflow_runs(id,workflow_id,revision,definition_json,principal_id,trigger_json,state,current_step,attempt,started_at) VALUES(?1,'workflow',1,'{}','principal','{}',?2,0,0,10)";

#[test]
fn v17_upgrade_preserves_identity_and_protected_backup_and_seeds_exact_defaults() {
    let f = Fixture::open();
    let directory = f.db.directory().clone();
    let server = f.db.server_id().to_owned();
    f.db.write(|c| {
        c.execute_batch(support::REMOVE_V23_SCHEMA)?;
        c.execute_batch(support::REMOVE_V22_SCHEMA)?;
        c.execute_batch(support::REMOVE_V18_SCHEMA)?;
        c.execute("INSERT INTO principals VALUES('existing',?,'Existing','active',10,10)", [&server])?;
        c.execute_batch("INSERT INTO roles VALUES('custom','Custom',10,0,'existing',10); INSERT INTO role_capabilities VALUES('custom','pack.access','*'); PRAGMA user_version=17;")?;
        Ok(())
    }).unwrap();
    f.db.close().unwrap();
    let upgraded = StateDatabase::open(directory, f.clock.clone()).unwrap();
    assert_eq!(upgraded.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(upgraded.server_id(), server);
    upgraded.read(|c| {
        assert_eq!(c.query_row("SELECT display_name FROM principals WHERE id='existing'", [], |r| r.get::<_, String>(0))?, "Existing");
        for role in ["owner", "administrator", "maintainer", "moderator", "viewer", "pack-user", "custom"] {
            for cap in ["automation.view", "automation.manage", "notifications.view"] {
                let expected = matches!(role, "owner" | "administrator") || (role == "moderator" && cap == "notifications.view");
                let exists: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM role_capabilities WHERE role_id=? AND capability=? AND scope='*')", params![role, cap], |r| r.get(0))?;
                assert_eq!(exists, expected, "{role} {cap}");
            }
        }
        assert_eq!(c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0))?, 0);
        Ok(())
    }).unwrap();
    upgraded.write(|c| {
        assert!(c.execute("INSERT INTO role_capabilities VALUES('moderator','automation.manage','*')", []).is_err());
        assert!(c.execute("DELETE FROM role_capabilities WHERE role_id='moderator' AND capability='notifications.view'", []).is_err());
        assert!(c.execute("UPDATE role_capabilities SET scope='other' WHERE role_id='owner' AND capability='automation.view'", []).is_err());
        Ok(())
    }).unwrap();
    let backup = rusqlite::Connection::open_with_flags(
        f.state_path().join(format!(
            "migration-backups/pre-migration-v17-to-v{CURRENT_SCHEMA_VERSION}-{DEFAULT_TEST_TIME_MS}.db"
        )),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        backup
            .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        17
    );
    assert_eq!(
        backup
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='workflows'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
        0
    );
    upgraded.close().unwrap();
}

#[test]
fn suspended_runs_block_duplicates_and_deleting_workflow_preserves_run_snapshots() {
    let f = Fixture::open();
    f.db.write(|c| {
        c.execute_batch(WORKFLOW)?;
        c.execute("INSERT INTO workflow_schedule VALUES('workflow',10)", [])?;
        c.execute(RUN, params!["first", "running"])?;
        c.execute_batch("INSERT INTO run_steps(run_id,index_no,attempt,state,started_at) VALUES('first',0,0,'running',10)")?;
        assert!(c.execute(RUN, params!["second", "running"]).is_err());
        c.execute("UPDATE workflow_runs SET state='suspended' WHERE id='first'", [])?;
        assert!(c.execute(RUN, params!["second", "running"]).is_err());
        assert!(c.execute(RUN, params!["second", "suspended"]).is_err());
        c.execute("UPDATE workflow_runs SET state='failed',outcome_code='timed_out',finished_at=20 WHERE id='first'", [])?;
        c.execute("UPDATE run_steps SET state='timed_out',error_code='timed_out',finished_at=20 WHERE run_id='first'", [])?;
        c.execute(RUN, params!["second", "running"])?;
        assert!(c.execute("UPDATE workflow_runs SET state='suspended' WHERE id='first'", []).is_err());
        c.execute("DELETE FROM workflows WHERE id='workflow'", [])?;
        assert_eq!(c.query_row("SELECT count(*) FROM workflow_schedule", [], |r| r.get::<_, u32>(0))?, 0);
        assert_eq!(c.query_row("SELECT count(*) FROM workflow_runs", [], |r| r.get::<_, u32>(0))?, 2);
        assert_eq!(c.query_row("SELECT definition_json FROM workflow_runs WHERE id='first'", [], |r| r.get::<_, String>(0))?, "{}");
        c.execute("DELETE FROM workflow_runs WHERE id='first'", [])?;
        assert_eq!(c.query_row("SELECT count(*) FROM run_steps", [], |r| r.get::<_, u32>(0))?, 0);
        assert!(c.execute("INSERT INTO run_steps(run_id,index_no,attempt,state,started_at) VALUES('missing',0,0,'running',10)", []).is_err());
        Ok(())
    }).unwrap();
}

#[test]
fn notification_cursors_never_reuse_and_principal_receipts_are_independent() {
    let f = Fixture::open();
    f.db.write(|c| {
        c.execute_batch("INSERT INTO notifications(id,created_at,severity,title,body,source) VALUES('one',10,'info','Title','','test'); INSERT INTO notification_state VALUES('alice','one',11,NULL); INSERT INTO notification_state VALUES('bob','one',NULL,12);")?;
        assert!(c.execute("INSERT INTO notification_state VALUES('alice','one',NULL,NULL)", []).is_err());
        assert!(c.execute("INSERT INTO notification_state VALUES('alice','missing',NULL,NULL)", []).is_err());
        assert_eq!(c.query_row("SELECT read_at FROM notification_state WHERE principal_id='bob'", [], |r| r.get::<_, Option<i64>>(0))?, None);
        let first: i64 = c.query_row("SELECT cursor FROM notifications", [], |r| r.get(0))?;
        c.execute("DELETE FROM notifications", [])?;
        assert_eq!(c.query_row("SELECT count(*) FROM notification_state", [], |r| r.get::<_, u32>(0))?, 0);
        c.execute("INSERT INTO notifications(id,created_at,severity,title,body,source) VALUES('two',10,'critical','Title','Body','test')", [])?;
        let second: i64 = c.query_row("SELECT cursor FROM notifications", [], |r| r.get(0))?;
        assert!(second > first);
        assert!(c.execute("INSERT INTO notifications(id,created_at,severity,title,body,source) VALUES('two',10,'warning','Title','','test')", []).is_err());
        Ok(())
    }).unwrap();
}

#[test]
fn automation_and_notification_checks_enforce_byte_bounds_json_and_domains() {
    let f = Fixture::open();
    f.db.write(|c| {
        c.execute_batch(WORKFLOW)?;
        c.execute(RUN, params!["first", "running"])?;
        c.execute_batch("INSERT INTO run_steps(run_id,index_no,attempt,state,started_at) VALUES('first',0,0,'running',10); INSERT INTO notifications(id,created_at,severity,title,body,source) VALUES('one',10,'info','Title','','test'); INSERT INTO notification_state VALUES('alice','one',NULL,NULL); INSERT INTO workflow_schedule VALUES('workflow',10);")?;
        for (table, column, value) in [
            ("workflows", "id", "''"),
            ("workflows", "name", "''"),
            ("workflows", "enabled", "2"),
            ("workflows", "revision", "0"),
            ("workflows", "revision", "9007199254740992"),
            ("workflows", "created_at", "-1"),
            ("workflows", "updated_at", "9"),
            ("workflows", "principal_id", "''"),
            ("workflows", "definition_json", "'invalid'"),
            ("workflow_runs", "state", "'unknown'"),
            ("workflow_runs", "attempt", "-1"),
            ("workflow_runs", "attempt", "6"),
            ("workflow_runs", "current_step", "32"),
            ("workflow_runs", "current_step", "-1"),
            ("workflow_runs", "next_wake_at", "-1"),
            ("workflow_runs", "finished_at", "9"),
            ("workflow_runs", "trigger_json", "'invalid'"),
            ("workflow_runs", "outcome_code", "''"),
            ("run_steps", "state", "'unknown'"),
            ("run_steps", "index_no", "-1"),
            ("run_steps", "attempt", "-1"),
            ("run_steps", "attempt", "6"),
            ("run_steps", "index_no", "32"),
            ("run_steps", "output_json", "'invalid'"),
            ("run_steps", "finished_at", "9"),
            ("run_steps", "operation_ref", "''"),
            ("run_steps", "error_code", "''"),
            ("workflow_schedule", "last_occurrence", "-1"),
            ("notifications", "severity", "'unknown'"),
            ("notifications", "title", "''"),
            ("notifications", "source", "''"),
            ("notifications", "created_at", "-1"),
            ("notification_state", "read_at", "-1"),
            ("notification_state", "acknowledged_at", "-1"),
        ] {
            assert!(c.execute(&format!("UPDATE {table} SET {column}={value}"), []).is_err(), "{table}.{column} accepted {value}");
        }
        for (table, column, limit, json) in [
            ("workflows", "name", 256, false),
            ("workflows", "principal_id", 128, false),
            ("workflows", "definition_json", 65536, true),
            ("workflow_runs", "definition_json", 65536, true),
            ("workflow_runs", "trigger_json", 65536, true),
            ("run_steps", "output_json", 65536, true),
            ("notifications", "title", 256, false),
            ("notifications", "body", 8192, false),
            ("notifications", "source", 128, false),
        ] {
            let value = if json { format!("\"{}\"", "x".repeat(limit - 2)) } else { "x".repeat(limit) };
            let sql = format!("UPDATE {table} SET {column}=?");
            c.execute(&sql, [&value])?;
            let too_long = if json { format!("\"{}\"", "x".repeat(limit - 1)) } else { format!("{value}x") };
            assert!(c.execute(&sql, [&too_long]).is_err(), "{table}.{column} overflow");
            // A character count would accept this, but UTF-8 bytes exceed the bound.
            let unicode = if json { format!("\"{}\"", "é".repeat(limit / 2)) } else { "é".repeat(limit / 2 + 1) };
            assert!(c.execute(&sql, [&unicode]).is_err(), "{table}.{column} UTF-8 overflow");
        }
        Ok(())
    }).unwrap();
}

#[test]
fn failed_v18_migration_rolls_back_tables_and_version() {
    let f = Fixture::open();
    let directory = f.db.directory().clone();
    f.db.write(|c| {
        c.execute_batch(support::REMOVE_V23_SCHEMA)?;
        c.execute_batch(support::REMOVE_V22_SCHEMA)?;
        c.execute_batch(support::REMOVE_V18_SCHEMA)?;
        // A conflicting legacy object forces failure after preceding DDL has run.
        c.execute_batch(
            "CREATE TABLE notifications(conflict TEXT) STRICT; PRAGMA user_version=17;",
        )?;
        Ok(())
    })
    .unwrap();
    f.db.close().unwrap();
    assert!(StateDatabase::open(directory, f.clock.clone()).is_err());
    let raw = rusqlite::Connection::open(f.state_path().join("vesta-state.db")).unwrap();
    assert_eq!(
        raw.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        17
    );
    assert_eq!(
        raw.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='workflows'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        raw.query_row(
            "SELECT count(*) FROM role_capabilities WHERE capability='automation.manage'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
}
