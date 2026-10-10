mod support;

use rusqlite::{Connection, params, types::Value};
use support::{DEFAULT_TEST_TIME_MS, Fixture};
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};

const PRESERVED_TABLES: &[&str] = &[
    "server_identity",
    "principals",
    "devices",
    "roles",
    "role_capabilities",
    "role_assignments",
    "management_device_keys",
    "management_pairing_codes",
    "management_owner_links",
    "user_minecraft_accounts",
    "user_default_roles",
    "device_password_grants",
    "access_role_sources",
    "pack_releases",
];

fn rows(connection: &Connection) -> rusqlite::Result<Vec<Vec<Vec<Value>>>> {
    PRESERVED_TABLES
        .iter()
        .map(|table| {
            let mut statement =
                connection.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
            let columns = statement.column_count();
            statement
                .query_map([], |row| {
                    (0..columns).map(|column| row.get(column)).collect()
                })?
                .collect()
        })
        .collect()
}

#[test]
fn populated_v12_to_v13_preserves_existing_rows_and_adds_empty_pack_management_tables() {
    let fixture = Fixture::open();
    let directory = fixture.db.directory().clone();
    let server = fixture.db.server_id().to_owned();
    fixture.db.write(|connection| {
        connection.execute("INSERT INTO principals VALUES ('preserved-user',?,'Preserved','active',17,18)", [&server])?;
        connection.execute("INSERT INTO devices VALUES ('preserved-device',?,'preserved-user','Preserved device',?,19,NULL,NULL)", params![server,"a".repeat(64)])?;
        connection.execute("INSERT INTO roles VALUES ('preserved-role','Preserved role',23,0,'local-owner',20)", [])?;
        connection.execute("INSERT INTO role_capabilities VALUES ('preserved-role','status.view','*')", [])?;
        connection.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES ('preserved-assignment','preserved-user','preserved-role','local-owner',21)", [])?;
        connection.execute("INSERT INTO management_device_keys VALUES ('preserved-device',?,?,'preserved-uuid')", params![server,vec![7u8;32]])?;
        connection.execute("INSERT INTO management_pairing_codes VALUES (?,?,'viewer',?,?)", params![vec![8u8;32],server,DEFAULT_TEST_TIME_MS,DEFAULT_TEST_TIME_MS+60_000])?;
        connection.execute("INSERT INTO management_owner_links VALUES (?,'preserved-uuid','owner',22)", [&server])?;
        connection.execute("INSERT INTO user_minecraft_accounts VALUES (?,'preserved-uuid','preserved-user')", [&server])?;
        connection.execute("INSERT INTO user_default_roles VALUES ('preserved-user','public')", [])?;
        connection.execute("INSERT INTO device_password_grants VALUES ('preserved-device','player',9)", [])?;
        connection.execute("INSERT INTO pack_releases VALUES (?,7,3,'preserved-pack','catalogue',?,321,23)", params![server,"b".repeat(64)])?;
        // Reconstruct v12 by removing all later management outcome tables.
        connection.execute_batch("DROP TABLE management_file_outcomes; DROP TABLE management_pack_outcomes; DROP TABLE pack_minimum_policy; PRAGMA user_version=12;")?;
        Ok(())
    }).unwrap();
    let before = fixture.db.read(|connection| Ok(rows(connection)?)).unwrap();
    fixture.db.close().unwrap();

    let upgraded = StateDatabase::open(directory, fixture.clock.clone()).unwrap();
    assert_eq!(upgraded.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(
        upgraded.schema_version().unwrap(),
        vesta_state::CURRENT_SCHEMA_VERSION
    );
    assert_eq!(upgraded.server_id(), server);
    upgraded
        .read(|connection| {
            assert_eq!(
                rows(connection)?,
                before,
                "v12 identity, authority and release rows must survive unchanged"
            );
            for table in ["management_pack_outcomes", "pack_minimum_policy"] {
                assert_eq!(
                    connection
                        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                            .get::<_, i64>(0))?,
                    0
                );
            }
            assert_eq!(
                connection.query_row(
                    "SELECT count(*) FROM pragma_foreign_key_check",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .unwrap();
    upgraded.close().unwrap();
}
