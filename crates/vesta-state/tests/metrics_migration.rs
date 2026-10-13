mod support;

use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};

#[test]
fn v22_upgrade_creates_metrics_state_and_keeps_it_out_of_recovery_exports() {
    let fixture = support::Fixture::open();
    let directory = fixture.db.directory().clone();
    fixture
        .db
        .write(|connection| {
            connection.execute_batch(support::REMOVE_V23_SCHEMA)?;
            connection.execute_batch("PRAGMA user_version=22;")?;
            Ok(())
        })
        .unwrap();
    fixture.db.close().unwrap();

    let db = StateDatabase::open(directory, fixture.clock.clone()).unwrap();
    assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(db.schema_version().unwrap(), 23);
    db.read(|connection| {
        for table in [
            "metrics_raw",
            "metrics_5m",
            "metrics_daily",
            "metrics_activity",
            "metrics_status",
        ] {
            let exists: i64 = connection.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )?;
            assert_eq!(exists, 1, "missing {table}");
        }
        let raw_sampled_at_type: String = connection.query_row(
            "SELECT type FROM pragma_table_info('metrics_raw') WHERE name='sampled_at'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(raw_sampled_at_type.to_ascii_uppercase(), "INTEGER");
        let raw_primary_key: i64 = connection.query_row(
            "SELECT pk FROM pragma_table_info('metrics_raw') WHERE name='sampled_at'",
            [],
            |row| row.get(0),
        )?;
        assert!(raw_primary_key > 0, "sample timestamp is part of the key");
        Ok(())
    })
    .unwrap();

    let mut bytes = Vec::new();
    vesta_state::export::write_recovery_export(&db, &mut bytes, None).unwrap();
    let export: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let tables = export["tables"].as_object().unwrap();
    for table in [
        "metrics_raw",
        "metrics_5m",
        "metrics_daily",
        "metrics_activity",
        "metrics_status",
    ] {
        assert!(
            !tables.contains_key(table),
            "{table} must not enter recovery exports"
        );
    }
}
