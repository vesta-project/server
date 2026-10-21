mod support;

use support::Fixture;
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};

#[test]
fn v20_upgrade_adds_the_backup_policy_table_and_owner_only_configure() {
    let f = Fixture::open();
    let directory = f.db.directory().clone();
    f.db.write(|c| {
        c.execute_batch(support::REMOVE_V25_SCHEMA)?;
        c.execute_batch(support::REMOVE_V24_SCHEMA)?;
        c.execute_batch(support::REMOVE_V23_SCHEMA)?;
        c.execute_batch(support::REMOVE_V22_SCHEMA)?;
        c.execute_batch(support::REMOVE_V21_SCHEMA)?;
        c.execute_batch("PRAGMA user_version=20;")?;
        Ok(())
    })
    .unwrap();
    f.db.close().unwrap();
    let upgraded = StateDatabase::open(directory, f.clock.clone()).unwrap();
    assert_eq!(upgraded.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    upgraded
        .read(|c| {
            let holders: Vec<String> = c
                .prepare("SELECT role_id FROM role_capabilities WHERE capability='backups.configure' ORDER BY role_id")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            assert_eq!(holders, vec!["owner".to_owned()]);
            Ok(())
        })
        .unwrap();
    upgraded
        .write(|c| {
            // Built-in roles stay immutable after the seeding.
            assert!(
                c.execute(
                    "INSERT INTO role_capabilities VALUES('administrator','backups.configure','*')",
                    []
                )
                .is_err()
            );
            c.execute(
                "INSERT INTO backup_policy VALUES(1,'{\"profiles\":[]}',1,10,'p')",
                [],
            )?;
            assert!(
                c.execute("INSERT INTO backup_policy VALUES(2,'{}',1,10,'p')", [])
                    .is_err()
            );
            assert!(
                c.execute("UPDATE backup_policy SET policy_json='not json'", [])
                    .is_err()
            );
            assert!(
                c.execute("UPDATE backup_policy SET revision=0", [])
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
}
