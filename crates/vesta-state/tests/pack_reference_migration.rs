mod support;

use rusqlite::{Connection, params};
use support::{DEFAULT_TEST_TIME_MS, Fixture};
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDatabase};

// Independent schema-v9 fixture: the original v7 release table is unchanged in v9.
const ORIGINAL_RELEASES: &str = r#"
CREATE TABLE pack_releases (
    server_id TEXT NOT NULL REFERENCES server_identity(server_id),
    revision INTEGER NOT NULL CHECK (revision BETWEEN 1 AND 9007199254740991),
    minimum_revision INTEGER NOT NULL CHECK (minimum_revision BETWEEN 1 AND revision),
    display_version TEXT NOT NULL CHECK (length(CAST(display_version AS BLOB)) BETWEEN 1 AND 256),
    format TEXT NOT NULL CHECK (format IN ('vestapack', 'zip', 'mrpack')),
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    size INTEGER NOT NULL CHECK (size >= 0),
    published_at INTEGER NOT NULL,
    PRIMARY KEY (server_id, revision)
) STRICT;
"#;

#[test]
fn v9_platform_reference_migration_preserves_legacy_releases_and_constraints() {
    let f = Fixture::open();
    let directory = f.db.directory().clone();
    let server = f.db.server_id().to_owned();
    f.db.close().unwrap();
    let old = Connection::open(directory.database_file()).unwrap();
    old.execute_batch(&format!(
        "DROP TABLE file_intents; DROP TABLE pack_releases; {ORIGINAL_RELEASES} PRAGMA user_version=9;"
    ))
    .unwrap();
    let insert = "INSERT INTO pack_releases VALUES (?,?,?,?,?,?,?,?)";
    for (revision, format) in [(1, "zip"), (2, "mrpack"), (3, "vestapack")] {
        old.execute(
            insert,
            params![
                server,
                revision,
                1,
                "1.0",
                format,
                "a".repeat(64),
                17,
                DEFAULT_TEST_TIME_MS
            ],
        )
        .unwrap();
    }
    assert!(
        old.execute(
            insert,
            params![
                server,
                4,
                1,
                "1.0",
                "catalogue",
                "a".repeat(64),
                17,
                DEFAULT_TEST_TIME_MS
            ]
        )
        .is_err()
    );
    drop(old);
    let migrated = StateDatabase::open(directory.clone(), f.clock.clone()).unwrap();
    assert_eq!(migrated.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(migrated.server_id(), server);
    migrated
        .write(|c| {
            let mut query =
                c.prepare("SELECT revision,format,size FROM pack_releases ORDER BY revision")?;
            let rows = query
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(
                rows,
                [
                    (1, "zip".to_owned(), 17),
                    (2, "mrpack".to_owned(), 17),
                    (3, "vestapack".to_owned(), 17)
                ]
            );
            c.execute(
                insert,
                params![
                    server,
                    4,
                    2,
                    "2.0",
                    "catalogue",
                    "b".repeat(64),
                    256,
                    DEFAULT_TEST_TIME_MS
                ],
            )?;
            for (column, value) in [
                ("server_id", "'missing'"),
                ("revision", "0"),
                ("minimum_revision", "5"),
                ("display_version", "''"),
                ("format", "'unknown'"),
                ("sha256", "'bad'"),
                ("size", "-1"),
            ] {
                assert!(
                    c.execute(
                        &format!("UPDATE pack_releases SET {column}={value} WHERE revision=4"),
                        []
                    )
                    .is_err()
                );
            }
            assert_eq!(
                c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                0
            );
            Ok(())
        })
        .unwrap();
    migrated.close().unwrap();
    let backup_path = directory.path().join(format!(
        "migration-backups/pre-migration-v9-to-v{CURRENT_SCHEMA_VERSION}-1700000000000.db"
    ));
    let backup =
        Connection::open_with_flags(backup_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert_eq!(
        backup
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        9
    );
    assert_eq!(
        backup
            .query_row("SELECT count(*) FROM pack_releases", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
    let sql: String = backup
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='pack_releases'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!sql.contains("catalogue"));
}
