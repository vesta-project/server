//! Optional bounded browsing cache in protected host state, not install authority.
use super::Result;
use rusqlite::{OptionalExtension, params};
use std::sync::Arc;
use vesta_server_install::{
    CATALOGUE_CACHE_DISK_BYTES, CATALOGUE_CACHE_DOCUMENT_BYTES, CATALOGUE_CACHE_ENTRIES,
    CatalogueRecord, CatalogueStore,
};
use vesta_state::StateDatabase;

struct Store {
    db: StateDatabase,
}
pub(crate) fn open(db: StateDatabase) -> Result<Arc<dyn CatalogueStore>> {
    db.write(|connection| {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS software_catalogue_cache (
            key TEXT PRIMARY KEY NOT NULL CHECK(length(key)=64),
            body BLOB NOT NULL CHECK(length(body)<=16777216),
            sha256 TEXT NOT NULL CHECK(length(sha256)=64),
            stored_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL
        ) STRICT;",
        )?;
        Ok(())
    })?;
    Ok(Arc::new(Store { db }))
}
fn key_valid(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit())
}
impl CatalogueStore for Store {
    fn load(&self, key: &str) -> Option<CatalogueRecord> {
        if !key_valid(key) {
            return None;
        }
        self.db
            .read(|connection| {
                connection
                    .query_row(
                        "SELECT body,sha256,stored_at,expires_at FROM software_catalogue_cache
                 WHERE key=? AND length(body)<=? AND length(sha256)=64
                   AND stored_at>=0 AND expires_at>stored_at
                   AND expires_at-stored_at<=300000",
                        params![key, CATALOGUE_CACHE_DOCUMENT_BYTES as i64],
                        |row| {
                            let stored: i64 = row.get(2)?;
                            let expires: i64 = row.get(3)?;
                            Ok(CatalogueRecord {
                                bytes: row.get(0)?,
                                sha256: row.get(1)?,
                                stored_at_ms: stored.try_into().unwrap_or(u64::MAX),
                                expires_at_ms: expires.try_into().unwrap_or(0),
                            })
                        },
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .ok()
            .flatten()
    }
    fn save(&self, records: &[(String, CatalogueRecord)]) {
        if records.is_empty() {
            return;
        }
        let _ = self.db.write(|connection| {
            for (key, record) in records {
                if !key_valid(key)
                    || !key_valid(&record.sha256)
                    || record.bytes.len() > CATALOGUE_CACHE_DOCUMENT_BYTES
                    || record.expires_at_ms <= record.stored_at_ms
                    || record.expires_at_ms - record.stored_at_ms > 300_000
                {
                    continue;
                }
                let (Ok(stored), Ok(expires)) = (
                    i64::try_from(record.stored_at_ms),
                    i64::try_from(record.expires_at_ms),
                ) else {
                    continue;
                };
                connection.execute(
                    "DELETE FROM software_catalogue_cache WHERE key=? OR expires_at<=?",
                    params![key, stored],
                )?;
                loop {
                    let (count, bytes): (i64, i64) = connection.query_row(
                        "SELECT count(*),coalesce(sum(length(body)+length(key)+length(sha256)+128),0)
                         FROM software_catalogue_cache",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    let cost = record.bytes.len() + key.len() + record.sha256.len() + 128;
                    if count < CATALOGUE_CACHE_ENTRIES as i64
                        && bytes + cost as i64 <= CATALOGUE_CACHE_DISK_BYTES as i64
                    {
                        break;
                    }
                    connection.execute(
                        "DELETE FROM software_catalogue_cache WHERE key=(
                            SELECT key FROM software_catalogue_cache ORDER BY stored_at,key LIMIT 1
                         )",
                        [],
                    )?;
                }
                connection.execute(
                    "INSERT INTO software_catalogue_cache(key,body,sha256,stored_at,expires_at)
                     VALUES(?,?,?,?,?)",
                    params![key, record.bytes, record.sha256, stored, expires],
                )?;
            }
            Ok(())
        });
    }
}
