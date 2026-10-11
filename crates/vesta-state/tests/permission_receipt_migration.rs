mod support;
use support::Fixture;
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};

#[test]
fn v16_upgrade_dates_existing_permission_receipts_and_keeps_them() {
    let f = Fixture::open();
    let directory = f.db.directory().clone();
    f.db.write(|c| {
        c.execute(
            "INSERT INTO management_permission_receipts(key_hash,intent_hash,result_json) VALUES(?,?,'{\"ok\":true}')",
            rusqlite::params![[1u8; 32].as_slice(), [2u8; 32].as_slice()],
        )?;
        c.execute_batch("DROP INDEX management_permission_receipts_retention; ALTER TABLE management_permission_receipts DROP COLUMN completed_at; PRAGMA user_version=16;")?;
        Ok(())
    })
    .unwrap();
    f.db.close().unwrap();
    let upgraded = StateDatabase::open(directory, f.clock.clone()).unwrap();
    assert_eq!(upgraded.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    upgraded
        .read(|c| {
            let (json, completed): (String, i64) = c.query_row(
                "SELECT result_json, completed_at FROM management_permission_receipts",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(json, "{\"ok\":true}");
            assert!(completed > 0, "existing receipts are dated at upgrade");
            Ok(())
        })
        .unwrap();
}
