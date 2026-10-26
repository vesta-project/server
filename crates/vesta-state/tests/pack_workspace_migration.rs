mod support;
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};
#[test]
fn v26_upgrades_to_a_separate_empty_workspace_without_changing_server_identity() {
    let f = support::Fixture::open();
    let directory = f.db.directory().clone();
    let server = f.db.server_id().to_owned();
    f.db.write(|c| {
        c.execute_batch(support::REMOVE_V27_SCHEMA)?;
        c.execute_batch("PRAGMA user_version=26;")?;
        Ok(())
    })
    .unwrap();
    f.db.close().unwrap();
    let db = StateDatabase::open(directory, f.clock.clone()).unwrap();
    assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(db.server_id(), server);
    db.read(|c| {
        for table in [
            "pack_workspace",
            "pack_workspace_blobs",
            "pack_workspace_receipts",
            "pack_workspace_published",
            "pack_workspace_uploads",
        ] {
            assert_eq!(
                c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
        }
        Ok(())
    })
    .unwrap();
    assert!(
        db.write(|c| {
            c.execute(
                "INSERT INTO pack_workspace(server_id,revision,manifest_json) VALUES(?,0,'{}')",
                [db.server_id()],
            )?;
            Ok(())
        })
        .is_err()
    );
    db.close().unwrap();
}
