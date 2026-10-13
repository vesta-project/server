mod support;

use support::Fixture;
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};

/// id, state, idempotency key, result JSON, applying flag.
type Row = (String, String, Option<String>, Option<String>, i64);

/// The v19 table, as shipped, with one finished and one queued job.
const V19_BACKUP_JOBS: &str = r#"
    DROP TABLE backup_jobs;
    CREATE TABLE backup_jobs (
        id TEXT PRIMARY KEY CHECK(length(CAST(id AS BLOB)) BETWEEN 1 AND 128),
        profile TEXT NOT NULL CHECK(length(CAST(profile AS BLOB)) BETWEEN 1 AND 32),
        kind TEXT NOT NULL CHECK(kind IN ('backup','verify','restore')),
        state TEXT NOT NULL CHECK(state IN ('queued','running','succeeded','failed','uncertain')),
        requested_by TEXT NOT NULL CHECK(length(CAST(requested_by AS BLOB)) BETWEEN 1 AND 128),
        requested_device TEXT NOT NULL CHECK(length(CAST(requested_device AS BLOB)) BETWEEN 1 AND 128),
        idempotency_key TEXT UNIQUE CHECK(idempotency_key IS NULL OR length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 200),
        snapshot_id TEXT CHECK(snapshot_id IS NULL OR (length(snapshot_id) BETWEEN 8 AND 64 AND snapshot_id NOT GLOB '*[^0-9a-f]*')),
        target TEXT CHECK(target IS NULL OR length(target)<=16384),
        result_json TEXT CHECK(result_json IS NULL OR length(result_json)<=16384),
        error_code TEXT CHECK(error_code IS NULL OR length(error_code) BETWEEN 1 AND 128),
        created_at INTEGER NOT NULL CHECK(created_at>=0),
        started_at INTEGER CHECK(started_at IS NULL OR started_at>=0),
        finished_at INTEGER CHECK(finished_at IS NULL OR finished_at>=0)
    ) STRICT;
    CREATE INDEX backup_jobs_queue ON backup_jobs(state, created_at, id);
    CREATE INDEX backup_jobs_history ON backup_jobs(created_at, id);
    INSERT INTO backup_jobs (id,profile,kind,state,requested_by,requested_device,idempotency_key,snapshot_id,result_json,created_at,started_at,finished_at)
        VALUES ('done','nightly','backup','succeeded','p','d','key-1','0123456789abcdef0123456789abcdef','{"files_new":1}',10,11,12);
    INSERT INTO backup_jobs (id,profile,kind,state,requested_by,requested_device,target,created_at)
        VALUES ('waiting','nightly','restore','queued','p','d','{"snapshot":"x","paths":[]}',20);
    PRAGMA user_version=19;
"#;

#[test]
fn v19_backup_jobs_upgrade_keeps_rows_and_admits_confirmed_in_place_restores() {
    let f = Fixture::open();
    let directory = f.db.directory().clone();
    f.db.write(|c| {
        c.execute_batch(support::REMOVE_V22_SCHEMA)?;
        c.execute_batch(support::REMOVE_V21_SCHEMA)?;
        c.execute_batch(V19_BACKUP_JOBS)?;
        Ok(())
    })
    .unwrap();
    f.db.close().unwrap();
    let upgraded = StateDatabase::open(directory, f.clock.clone()).unwrap();
    assert_eq!(upgraded.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    upgraded
        .read(|c| {
            let rows: Vec<Row> = c
                .prepare("SELECT id,state,idempotency_key,result_json,applying FROM backup_jobs ORDER BY id")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<_>>()?;
            assert_eq!(
                rows,
                vec![
                    ("done".into(), "succeeded".into(), Some("key-1".into()), Some(r#"{"files_new":1}"#.into()), 0),
                    ("waiting".into(), "queued".into(), None, None, 0),
                ]
            );
            let indexes: i64 = c.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='index' AND name IN ('backup_jobs_queue','backup_jobs_history')",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(indexes, 2);
            Ok(())
        })
        .unwrap();
    upgraded
        .write(|c| {
            let hash = "a".repeat(64);
            // An awaiting confirmation needs its hash and deadline, and nothing else may carry them.
            c.execute(
                "INSERT INTO backup_jobs (id,profile,kind,state,requested_by,requested_device,created_at,confirmation_hash,confirm_by) VALUES ('new','nightly','restore_in_place','awaiting_confirmation','p','d',30,?,40)",
                [&hash],
            )?;
            assert!(c.execute("INSERT INTO backup_jobs (id,profile,kind,state,requested_by,requested_device,created_at) VALUES ('bad1','nightly','restore_in_place','awaiting_confirmation','p','d',30)", []).is_err());
            assert!(c.execute("INSERT INTO backup_jobs (id,profile,kind,state,requested_by,requested_device,created_at,confirmation_hash,confirm_by) VALUES ('bad2','nightly','restore_in_place','queued','p','d',30,?,40)", [&hash]).is_err());
            assert!(c.execute("UPDATE backup_jobs SET state='queued' WHERE id='new'", []).is_err());
            c.execute("UPDATE backup_jobs SET state='queued',confirmation_hash=NULL,confirm_by=NULL WHERE id='new'", [])?;
            c.execute("UPDATE backup_jobs SET state='recovery_required',applying=1 WHERE id='new'", [])?;
            assert!(c.execute("UPDATE backup_jobs SET kind='format' WHERE id='new'", []).is_err());
            assert!(c.execute("UPDATE backup_jobs SET applying=2 WHERE id='new'", []).is_err());
            assert!(c.execute("UPDATE backup_jobs SET safety_snapshot_id='short' WHERE id='new'", []).is_err());
            Ok(())
        })
        .unwrap();
}
