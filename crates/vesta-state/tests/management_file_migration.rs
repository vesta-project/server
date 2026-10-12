mod support;
use rusqlite::params;
use support::Fixture;
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};

#[test]
fn genuine_v13_upgrade_adds_empty_file_ledger_without_changing_pack_outcomes() {
    let f = Fixture::open();
    let directory = f.db.directory().clone();
    let server = f.db.server_id().to_owned();
    f.db.write(|c| {
        c.execute(
            "INSERT INTO management_pack_outcomes VALUES (?,'principal','pack-key','{}',7,3)",
            [&server],
        )?;
        c.execute_batch(support::REMOVE_V18_SCHEMA)?;
        c.execute_batch(support::REMOVE_V16_SCHEMA)?;
        c.execute_batch("DROP TABLE management_file_outcomes; PRAGMA user_version=13;")?;
        Ok(())
    })
    .unwrap();
    f.db.close().unwrap();
    let upgraded = StateDatabase::open(directory, f.clock.clone()).unwrap();
    assert_eq!(upgraded.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    upgraded
        .read(|c| {
            let files: i64 =
                c.query_row("SELECT count(*) FROM management_file_outcomes", [], |r| {
                    r.get(0)
                })?;
            let packs: i64 = c.query_row(
                "SELECT count(*) FROM management_pack_outcomes WHERE server_id=?",
                [&server],
                |r| r.get(0),
            )?;
            assert_eq!(files, 0);
            assert_eq!(packs, 1);
            Ok(())
        })
        .unwrap();
    upgraded.close().unwrap();
}

#[test]
fn file_outcome_scope_bounds_and_completion_are_transactional() {
    let f = Fixture::open();
    let insert = |device: &str, key: &str, hash: &str, authority: &str, outcome: Option<&str>| {
        f.db.write(|c| {
            c.execute(
                "INSERT INTO management_file_outcomes (server_id,principal_id,device_id,request_key,input_sha256,authority_json,outcome_json) VALUES (?,'principal',?,?,?,?,?)",
                params![f.db.server_id(), device, key, hash, authority, outcome],
            )?;
            Ok(())
        })
    };
    insert("device1", "key", &"a".repeat(64), "[]", None).unwrap();
    insert("device2", "key", &"a".repeat(64), "[]", None).unwrap();
    assert!(insert("device1", "key", &"a".repeat(64), "[]", None).is_err());
    for (key, hash, authority, outcome) in [
        ("".to_owned(), "a".repeat(64), "[]".to_owned(), None),
        ("k".repeat(129), "a".repeat(64), "[]".to_owned(), None),
        (
            "bad-hash".to_owned(),
            "short".to_owned(),
            "[]".to_owned(),
            None,
        ),
        (
            "bad-auth".to_owned(),
            "a".repeat(64),
            "x".repeat(16385),
            None,
        ),
        (
            "bad-outcome".to_owned(),
            "a".repeat(64),
            "[]".to_owned(),
            Some("x".repeat(4097)),
        ),
    ] {
        assert!(insert("device1", &key, &hash, &authority, outcome.as_deref()).is_err());
    }
    assert!(f.db.write(|c| {
        c.execute("UPDATE management_file_outcomes SET outcome_json='{}' WHERE device_id='device1'",[])?;
        c.execute("INSERT INTO management_file_outcomes (server_id,principal_id,device_id,request_key,input_sha256,authority_json,outcome_json) VALUES (?,'principal','device1','bad','short','[]',NULL)",[f.db.server_id()])?;
        Ok(())
    }).is_err());
    let outcome: Option<String> =
        f.db.read(|c| {
            Ok(c.query_row(
                "SELECT outcome_json FROM management_file_outcomes WHERE device_id='device1'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert!(outcome.is_none());
}

#[test]
fn genuine_v14_retention_upgrade_dates_terminal_rows_and_preserves_pending_rows() {
    let f = Fixture::open();
    let directory = f.db.directory().clone();
    let server = f.db.server_id().to_owned();
    f.db.write(|c| {
        c.execute_batch(support::REMOVE_V18_SCHEMA)?; c.execute_batch(support::REMOVE_V16_SCHEMA)?;
        c.execute_batch("DROP INDEX management_file_outcomes_retention; ALTER TABLE management_file_outcomes DROP COLUMN completed_at; PRAGMA user_version=14;")?;
        for (key,outcome) in [("terminal",Some("{}")),("pending",None)] {
            c.execute("INSERT INTO management_file_outcomes VALUES (?,'actor','device',?,?,'[]',?)",params![server,key,"a".repeat(64),outcome])?;
        } Ok(())
    }).unwrap();
    f.db.close().unwrap();
    let now = f.clock.advance(1000);
    let upgraded = StateDatabase::open(directory, f.clock.clone()).unwrap();
    let rows=upgraded.read(|c| {
        let mut stmt=c.prepare("SELECT request_key,completed_at FROM management_file_outcomes ORDER BY request_key")?;
        Ok(stmt.query_map([],|r| Ok((r.get::<_,String>(0)?,r.get::<_,Option<i64>>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).unwrap();
    assert_eq!(
        rows,
        vec![("pending".into(), None), ("terminal".into(), Some(now))]
    );
    upgraded.close().unwrap();
}
