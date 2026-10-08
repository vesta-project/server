mod support;

use rusqlite::{Connection, params};
use std::{collections::BTreeSet, fs, sync::Arc};
use support::TestClock;
use vesta_state::{
    CURRENT_SCHEMA_VERSION, StateDatabase, StateDirectory,
    audit::{AuditActor, AuditLog, AuditRecord},
};

const FIXED_TIME_MS: i64 = 1_791_201_600_000; // 2026-10-05T12:00:00Z
const EXPIRY_NOW_MS: i64 = 1_000;

type DeviceRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    Option<i64>,
    Option<String>,
);
type AssignmentRow = (
    String,
    String,
    String,
    String,
    i64,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<i64>,
);
type OverrideRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<i64>,
);
type PasswordRow = (String, i64, Option<String>, i64, i64, Option<i64>);

#[derive(Debug, PartialEq)]
struct PersistedRows {
    server: (String, i64),
    principals: Vec<(String, String, String, String, i64, i64)>,
    devices: Vec<DeviceRow>,
    assignments: Vec<AssignmentRow>,
    overrides: Vec<OverrideRow>,
    pack_password: Option<PasswordRow>,
    audit: Vec<Vec<rusqlite::types::Value>>,
}

fn persisted_rows(connection: &Connection) -> rusqlite::Result<PersistedRows> {
    let server = connection.query_row(
        "SELECT server_id,created_at FROM server_identity WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let principals = connection
        .prepare(
            "SELECT id,server_id,display_name,status,created_at,updated_at FROM principals ORDER BY id",
        )?
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
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let devices = connection
        .prepare(
            "SELECT id,server_id,principal_id,label,public_key_sha256,created_at,revoked_at,revoked_by \
             FROM devices ORDER BY id",
        )?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let assignments = connection
        .prepare(
            "SELECT id,principal_id,role_id,granted_by,created_at,expires_at,revoked_at,revoked_by,expiry_recorded_at \
             FROM role_assignments ORDER BY id",
        )?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let overrides = connection
        .prepare(
            "SELECT id,principal_id,capability,scope,effect,granted_by,created_at,expires_at,revoked_at,revoked_by,expiry_recorded_at \
             FROM principal_overrides ORDER BY id",
        )?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let pack_password = connection
        .query_row(
            "SELECT server_id,revision,phc,created_at,updated_at,revoked_at FROM pack_passwords",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .map(Some)?;
    let audit = connection
        .prepare(
            "SELECT seq,server_id,principal_id,device_id,source,action,target,outcome,operation_id,at,metadata_json,prev_hash,hash \
             FROM audit_events ORDER BY seq",
        )?
        .query_map([], |row| {
            (0..row.as_ref().column_count())
                .map(|index| row.get::<_, rusqlite::types::Value>(index))
                .collect::<rusqlite::Result<Vec<_>>>()
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(PersistedRows {
        server,
        principals,
        devices,
        assignments,
        overrides,
        pack_password,
        audit,
    })
}

fn seed_v4_data(db: &StateDatabase) {
    db.write(|connection| {
        connection.execute(
            "INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) \
             VALUES ('principal-expiry-fixture',?1,'Expiry fixture','active',10,11)",
            [db.server_id()],
        )?;
        connection.execute(
            "INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) \
             VALUES ('device-expiry-fixture',?1,'principal-expiry-fixture','fixture device',?2,12)",
            params![db.server_id(), "a".repeat(64)],
        )?;
        connection.execute(
            "INSERT INTO role_assignments \
             (id,principal_id,role_id,granted_by,created_at,expires_at,revoked_at,revoked_by,expiry_recorded_at) \
             VALUES ('assignment-pending','principal-expiry-fixture','viewer','owner',13,900,NULL,NULL,NULL)",
            [],
        )?;
        connection.execute(
            "INSERT INTO role_assignments \
             (id,principal_id,role_id,granted_by,created_at,expires_at,revoked_at,revoked_by,expiry_recorded_at) \
             VALUES ('assignment-future','principal-expiry-fixture','viewer','owner',14,2000,NULL,NULL,NULL)",
            [],
        )?;
        connection.execute(
            "INSERT INTO principal_overrides \
             (id,principal_id,capability,scope,effect,granted_by,created_at,expires_at,revoked_at,revoked_by,expiry_recorded_at) \
             VALUES ('override-pending','principal-expiry-fixture','status.view','*','allow','owner',15,900,NULL,NULL,NULL)",
            [],
        )?;
        connection.execute(
            "INSERT INTO principal_overrides \
             (id,principal_id,capability,scope,effect,granted_by,created_at,expires_at,revoked_at,revoked_by,expiry_recorded_at) \
             VALUES ('override-future','principal-expiry-fixture','status.view','*','allow','owner',16,2000,NULL,NULL,NULL)",
            [],
        )?;
        connection.execute(
            "INSERT INTO pack_passwords (server_id,revision,phc,created_at,updated_at,revoked_at) \
             VALUES (?1,7,NULL,17,18,19)",
            [db.server_id()],
        )?;

        // Historical rows are deliberately numerous so the real planner has a meaningful
        // pending subset to choose from. They remain valid FK-backed rows, not planner mocks.
        for index in 0..64 {
            let revoked_assignment = format!("assignment-revoked-{index:03}");
            let recorded_assignment = format!("assignment-recorded-{index:03}");
            let revoked_override = format!("override-revoked-{index:03}");
            let recorded_override = format!("override-recorded-{index:03}");
            let past = 20 + index;
            let revoked_at = 60 + index;
            connection.execute(
                "INSERT INTO role_assignments \
                 (id,principal_id,role_id,granted_by,created_at,expires_at,revoked_at,revoked_by,expiry_recorded_at) \
                 VALUES (?1,'principal-expiry-fixture','viewer','owner',?2,?3,?4,'owner',NULL)",
                params![revoked_assignment, 20 + index, past, revoked_at],
            )?;
            connection.execute(
                "INSERT INTO role_assignments \
                 (id,principal_id,role_id,granted_by,created_at,expires_at,revoked_at,revoked_by,expiry_recorded_at) \
                 VALUES (?1,'principal-expiry-fixture','viewer','owner',?2,?3,NULL,NULL,?4)",
                params![recorded_assignment, 30 + index, past, revoked_at],
            )?;
            connection.execute(
                "INSERT INTO principal_overrides \
                 (id,principal_id,capability,scope,effect,granted_by,created_at,expires_at,revoked_at,revoked_by,expiry_recorded_at) \
                 VALUES (?1,'principal-expiry-fixture','status.view','*','allow','owner',?2,?3,?4,'owner',NULL)",
                params![revoked_override, 40 + index, past, revoked_at],
            )?;
            connection.execute(
                "INSERT INTO principal_overrides \
                 (id,principal_id,capability,scope,effect,granted_by,created_at,expires_at,revoked_at,revoked_by,expiry_recorded_at) \
                 VALUES (?1,'principal-expiry-fixture','status.view','*','allow','owner',?2,?3,NULL,NULL,?4)",
                params![recorded_override, 50 + index, past, revoked_at],
            )?;
        }
        Ok(())
    })
    .expect("seed FK-valid identity, grant, and tombstone rows");

    let audit = AuditLog::new(db.clone()).expect("construct audit log on empty chain");
    let record = AuditRecord::of(
        AuditActor::system("identity-expiry-migration-test".into())
            .expect("valid system audit actor"),
        "identity.expiry.migration_fixture".into(),
        "schema:v4".into(),
        "ok".into(),
        None,
    )
    .expect("valid migration-fixture audit record");
    audit
        .append_record(&record)
        .expect("append seed audit through the real audit API");
}

fn user_version(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row("PRAGMA user_version", [], |row| row.get(0))
}

fn index_definitions(connection: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    connection
        .prepare(
            "SELECT name,sql FROM sqlite_master WHERE type='index' AND sql IS NOT NULL ORDER BY name",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

fn normalized_sql(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn assert_backup_checks(connection: &Connection) {
    let mut integrity = connection
        .prepare("PRAGMA integrity_check")
        .expect("prepare backup integrity check");
    let results = integrity
        .query_map([], |row| row.get::<_, String>(0))
        .expect("run backup integrity check")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read all integrity results");
    assert_eq!(
        results,
        vec!["ok".to_owned()],
        "the v4 backup must pass integrity_check"
    );

    let mut foreign_keys = connection
        .prepare("PRAGMA foreign_key_check")
        .expect("prepare backup foreign-key check");
    let violations = foreign_keys
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .expect("run backup foreign-key check")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read all foreign-key violations");
    assert!(
        violations.is_empty(),
        "v4 backup FK violations: {violations:?}"
    );
}

const ASSIGNMENT_EXPIRY_QUERY: &str = "SELECT g.id, g.principal_id FROM role_assignments g JOIN principals p ON p.id = g.principal_id \
     WHERE g.expires_at IS NOT NULL AND g.expires_at <= ? AND g.revoked_at IS NULL \
     AND g.expiry_recorded_at IS NULL AND p.server_id = ? ORDER BY g.expires_at, g.id LIMIT ?";
const OVERRIDE_EXPIRY_QUERY: &str = "SELECT g.id, g.principal_id FROM principal_overrides g JOIN principals p ON p.id = g.principal_id \
     WHERE g.expires_at IS NOT NULL AND g.expires_at <= ? AND g.revoked_at IS NULL \
     AND g.expiry_recorded_at IS NULL AND p.server_id = ? ORDER BY g.expires_at, g.id LIMIT ?";

fn query_plan(
    connection: &Connection,
    query: &str,
    server_id: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut statement = connection.prepare(&format!("EXPLAIN QUERY PLAN {query}"))?;
    statement
        .query_map(params![EXPIRY_NOW_MS, server_id, 500], |row| row.get(3))?
        .collect()
}

fn pending_ids(
    connection: &Connection,
    query: &str,
    server_id: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut statement = connection.prepare(query)?;
    statement
        .query_map(params![EXPIRY_NOW_MS, server_id, 500], |row| row.get(0))?
        .collect()
}

#[test]
fn schema_four_to_five_preserves_data_backup_and_pending_expiry_plans() {
    let temp = tempfile::tempdir().expect("create local test root");
    let directory = StateDirectory::new(temp.path().join("private state"), None)
        .expect("create private state directory");
    let clock = Arc::new(TestClock::new(FIXED_TIME_MS));

    // Start with a genuine current database and seed all rows through real SQLite and audit APIs.
    let db = StateDatabase::open(directory.clone(), clock.clone()).expect("open fresh v5 state");
    assert_eq!(
        db.schema_version().expect("read schema version"),
        CURRENT_SCHEMA_VERSION
    );
    assert_eq!(CURRENT_SCHEMA_VERSION, 5);
    seed_v4_data(&db);
    let expected_rows = db
        .read(|connection| Ok(persisted_rows(connection)?))
        .expect("snapshot seeded identity and audit rows");
    assert_eq!(expected_rows.audit.len(), 1);
    assert_eq!(
        expected_rows.pack_password,
        Some((db.server_id().to_owned(), 7, None, 17, 18, Some(19)))
    );
    let server_id = db.server_id().to_owned();
    db.close().expect("close before constructing v4 fixture");

    // The trusted fixture connection removes only v5's two indexes and restores v4's version.
    let raw = Connection::open(directory.database_file()).expect("open local fixture database");
    raw.execute_batch(
        "DROP INDEX role_assignments_pending_expiry;\
         DROP INDEX principal_overrides_pending_expiry;\
         PRAGMA user_version=4;",
    )
    .expect("remove only v5 indexes and set the v4 schema marker");
    assert_eq!(user_version(&raw).expect("read fixture version"), 4);
    let v4_index_names = index_definitions(&raw)
        .expect("list v4 indexes")
        .into_iter()
        .map(|(name, _)| name)
        .collect::<BTreeSet<_>>();
    assert!(!v4_index_names.contains("role_assignments_pending_expiry"));
    assert!(!v4_index_names.contains("principal_overrides_pending_expiry"));
    drop(raw);

    // Reopening exercises the real pre-migration backup and v4->v5 migration path.
    let migrated = StateDatabase::open(directory.clone(), clock.clone())
        .expect("migrate genuine v4 schema to v5");
    assert_eq!(migrated.schema_version().expect("read migrated version"), 5);
    assert_eq!(migrated.server_id(), server_id);
    assert_eq!(
        migrated
            .read(|connection| Ok(persisted_rows(connection)?))
            .expect("read migrated rows"),
        expected_rows
    );
    let audit = AuditLog::new(migrated.clone()).expect("reopen valid migrated audit chain");
    let verification = audit.verify_chain().expect("verify migrated audit chain");
    assert!(verification.valid);
    assert_eq!(verification.checked, 1);

    let post_indexes = migrated
        .read(|connection| Ok(index_definitions(connection)?))
        .expect("read migrated index definitions");
    let post_index_names = post_indexes
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    let prior_index_names = v4_index_names
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let added = post_index_names
        .difference(&prior_index_names)
        .copied()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        added,
        BTreeSet::from([
            "principal_overrides_pending_expiry",
            "role_assignments_pending_expiry",
        ]),
        "v5 must add exactly its two pending-expiry indexes",
    );
    let assignment_ddl = post_indexes
        .iter()
        .find(|(name, _)| name == "role_assignments_pending_expiry")
        .expect("assignment pending-expiry index exists");
    assert_eq!(
        normalized_sql(&assignment_ddl.1),
        "create index role_assignments_pending_expiry on role_assignments(expires_at, id) where expires_at is not null and revoked_at is null and expiry_recorded_at is null"
    );
    let override_ddl = post_indexes
        .iter()
        .find(|(name, _)| name == "principal_overrides_pending_expiry")
        .expect("override pending-expiry index exists");
    assert_eq!(
        normalized_sql(&override_ddl.1),
        "create index principal_overrides_pending_expiry on principal_overrides(expires_at, id) where expires_at is not null and revoked_at is null and expiry_recorded_at is null"
    );

    let (assignment_plan, override_plan, assignments, overrides) = migrated
        .read(|connection| {
            Ok((
                query_plan(connection, ASSIGNMENT_EXPIRY_QUERY, &server_id)?,
                query_plan(connection, OVERRIDE_EXPIRY_QUERY, &server_id)?,
                pending_ids(connection, ASSIGNMENT_EXPIRY_QUERY, &server_id)?,
                pending_ids(connection, OVERRIDE_EXPIRY_QUERY, &server_id)?,
            ))
        })
        .expect("explain and execute the production expiry queries");
    for (plan, index) in [
        (&assignment_plan, "role_assignments_pending_expiry"),
        (&override_plan, "principal_overrides_pending_expiry"),
    ] {
        assert!(
            plan.iter().any(|detail| detail.contains(index)),
            "query plan must use {index}; actual detail rows: {plan:?}"
        );
        assert!(
            !plan.iter().any(|detail| detail
                .to_ascii_uppercase()
                .contains("USE TEMP B-TREE FOR ORDER BY")),
            "indexed expiry ordering must not require a temporary B-tree: {plan:?}"
        );
    }
    assert_eq!(assignments, vec!["assignment-pending".to_owned()]);
    assert_eq!(overrides, vec!["override-pending".to_owned()]);
    assert!(
        assignments
            .iter()
            .all(|id| !id.contains("revoked") && !id.contains("recorded"))
    );
    assert!(
        overrides
            .iter()
            .all(|id| !id.contains("revoked") && !id.contains("recorded"))
    );

    migrated
        .close()
        .expect("close migrated database before backup inspection");
    let backups = fs::read_dir(directory.backups_dir())
        .expect("list migration backups")
        .map(|entry| entry.expect("read backup directory entry").path())
        .collect::<Vec<_>>();
    assert_eq!(
        backups.len(),
        1,
        "one verified v4 pre-migration backup is expected"
    );
    let backup = Connection::open(&backups[0]).expect("open original v4 backup");
    assert_eq!(
        user_version(&backup).expect("read backup schema version"),
        4
    );
    let journal_mode: String = backup
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read backup journal mode");
    assert_eq!(journal_mode.to_ascii_lowercase(), "delete");
    assert_backup_checks(&backup);
    assert_eq!(
        persisted_rows(&backup).expect("read backup rows"),
        expected_rows
    );
    let backup_indexes = index_definitions(&backup)
        .expect("read original v4 backup indexes")
        .into_iter()
        .map(|(name, _)| name)
        .collect::<BTreeSet<_>>();
    assert!(!backup_indexes.contains("role_assignments_pending_expiry"));
    assert!(!backup_indexes.contains("principal_overrides_pending_expiry"));
}
