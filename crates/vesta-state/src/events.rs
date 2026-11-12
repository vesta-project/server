//! Durable event cursors, contiguous-prefix retention, and bounded replay scans.
//! Payloads are non-secret internal data; this module does not authorize a reader.
use crate::{Result, StateDatabase, StateError, values};
use rusqlite::{Connection, Row, params, params_from_iter, types::Value};

pub const MAX_PAGE: usize = 500;
pub const MAX_PAYLOAD_CHARS: usize = 4_096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredEvent {
    pub cursor: i64,
    pub server_id: String,
    pub event_type: String,
    pub operation_id: Option<String>,
    pub at_ms: i64,
    pub payload_json: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventPage {
    pub events: Vec<StoredEvent>,
    pub next_cursor: i64,
    pub gap: bool,
    pub oldest_available_cursor: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bounds {
    pub pruned_through: i64,
    pub oldest_retained: i64,
    pub latest: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanRow {
    pub cursor: i64,
    pub event_type: String,
    pub operation_id: Option<String>,
    pub at_ms: i64,
    pub payload_json: Option<String>,
    /// SQLite scalar count, or -1 for a corrupt NUL-bearing payload that is never projected.
    pub payload_chars: i64,
}

#[derive(Clone)]
pub struct EventLog {
    db: StateDatabase,
}

impl EventLog {
    pub fn new(db: StateDatabase) -> Self {
        Self { db }
    }

    pub fn append(
        &self,
        c: &Connection,
        event_type: &str,
        operation_id: Option<&str>,
        payload_json: Option<&str>,
    ) -> Result<i64> {
        values::require_text(event_type, 64)?;
        let payload = payload_json.unwrap_or("{}");
        if payload.contains('\0') || values::utf16_len(payload) > MAX_PAYLOAD_CHARS {
            return Err(StateError::new("state_invalid_request"));
        }
        c.execute(
            "INSERT INTO events (server_id,type,operation_id,at,payload_json) VALUES (?,?,?,?,?)",
            params![
                self.db.server_id(),
                event_type,
                operation_id,
                self.db.clock().millis(),
                payload
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Trusted local inspection of normally bounded rows. Use `scan` for untrusted replay.
    pub fn read(&self, after_cursor: i64, limit: i32) -> Result<EventPage> {
        self.db.read(|c| {
            let bounds = self.bounds(c)?;
            let gap = after_cursor < bounds.pruned_through;
            let mut next = if gap { after_cursor.max(bounds.pruned_through) } else { after_cursor };
            let mut statement = c.prepare(
                "SELECT cursor,server_id,type,operation_id,at,payload_json FROM events WHERE cursor > ? ORDER BY cursor LIMIT ?",
            )?;
            let rows = statement.query_map(params![after_cursor, limit.clamp(1, MAX_PAGE as i32)], |row| {
                Ok(StoredEvent {
                    cursor: row.get(0)?, server_id: row.get(1)?, event_type: row.get(2)?,
                    operation_id: row.get(3)?, at_ms: row.get(4)?, payload_json: row.get(5)?,
                })
            })?;
            let events = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            if let Some(last) = events.last() { next = last.cursor; }
            Ok(EventPage { events, next_cursor: next, gap, oldest_available_cursor: bounds.oldest_retained })
        })
    }

    /// Clock rollback never causes an eviction hole: stop at the first unexpired cursor.
    pub fn prune_older_than(&self, cutoff_ms: i64) -> Result<usize> {
        self.db.write(|c| {
            let max_pruned: i64 = c.query_row(
                "SELECT COALESCE((SELECT MIN(cursor)-1 FROM events WHERE at >= ?),(SELECT COALESCE(MAX(cursor),0) FROM events))",
                [cutoff_ms], |row| row.get(0),
            )?;
            if max_pruned <= 0 { return Ok(0); }
            let deleted = c.execute("DELETE FROM events WHERE cursor <= ?", [max_pruned])?;
            c.execute(
                "UPDATE meta SET value = ? WHERE key='events_pruned_through' AND CAST(value AS INTEGER) < ?",
                params![max_pruned.to_string(), max_pruned],
            )?;
            Ok(deleted)
        })
    }

    pub fn bounds(&self, c: &Connection) -> Result<Bounds> {
        let pruned_through = pruned_through(c)?;
        let (oldest, latest): (Option<i64>, Option<i64>) =
            c.query_row("SELECT MIN(cursor),MAX(cursor) FROM events", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        Ok(Bounds {
            pruned_through,
            oldest_retained: oldest.unwrap_or(0),
            latest: latest.unwrap_or(0).max(pruned_through),
        })
    }

    /// Projects payloads only for selected types and within the caller's smaller cap.
    /// SQL clips headers before loading them. This never materializes unrelated or oversized payloads.
    pub fn scan(
        &self,
        c: &Connection,
        after: i64,
        through: i64,
        limit: usize,
        payload_types: &[&str],
        max_payload_chars: usize,
    ) -> Result<Vec<ScanRow>> {
        if !(1..=MAX_PAGE).contains(&limit)
            || payload_types.is_empty()
            || payload_types.len() > 8
            || max_payload_chars > MAX_PAYLOAD_CHARS
        {
            return Err(StateError::new("state_invalid_request"));
        }
        let marks = vec!["?"; payload_types.len()].join(",");
        let sql = format!(
            "SELECT cursor,substr(type,1,65),substr(operation_id,1,65),at,\
             CASE WHEN type IN ({marks}) AND length(payload_json) <= ? \
             AND length(CAST(payload_json AS BLOB)) <= ? \
             AND instr(payload_json,char(0)) = 0 THEN payload_json END,\
             CASE WHEN type IN ({marks}) THEN \
               CASE WHEN instr(payload_json,char(0)) > 0 THEN -1 ELSE length(payload_json) END \
             ELSE 0 END \
             FROM events WHERE cursor > ? AND cursor <= ? ORDER BY cursor LIMIT ?",
        );
        let mut parameters: Vec<Value> = payload_types
            .iter()
            .map(|s| Value::Text((*s).into()))
            .collect();
        parameters.push(Value::Integer(max_payload_chars as i64));
        // SQLite text length stops at NUL. A separate UTF-8 byte cap keeps corrupt rows
        // bounded even when their SQL character count under-reports the stored payload.
        parameters.push(Value::Integer((max_payload_chars * 4) as i64));
        parameters.extend(payload_types.iter().map(|s| Value::Text((*s).into())));
        parameters.extend([
            Value::Integer(after),
            Value::Integer(through),
            Value::Integer(limit as i64),
        ]);
        let mut statement = c.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(parameters), scan_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn scan_row(row: &Row<'_>) -> rusqlite::Result<ScanRow> {
    Ok(ScanRow {
        cursor: row.get(0)?,
        event_type: row.get(1)?,
        operation_id: row.get(2)?,
        at_ms: row.get(3)?,
        payload_json: row.get(4)?,
        payload_chars: row.get(5)?,
    })
}

fn pruned_through(c: &Connection) -> Result<i64> {
    use rusqlite::OptionalExtension;
    let value: Option<String> = c
        .query_row(
            "SELECT value FROM meta WHERE key='events_pruned_through'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    value
        .as_deref()
        .unwrap_or("0")
        .parse::<i64>()
        .map_err(|_| StateError::new("state_integrity_failed"))
}
