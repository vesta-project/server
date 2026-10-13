mod support;
use support::Fixture;
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};

#[test]
fn v21_upgrade_adds_moderation_history_and_operator_capability_without_mutable_builtins() {
    let f = Fixture::open();
    let directory = f.db.directory().clone();
    f.db.write(|c| {
        c.execute_batch(support::REMOVE_V24_SCHEMA)?;
        c.execute_batch(support::REMOVE_V23_SCHEMA)?;
        c.execute_batch(support::REMOVE_V22_SCHEMA)?;
        c.execute_batch("PRAGMA user_version=21;")?;
        Ok(())
    })
    .unwrap();
    f.db.close().unwrap();
    let db = StateDatabase::open(directory, f.clock.clone()).unwrap();
    assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    db.read(|c| {
        let roles: Vec<String> = c.prepare("SELECT role_id FROM role_capabilities WHERE capability='players.operator' ORDER BY role_id")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        assert_eq!(roles, vec!["administrator", "owner"]);
        assert_eq!(c.query_row("SELECT count(*) FROM moderation_actions", [], |r| r.get::<_,i64>(0))?, 0);
        Ok(())
    }).unwrap();
    db.write(|c| {
        assert!(
            c.execute(
                "INSERT INTO role_capabilities VALUES('moderator','players.operator','*')",
                []
            )
            .is_err()
        );
        Ok(())
    })
    .unwrap();
}
