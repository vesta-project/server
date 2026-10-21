mod support;

use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};

#[test]
fn v22_upgrade_creates_metrics_state_and_keeps_it_out_of_recovery_exports() {
    let fixture = support::Fixture::open();
    let directory = fixture.db.directory().clone();
    fixture
        .db
        .write(|connection| {
            connection.execute_batch(support::REMOVE_V25_SCHEMA)?;
            connection.execute_batch(support::REMOVE_V24_SCHEMA)?;
            connection.execute_batch(support::REMOVE_V23_SCHEMA)?;
            connection.execute_batch("PRAGMA user_version=22;")?;
            Ok(())
        })
        .unwrap();
    fixture.db.close().unwrap();

    let db = StateDatabase::open(directory, fixture.clock.clone()).unwrap();
    assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    db.read(|connection| {
        for table in [
            "metrics_raw",
            "metrics_5m",
            "metrics_daily",
            "metrics_activity",
            "metrics_status",
            "metrics_policy",
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
        let tick_source_type: String = connection.query_row(
            "SELECT type FROM pragma_table_info('metrics_raw') WHERE name='tick_source'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(tick_source_type.to_ascii_uppercase(), "TEXT");
        let status_revision: i64 = connection.query_row(
            "SELECT revision FROM metrics_status WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(status_revision, 0);
        let policy_rows: i64 =
            connection.query_row("SELECT count(*) FROM metrics_policy", [], |row| row.get(0))?;
        assert_eq!(policy_rows, 0, "host configuration remains the fallback");
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
        "metrics_policy",
    ] {
        assert!(
            !tables.contains_key(table),
            "{table} must not enter recovery exports"
        );
    }
}

#[test]
fn v23_upgrade_adds_nullable_tick_source_revision_and_persistent_policy() {
    let fixture = support::Fixture::open();
    let directory = fixture.db.directory().clone();
    fixture
        .db
        .write(|connection| {
            connection.execute_batch(support::REMOVE_V25_SCHEMA)?;
            connection.execute_batch(support::REMOVE_V24_SCHEMA)?;
            connection.execute("INSERT INTO metrics_raw(sampled_at,phase,readiness,bytes) VALUES(100,'stopped','unknown',0)", [])?;
            connection.execute("INSERT INTO metrics_5m(bucket_start,data,bytes) VALUES(300,'{}',0)", [])?;
            connection.execute("INSERT INTO metrics_daily(bucket_start,data,bytes) VALUES(0,'{}',0)", [])?;
            connection.execute_batch("PRAGMA user_version=23;")?;
            Ok(())
        })
        .unwrap();
    fixture.db.close().unwrap();

    let db = StateDatabase::open(directory.clone(), fixture.clock.clone()).unwrap();
    assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    db.read(|connection| {
        for (table, key_column, key) in [
            ("metrics_raw", "sampled_at", 100_i64),
            ("metrics_5m", "bucket_start", 300_i64),
            ("metrics_daily", "bucket_start", 0_i64),
        ] {
            let revision: i64 = connection.query_row(
                &format!("SELECT revision FROM {table} WHERE {key_column}=?1"),
                [key],
                |row| row.get(0),
            )?;
            assert_eq!(revision, 0, "v23 {table} row gets the v24 default");
        }
        let prune_revision: i64 = connection.query_row(
            "SELECT prune_revision FROM metrics_status WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(prune_revision, 0);
        Ok(())
    })
    .unwrap();
    db.write(|connection| {
        connection.execute(
            "INSERT INTO metrics_policy(singleton,revision,document) VALUES(1,1,'{\"enabled\":true}')",
            [],
        )?;
        connection.execute(
            "INSERT INTO metrics_raw(sampled_at,phase,readiness,bytes,tick_source,revision) VALUES(101,'stopped','unknown',0,'server_brand',8)",
            [],
        )?;
        connection.execute("UPDATE metrics_raw SET revision=7 WHERE sampled_at=100", [])?;
        connection.execute("UPDATE metrics_5m SET revision=7 WHERE bucket_start=300", [])?;
        connection.execute("UPDATE metrics_daily SET revision=7 WHERE bucket_start=0", [])?;
        connection.execute("UPDATE metrics_status SET revision=8 WHERE singleton=1", [])?;
        Ok(())
    })
    .unwrap();
    db.close().unwrap();

    let reopened = StateDatabase::open(directory, fixture.clock.clone()).unwrap();
    reopened
        .read(|connection| {
            let (revision, document): (i64, String) = connection.query_row(
                "SELECT revision,document FROM metrics_policy WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(revision, 1);
            assert_eq!(document, "{\"enabled\":true}");
            let tick_source: Option<String> = connection.query_row(
                "SELECT tick_source FROM metrics_raw WHERE sampled_at=100",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(tick_source, None);
            let tick_source: Option<String> = connection.query_row(
                "SELECT tick_source FROM metrics_raw WHERE sampled_at=101",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(tick_source.as_deref(), Some("server_brand"));
            let status_revision: i64 = connection.query_row(
                "SELECT revision FROM metrics_status WHERE singleton=1",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(status_revision, 8);
            let raw_revision: i64 = connection.query_row("SELECT revision FROM metrics_raw WHERE sampled_at=100", [], |row| row.get(0))?;
            let aggregate_revisions: (i64, i64) = connection.query_row(
                "SELECT (SELECT revision FROM metrics_5m WHERE bucket_start=300),(SELECT revision FROM metrics_daily WHERE bucket_start=0)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(raw_revision, 7);
            assert_eq!(aggregate_revisions, (7, 7));
            Ok(())
        })
        .unwrap();
}
