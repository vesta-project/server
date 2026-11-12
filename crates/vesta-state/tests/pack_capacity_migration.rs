mod support;
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};
#[test]
fn v28_uploads_and_drafts_survive_the_capacity_migration() {
    let f = support::Fixture::open();
    let directory = f.db.directory().clone();
    let id = f.db.server_id().to_owned();
    f.db.write(|c| {
        c.execute_batch(support::REMOVE_V31_SCHEMA)?;
        c.execute_batch("DROP TABLE pack_import_warnings; DROP TABLE pack_workspace_uploads; CREATE TABLE pack_workspace_uploads(server_id TEXT NOT NULL REFERENCES server_identity(server_id),principal_id TEXT NOT NULL,device_id TEXT NOT NULL,upload_id TEXT NOT NULL,total_size INTEGER NOT NULL,sha256 TEXT NOT NULL,archive INTEGER NOT NULL,complete INTEGER NOT NULL DEFAULT 0,body BLOB NOT NULL,PRIMARY KEY(server_id,principal_id,device_id,upload_id),CHECK(total_size>=0 AND total_size<=71303168),CHECK(length(body)<=total_size)) STRICT; PRAGMA user_version=28;")?;
        c.execute("INSERT INTO pack_workspace_uploads VALUES(?,'owner','device','resume',6,?,1,0,?)",rusqlite::params![id,"a".repeat(64),b"abc".as_slice()])?;
        c.execute("INSERT INTO pack_workspace(server_id,revision,manifest_json) VALUES(?,1,'{}')",[&id])?;Ok(())
    }).unwrap();
    f.db.close().unwrap();
    let db = StateDatabase::open(directory, f.clock.clone()).unwrap();
    assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(db.server_id(), id);
    db.write(|c| {
        let old:(i64,Vec<u8>)=c.query_row("SELECT total_size,body FROM pack_workspace_uploads WHERE upload_id='resume'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;assert_eq!(old,(6,b"abc".to_vec()));
        c.execute("INSERT INTO pack_workspace_uploads VALUES(?,'owner','device','large',1073741824,?,1,0,X'')",rusqlite::params![id,"b".repeat(64)])?;
        assert_eq!(c.query_row("SELECT manifest_json FROM pack_workspace",[],|r|r.get::<_,String>(0))?,"{}");Ok(())
    }).unwrap();
    db.close().unwrap();
}
#[test]
fn v29_workspace_rows_survive_review_threshold_migration() {
    let f = support::Fixture::open();
    let directory = f.db.directory().clone();
    let id = f.db.server_id().to_owned();
    f.db.write(|c| {
        c.execute_batch(support::REMOVE_V31_SCHEMA)?;
        c.execute(
            "INSERT INTO pack_workspace(server_id,revision,manifest_json) VALUES(?,7,'{}')",
            [&id],
        )?;
        c.execute(
            "INSERT INTO pack_workspace_blobs VALUES(?,?,X'0102')",
            rusqlite::params![id, "a".repeat(64)],
        )?;
        c.execute_batch("DROP TABLE pack_import_warnings; PRAGMA user_version=29;")?;
        Ok(())
    })
    .unwrap();
    f.db.close().unwrap();
    let db = StateDatabase::open(directory, f.clock.clone()).unwrap();
    db.write(|c| {
        assert_eq!(c.query_row("SELECT revision FROM pack_workspace", [], |r| r.get::<_, i64>(0))?, 7);
        assert_eq!(c.query_row("SELECT body FROM pack_workspace_blobs", [], |r| r.get::<_, Vec<u8>>(0))?, [1, 2]);
        c.execute("INSERT INTO pack_workspace_blobs VALUES(?,?,zeroblob(16777217))", rusqlite::params![id, "b".repeat(64)])?;
        c.execute("INSERT INTO pack_workspace_uploads VALUES(?,'owner','device','huge',137438953473,?,1,0,X'')", rusqlite::params![id, "c".repeat(64)])?;
        assert!(c.execute("INSERT INTO pack_workspace_uploads VALUES(?,'owner','device','negative',-1,?,1,0,X'')", rusqlite::params![id, "d".repeat(64)]).is_err());
        Ok(())
    }).unwrap();
    db.close().unwrap();
}
