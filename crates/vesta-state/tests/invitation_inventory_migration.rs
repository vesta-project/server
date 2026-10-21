mod support;
use vesta_state::StateDatabase;

#[test]
fn v24_invitations_receive_independent_ids_and_cascade_on_removal() {
    let f = support::Fixture::open();
    let directory = f.db.directory().clone();
    let server = f.db.server_id().to_owned();
    f.db.write(|c| {
        c.execute_batch("DROP TABLE management_invitation_ids; PRAGMA user_version=24;")?;
        c.execute(
            "INSERT INTO principals VALUES('inviter',?,'Alice','active',1,1)",
            [&server],
        )?;
        c.execute(
            "INSERT INTO devices VALUES('inviter-device',?,'inviter','Desktop',?,1,NULL,NULL)",
            rusqlite::params![server, "0".repeat(64)],
        )?;
        c.execute(
            "INSERT INTO management_pairing_codes VALUES(?,?,'viewer',1,9999999999999)",
            rusqlite::params![vec![7u8; 32], server],
        )?;
        c.execute(
            "INSERT INTO management_invitation_issuers VALUES(?,'inviter','inviter-device')",
            [vec![7u8; 32]],
        )?;
        Ok(())
    })
    .unwrap();
    f.db.close().unwrap();
    let db = StateDatabase::open(directory, f.clock.clone()).unwrap();
    db.write(|c| {
        let id: String =
            c.query_row("SELECT id FROM management_invitation_ids", [], |r| r.get(0))?;
        assert_eq!(id.len(), 32);
        assert_ne!(id, "07".repeat(16));
        c.execute("DELETE FROM management_pairing_codes", [])?;
        assert_eq!(
            c.query_row("SELECT count(*) FROM management_invitation_ids", [], |r| {
                r.get::<_, i64>(0)
            })?,
            0
        );
        Ok(())
    })
    .unwrap();
    db.close().unwrap();
}
