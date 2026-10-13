mod support;

use std::{io::Cursor, sync::Arc};
use support::Fixture;
use vesta_state::{
    StateDatabase, StateDirectory,
    audit::{AuditActor, AuditLog, AuditRecord},
    export::{RecoveryExport, import_recovery_export, read_recovery_export, write_recovery_export},
};

fn exported(fixture: &Fixture) -> RecoveryExport {
    let mut bytes = Vec::new();
    write_recovery_export(&fixture.db, &mut bytes, None).unwrap();
    read_recovery_export(&mut Cursor::new(bytes), None).unwrap()
}

fn actor() -> AuditActor {
    AuditActor::new("owner".into(), "device".into(), "local-cli".into()).unwrap()
}

fn populated_source() -> (Fixture, RecoveryExport) {
    let source = Fixture::with_time(1_700_000_000_000);
    let server_id = source.db.server_id().to_owned();
    source.db.write(|c| {
        c.execute(
            "INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES ('recover-principal',?1,'Recovery principal','active',10,11)",
            [&server_id],
        )?;
        c.execute(
            "INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES ('recover-assignment','recover-principal','viewer','fixture-owner',12)",
            [],
        )?;
        c.execute(
            "INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES ('recover-device',?1,'recover-principal','Recovery device',?2,13)",
            rusqlite::params![&server_id, "b".repeat(64)],
        )?;
        c.execute(
            "INSERT INTO management_device_keys (device_id,server_id,public_key,linked_minecraft_uuid) VALUES ('recover-device',?1,?2,NULL)",
            rusqlite::params![&server_id, vec![0xabu8; 32]],
        )?;
        c.execute(
            "INSERT INTO settings (server_id,key,desired_value,desired_revision,apply_mode,effective_value,effective_revision,updated_at) VALUES (?1,'game.memory','4G',1,'on_restart',NULL,0,14)",
            [&server_id],
        )?;
        c.execute(
            "INSERT INTO backup_policy (singleton,policy_json,revision,updated_at,updated_by) VALUES (1,'{\"profiles\":[]}',1,15,'fixture-owner')",
            [],
        )?;
        c.execute("INSERT INTO user_minecraft_accounts (server_id,minecraft_uuid,principal_id) VALUES (?,'11111111-2222-4333-8444-555555555555','recover-principal')", [&server_id])?;
        c.execute("INSERT INTO pack_minimum_policy (server_id,minimum_revision) VALUES (?,1)", [&server_id])?;
        c.execute_batch("INSERT INTO user_minecraft_names (principal_id,name) VALUES ('recover-principal','Recovery');
            INSERT INTO user_default_roles (principal_id,role_id) VALUES ('recover-principal','player');
            INSERT INTO roles (id,name,rank,builtin,created_by,created_at) VALUES ('custom-recovery','Recovery custom',20,0,'fixture-owner',16);
            INSERT INTO role_capabilities (role_id,capability,scope) VALUES ('custom-recovery','pack.download','*');
            UPDATE role_assignments SET role_id='custom-recovery' WHERE id='recover-assignment';
            INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES ('override-recovery','recover-principal','public.info','*','deny','fixture-owner',17);
            INSERT INTO workflows (id,name,enabled,principal_id,definition_json,created_at,updated_at,revision) VALUES ('workflow-recovery','Recovery workflow',0,'recover-principal','{}',18,18,1);
            INSERT INTO workflow_schedule (workflow_id,last_occurrence) VALUES ('workflow-recovery',19);
            INSERT INTO notifications (id,created_at,severity,title,body,source) VALUES ('notification-recovery',20,'info','Recovery notification','Restored inbox','fixture');
            INSERT INTO notification_state (principal_id,notification_id,read_at,acknowledged_at) VALUES ('recover-principal','notification-recovery',21,22);")?;
        Ok(())
    }).unwrap();
    AuditLog::new(source.db.clone())
        .unwrap()
        .append_record(
            &AuditRecord::of(
                actor(),
                "fixture.seed".into(),
                "recovery".into(),
                "ok".into(),
                None,
            )
            .unwrap(),
        )
        .unwrap();
    let export = exported(&source);
    (source, export)
}

fn reopen(directory: StateDirectory, clock: Arc<support::TestClock>) -> StateDatabase {
    StateDatabase::open(directory, clock).unwrap()
}

fn rows<'a>(export: &'a RecoveryExport, table: &str) -> &'a [Vec<serde_json::Value>] {
    &export.tables[table].rows
}

#[test]
fn import_restores_included_rows_and_binary_management_key() {
    let (source, source_export) = populated_source();
    let target = Fixture::with_time(1_800_000_000_000);
    assert_ne!(source.db.server_id(), target.db.server_id());
    let directory = target.db.directory().clone();
    let target_clock = target.clock.clone();
    target.db.close().unwrap();

    let summary =
        import_recovery_export(directory.clone(), target_clock.clone(), &source_export).unwrap();
    assert_eq!(summary.server_id, source_export.server_id);
    assert!(summary.tables > 0);
    let db = reopen(directory, target_clock);
    assert_eq!(db.server_id(), source_export.server_id);
    let actual = {
        let mut bytes = Vec::new();
        write_recovery_export(&db, &mut bytes, None).unwrap();
        read_recovery_export(&mut Cursor::new(bytes), None).unwrap()
    };

    for table in source_export
        .tables
        .keys()
        .filter(|name| name.as_str() != "roles" && name.as_str() != "audit_events")
    {
        let canonical = |rows: &[Vec<serde_json::Value>]| {
            let mut rows = rows
                .iter()
                .map(|row| serde_json::to_string(row).unwrap())
                .collect::<Vec<_>>();
            rows.sort();
            rows
        };
        assert_eq!(
            canonical(rows(&actual, table)),
            canonical(rows(&source_export, table)),
            "table {table}"
        );
    }
    assert_eq!(
        &rows(&actual, "audit_events")[..rows(&source_export, "audit_events").len()],
        rows(&source_export, "audit_events")
    );
    db.read(|c| {
        let key: Vec<u8> = c.query_row(
            "SELECT public_key FROM management_device_keys WHERE device_id='recover-device'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(key, vec![0xab; 32]);
        Ok(())
    })
    .unwrap();
    let source_roles = rows(&source_export, "roles");
    let actual_roles = rows(&actual, "roles");
    let id_column = source_export.tables["roles"]
        .columns
        .iter()
        .position(|column| column == "id")
        .unwrap();
    let created_by_column = source_export.tables["roles"]
        .columns
        .iter()
        .position(|column| column == "created_by")
        .unwrap();
    let created_at_column = source_export.tables["roles"]
        .columns
        .iter()
        .position(|column| column == "created_at")
        .unwrap();
    for role in actual_roles {
        if role[3].as_i64() == Some(1) {
            let source_role = source_roles
                .iter()
                .find(|source_role| source_role[id_column] == role[id_column])
                .unwrap();
            assert_ne!(role[created_at_column], source_role[created_at_column]);
            assert_eq!(role[created_at_column], 1_800_000_000_000i64);
            for (column, (expected, found)) in source_role.iter().zip(role).enumerate() {
                if column != created_by_column && column != created_at_column {
                    assert_eq!(found, expected, "builtin role column {column}");
                }
            }
        } else {
            assert_eq!(
                role,
                source_roles
                    .iter()
                    .find(|source_role| source_role[id_column] == role[id_column])
                    .unwrap()
            );
        }
    }
    let key_hex = rows(&actual, "management_device_keys")
        .iter()
        .find(|row| row[0] == "recover-device")
        .unwrap()[2]
        .as_str()
        .unwrap();
    assert_eq!(key_hex, "ab".repeat(32));

    let audit = AuditLog::new(db.clone()).unwrap();
    let entries = audit.page(0, 20).unwrap();
    assert_eq!(
        entries.len(),
        rows(&source_export, "audit_events").len() + 1
    );
    assert_eq!(entries.last().unwrap().action, "recovery.import");
    assert!(audit.verify_chain().unwrap().valid);
    db.close().unwrap();
    source.db.close().unwrap();
}

fn unchanged_after_rejection(export: &RecoveryExport, target: &Fixture, expected: &str) {
    let directory = target.db.directory().clone();
    let clock = target.clock.clone();
    target.db.close().unwrap();
    let error = import_recovery_export(directory.clone(), clock.clone(), export).unwrap_err();
    assert_eq!(error.code(), expected);
    let db = reopen(directory, clock);
    assert_eq!(db.server_id(), target.db.server_id());
    db.read(|c| {
        assert_eq!(
            c.query_row("SELECT count(*) FROM principals", [], |row| row
                .get::<_, i64>(0))?,
            0
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM audit_events", [], |row| row
                .get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .unwrap();
    db.close().unwrap();
}

#[test]
fn nonempty_target_is_refused_without_mutation() {
    let (_, export) = populated_source();
    let target = Fixture::with_time(1_800_000_000_000);
    target.db.write(|c| {
        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES ('existing',?1,'Existing','active',1,1)", [target.db.server_id()])?;
        Ok(())
    }).unwrap();
    let id = target.db.server_id().to_owned();
    let directory = target.db.directory().clone();
    let clock = target.clock.clone();
    target.db.close().unwrap();
    let error = import_recovery_export(directory.clone(), clock.clone(), &export).unwrap_err();
    assert_eq!(error.code(), "state_import_target_not_empty");
    let db = reopen(directory, clock);
    assert_eq!(db.server_id(), id);
    db.read(|c| {
        assert_eq!(
            c.query_row("SELECT id FROM principals", [], |row| row
                .get::<_, String>(0))?,
            "existing"
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM audit_events", [], |row| row
                .get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .unwrap();
    db.close().unwrap();
}

#[test]
fn mismatched_builtin_capability_and_schema_are_rejected() {
    let (_, export) = populated_source();
    let target = Fixture::with_time(1_800_000_000_000);
    let mut altered = export.clone();
    let capability = altered
        .tables
        .get_mut("role_capabilities")
        .unwrap()
        .rows
        .iter_mut()
        .find(|row| row[0] == "owner")
        .unwrap();
    capability[1] = serde_json::Value::String("owner.tampered".into());
    unchanged_after_rejection(&altered, &target, "state_import_builtin_mismatch");

    let target = Fixture::with_time(1_800_000_000_000);
    let mut wrong_schema = export;
    wrong_schema.schema_version += 1;
    unchanged_after_rejection(&wrong_schema, &target, "state_import_schema_mismatch");
}

#[test]
fn corrupt_audit_hash_rolls_back_identity_and_all_imported_rows() {
    let (_, mut export) = populated_source();
    let target = Fixture::with_time(1_800_000_000_000);
    let original_id = target.db.server_id().to_owned();
    let row = export
        .tables
        .get_mut("audit_events")
        .unwrap()
        .rows
        .first_mut()
        .unwrap();
    row[12] = serde_json::Value::String("tampered-hash".into());
    let directory = target.db.directory().clone();
    let clock = target.clock.clone();
    target.db.close().unwrap();
    let error = import_recovery_export(directory.clone(), clock.clone(), &export).unwrap_err();
    assert_eq!(error.code(), "state_audit_chain_corrupt");
    let db = reopen(directory, clock);
    assert_eq!(db.server_id(), original_id);
    db.read(|c| {
        assert_eq!(
            c.query_row("SELECT count(*) FROM principals", [], |row| row
                .get::<_, i64>(0))?,
            0
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM audit_events", [], |row| row
                .get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .unwrap();
    db.close().unwrap();
}
