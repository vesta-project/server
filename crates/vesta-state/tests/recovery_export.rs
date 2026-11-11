mod support;

use serde_json::Value;
use std::fs::{self, File};
use vesta_state::{CURRENT_SCHEMA_VERSION, export::write_recovery_export};

#[test]
fn recovery_export_includes_identity_and_roles_but_not_secret_tables() {
    let fixture = support::Fixture::open();
    let server_id = fixture.db.server_id().to_owned();
    fixture
        .db
        .write(|connection| {
            connection.execute(
                "INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) \
                 VALUES ('export-principal',?1,'Recovery principal','active',10,11)",
                [&server_id],
            )?;
            connection.execute(
                "INSERT INTO role_assignments \
                 (id,principal_id,role_id,granted_by,created_at) \
                 VALUES ('export-assignment','export-principal','viewer','fixture-owner',12)",
                [],
            )?;
            connection.execute(
                "INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) \
                 VALUES ('export-device',?1,'export-principal','Export device',?2,12)",
                rusqlite::params![&server_id, "a".repeat(64)],
            )?;
            connection.execute(
                "INSERT INTO management_device_keys (device_id,server_id,public_key,linked_minecraft_uuid) \
                 VALUES ('export-device',?1,?2,NULL)",
                rusqlite::params![&server_id, vec![0xabu8; 32]],
            )?;
            connection.execute(
                "INSERT INTO pack_passwords \
                 (server_id,revision,phc,created_at,updated_at,revoked_at) \
                 VALUES (?1,1,?2,13,14,NULL)",
                [&server_id, &"unique-pack-secret-do-not-export".to_owned()],
            )?;
            connection.execute(
                "INSERT INTO bootstrap_codes \
                 (id,server_id,purpose,code_hash,created_at,expires_at) \
                 VALUES ('export-bootstrap',?1,'owner_recovery',?2,15,1000)",
                rusqlite::params![
                    server_id,
                    format!("{:<64}", "unique-bootstrap-secret-do-not-export")
                ],
            )?;
            connection.execute("INSERT INTO whitelist_sync_actor VALUES(1,'export-principal','export-device')", [])?;
            connection.execute("INSERT INTO whitelist_sync_members VALUES('11111111-2222-4333-8444-555555555555','OwnedSyncMember')", [])?;
            connection.execute("INSERT INTO management_pairing_codes(code_hash,server_id,role_id,created_at,expires_at) VALUES(?,?,'player',1,2)", rusqlite::params![vec![0xcdu8; 32], &server_id])?;
            connection.execute("INSERT INTO management_invitation_uses VALUES(?,3,1)", [vec![0xcdu8; 32]])?;
            Ok(())
        })
        .unwrap();

    let output = fixture.root.path().join("recovery-export.json");
    let mut file = File::create(&output).unwrap();
    let summary = write_recovery_export(&fixture.db, &mut file, None).unwrap();
    drop(file);
    assert!(output.is_file());
    assert!(summary.tables > 0);

    let bytes = fs::read(&output).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("unique-pack-secret-do-not-export"));
    assert!(!text.contains("unique-bootstrap-secret-do-not-export"));
    let export: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(export["format"], "vesta-recovery-export");
    assert_eq!(export["version"], 1);
    assert_eq!(export["schema_version"], CURRENT_SCHEMA_VERSION);
    assert_eq!(export["server_id"], server_id);
    assert!(export["exported_at_ms"].as_i64().is_some());

    let tables = export["tables"].as_object().unwrap();
    let principal_table = &tables["principals"];
    let principal_id = principal_table["columns"]
        .as_array()
        .unwrap()
        .iter()
        .position(|column| column == "id")
        .unwrap();
    let principals = principal_table["rows"].as_array().unwrap();
    assert!(
        principals
            .iter()
            .any(|row| row[principal_id] == "export-principal")
    );

    let assignment_table = &tables["role_assignments"];
    let assignment_id = assignment_table["columns"]
        .as_array()
        .unwrap()
        .iter()
        .position(|column| column == "id")
        .unwrap();
    let assignments = assignment_table["rows"].as_array().unwrap();
    assert!(
        assignments
            .iter()
            .any(|row| row[assignment_id] == "export-assignment")
    );

    let device_key_table = &tables["management_device_keys"];
    let device_key_columns = device_key_table["columns"].as_array().unwrap();
    let device_id = device_key_columns
        .iter()
        .position(|column| column == "device_id")
        .unwrap();
    let public_key_hex = device_key_columns
        .iter()
        .position(|column| column == "public_key_hex")
        .unwrap();
    assert!(
        !device_key_columns
            .iter()
            .any(|column| column == "public_key")
    );
    let device_keys = device_key_table["rows"].as_array().unwrap();
    let exported_key = device_keys
        .iter()
        .find(|row| row[device_id] == "export-device")
        .unwrap();
    assert_eq!(exported_key[public_key_hex], "ab".repeat(32));
    assert!(!tables.contains_key("pack_passwords"));
    assert!(!tables.contains_key("bootstrap_codes"));
    assert!(!tables.contains_key("management_invitation_uses"));
    assert!(!tables.contains_key("whitelist_sync_actor"));
    assert!(!tables.contains_key("whitelist_sync_members"));
    assert!(!text.contains("OwnedSyncMember"));
}
