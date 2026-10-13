mod support;

use rusqlite::{Connection, params};
use support::{DEFAULT_TEST_TIME_MS, Fixture};
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};

// Frozen from b950e75's original apply_v8, which has no file_uploads table.
const ORIGINAL_V8_TRASH: &str = r#"
CREATE TABLE file_trash (
    id TEXT PRIMARY KEY CHECK (length(id) = 32 AND id NOT GLOB '*[^0-9a-f]*'),
    server_id TEXT NOT NULL REFERENCES server_identity(server_id),
    path TEXT NOT NULL CHECK (length(CAST(path AS BLOB)) BETWEEN 1 AND 4096),
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    size INTEGER NOT NULL CHECK (size >= 0),
    reason TEXT NOT NULL CHECK (reason IN ('replace', 'delete')),
    principal_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'complete', 'recovery-required', 'restored')),
    effect_sha256 TEXT CHECK (effect_sha256 IS NULL OR (length(effect_sha256) = 64 AND effect_sha256 NOT GLOB '*[^0-9a-f]*')),
    unix_mode INTEGER CHECK (unix_mode IS NULL OR unix_mode BETWEEN 0 AND 4095),
    readonly INTEGER NOT NULL CHECK (readonly IN (0, 1)),
    trashed_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL CHECK (expires_at > trashed_at),
    CHECK ((reason = 'replace' AND effect_sha256 IS NOT NULL) OR (reason = 'delete' AND effect_sha256 IS NULL))
) STRICT
"#;

fn normalize(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn original_v8_without_uploads_migrates_to_current_and_preserves_existing_rows() {
    let fixture = Fixture::open();
    let directory = fixture.db.directory().clone();
    let server = fixture.db.server_id().to_owned();
    let trash = "a".repeat(32);
    let hash = "b".repeat(64);
    fixture.db.write(|c| {
        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES ('legacy-principal',?,'Preserved','active',?,?)", params![server, DEFAULT_TEST_TIME_MS, DEFAULT_TEST_TIME_MS])?;
        c.execute("INSERT INTO file_trash (id,server_id,path,sha256,size,reason,principal_id,device_id,status,effect_sha256,unix_mode,readonly,trashed_at,expires_at) VALUES (?,?,'config/old.toml',?,3,'delete','legacy-principal','legacy-device','complete',NULL,NULL,0,?,?)", params![trash, server, hash, DEFAULT_TEST_TIME_MS, DEFAULT_TEST_TIME_MS + 60000])?;
        Ok(())
    }).unwrap();
    fixture.db.close().unwrap();
    let offline = Connection::open(directory.database_file()).unwrap();
    let release_sql: String = offline
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='pack_releases'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let legacy_release_sql = release_sql.replace(", 'catalogue'", "");
    support::remove_v12_schema(&offline);
    offline
        .execute_batch(&format!("DROP TABLE pack_releases; {legacy_release_sql};"))
        .unwrap();
    // Restore v8: v9 adds uploads, and v10 widens the release-format constraint.
    offline
        .execute_batch("BEGIN IMMEDIATE; DROP TABLE file_intents; DROP TABLE file_uploads; PRAGMA user_version=8; COMMIT;")
        .unwrap();
    assert_eq!(
        offline
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        8
    );
    let old_trash: String = offline
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='file_trash'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(normalize(&old_trash), normalize(ORIGINAL_V8_TRASH));
    assert!(
        !offline
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='file_uploads')",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
    );
    let mut statement = offline
        .prepare("SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND name!='pack_releases' ORDER BY type,name")
        .unwrap();
    let old_schema = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    drop(statement);
    drop(offline);

    let migrated = StateDatabase::open(directory, fixture.clock.clone()).unwrap();
    assert_eq!(migrated.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(migrated.server_id(), server);
    migrated.write(|c| {
        assert_eq!(c.query_row("SELECT display_name FROM principals WHERE id='legacy-principal'", [], |r| r.get::<_, String>(0))?, "Preserved");
        assert_eq!(c.query_row("SELECT sha256 FROM file_trash WHERE id=?", [&trash], |r| r.get::<_, String>(0))?, hash);
        assert_eq!(c.query_row("SELECT count(*) FROM file_uploads", [], |r| r.get::<_, i64>(0))?, 0);
        let mut statement = c.prepare("SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT IN ('file_uploads','pack_releases','file_intents','file_intents_pending','management_device_keys','management_pairing_codes','management_owner_links','user_minecraft_accounts','user_default_roles','device_password_grants','access_role_sources','management_file_outcomes','management_file_outcomes_retention','management_pack_outcomes','pack_minimum_policy','management_permission_receipts','user_minecraft_names','management_invitation_issuers','management_permission_receipts_retention','workflows','workflow_runs','workflow_runs_active','workflow_runs_due','workflow_runs_history','run_steps','workflow_schedule','notifications','notifications_created','notification_state','notification_state_notification','backup_jobs','backup_jobs_queue','backup_jobs_history','backup_policy','moderation_actions','moderation_expiry','moderation_history') ORDER BY type,name")?;
        let preserved = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(preserved, old_schema);
        let insert = "INSERT INTO file_uploads (id,server_id,principal_id,device_id,path,expected_sha256,total_size,final_sha256,expires_at) VALUES (?,?,'legacy-principal','legacy-device','config/new.toml',NULL,3,?,?)";
        let id = "c".repeat(32);
        c.execute(insert, params![id, server, hash, DEFAULT_TEST_TIME_MS + 60000])?;
        for (column, value) in [("id", "'invalid'"), ("server_id", "'missing-server'"), ("path", "''"), ("total_size", "-1"), ("total_size", "16777217"), ("final_sha256", "'invalid'"), ("expected_sha256", "'invalid'")] {
            assert!(c.execute(&format!("UPDATE file_uploads SET {column}={value} WHERE id=?"), [&id]).is_err(), "constraint {column}={value}");
        }
        Ok(())
    }).unwrap();
    migrated.close().unwrap();
}
