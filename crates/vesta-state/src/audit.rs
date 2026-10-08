//! Append-only audit with atomic caller-owned transactions and Java/Gson-compatible hashes.
//!
//! Startup verifies every retained row. Tail truncation or whole-chain replacement
//! cannot be detected without an external checkpoint; this is not tamper-proof storage.
use crate::{Result, StateDatabase, StateError, values};
use rusqlite::{Connection, Row, params};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt::Write;

pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
pub const MAX_PAGE: usize = 500;
/// Maximum number of metadata entries on one record.
pub const MAX_METADATA_ENTRIES: usize = 16;
/// Maximum length of one metadata value, counted in UTF-16 units.
///
/// Anything that must be recorded as audit metadata has to be validated against this bound
/// *before* the change that records it is committed; otherwise the audit append fails after
/// the fact and rolls back work the caller was entitled to perform.
pub const MAX_METADATA_VALUE_UNITS: usize = 256;
const FORBIDDEN_METADATA: &[&str] = &[
    "password",
    "secret",
    "token",
    "key",
    "credential",
    "cookie",
    "authorization",
    "command",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditActor {
    principal_id: String,
    device_id: String,
    source: String,
}

impl AuditActor {
    pub fn new(principal_id: String, device_id: String, source: String) -> Result<Self> {
        values::require_text(&principal_id, 128)?;
        values::require_text(&device_id, 128)?;
        values::require_text(&source, 64)?;
        Ok(Self {
            principal_id,
            device_id,
            source,
        })
    }

    pub fn system(source: String) -> Result<Self> {
        Self::new("system".into(), "local".into(), source)
    }

    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }
    pub fn device_id(&self) -> &str {
        &self.device_id
    }
    pub fn source(&self) -> &str {
        &self.source
    }
}

#[derive(Clone)]
pub struct AuditRecord {
    actor: AuditActor,
    action: String,
    target: String,
    outcome: String,
    operation_id: Option<String>,
    metadata: Vec<(String, String)>,
}

impl AuditRecord {
    /// Values may be null (normalized to empty text), as in the Java record.
    pub fn new(
        actor: AuditActor,
        action: String,
        target: String,
        outcome: String,
        operation_id: Option<String>,
        metadata: impl IntoIterator<Item = (String, Option<String>)>,
    ) -> Result<Self> {
        values::require_text(&action, 96)?;
        values::require_text(&target, 512)?;
        values::require_text(&outcome, 32)?;
        values::optional_text(operation_id.as_deref(), 64)?;
        let mut seen = HashSet::new();
        let mut safe = Vec::new();
        for (key, value) in metadata {
            if safe.len() >= MAX_METADATA_ENTRIES || !seen.insert(key.clone()) {
                return Err(StateError::new("state_invalid_request"));
            }
            values::require_text(&key, 64)?;
            let lower = key.to_lowercase();
            if FORBIDDEN_METADATA.iter().any(|part| lower.contains(part)) {
                return Err(StateError::new("state_invalid_request"));
            }
            let value = value.unwrap_or_default();
            if values::utf16_len(&value) > MAX_METADATA_VALUE_UNITS {
                return Err(StateError::new("state_invalid_request"));
            }
            safe.push((key, value));
        }
        // Java TreeMap uses UTF-16 ordering, which differs from scalar ordering
        // when supplementary and high-BMP characters appear in metadata keys.
        safe.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
        Ok(Self {
            actor,
            action,
            target,
            outcome,
            operation_id,
            metadata: safe,
        })
    }

    pub fn of(
        actor: AuditActor,
        action: String,
        target: String,
        outcome: String,
        operation_id: Option<String>,
    ) -> Result<Self> {
        Self::new(
            actor,
            action,
            target,
            outcome,
            operation_id,
            std::iter::empty(),
        )
    }

    pub fn metadata_json(&self) -> String {
        let mut out = String::from("{");
        for (index, (key, value)) in self.metadata.iter().enumerate() {
            if index != 0 {
                out.push(',');
            }
            gson_string(&mut out, key);
            out.push(':');
            gson_string(&mut out, value);
        }
        out.push('}');
        out
    }
}

/// Raw hashing fields, for persisted-chain verification and independent fixtures.
/// Constructing these fields grants no authority and does not append an audit row.
pub struct ChainFields<'a> {
    pub prev_hash: &'a str,
    pub server_id: &'a str,
    pub principal_id: &'a str,
    pub device_id: &'a str,
    pub source: &'a str,
    pub action: &'a str,
    pub target: &'a str,
    pub outcome: &'a str,
    pub operation_id: Option<&'a str>,
    pub at_ms: i64,
    pub metadata_json: &'a str,
}

/// Exact Gson default JsonWriter escaping; serde's default escaping is different.
fn gson_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' | '>' | '&' | '=' | '\'' | '\u{2028}' | '\u{2029}' | '\u{0000}'..='\u{001f}' => {
                write!(out, "\\u{:04x}", c as u32).expect("writing to a String cannot fail");
            }
            _ => out.push(c),
        }
    }
    out.push('"');
}

pub fn chain_input(fields: &ChainFields<'_>) -> String {
    let mut out = String::from("[");
    for (index, field) in [
        "vesta-audit-v1",
        fields.prev_hash,
        fields.server_id,
        fields.principal_id,
        fields.device_id,
        fields.source,
        fields.action,
        fields.target,
        fields.outcome,
    ]
    .iter()
    .enumerate()
    {
        if index != 0 {
            out.push(',');
        }
        gson_string(&mut out, field);
    }
    out.push(',');
    if let Some(operation) = fields.operation_id {
        gson_string(&mut out, operation);
    } else {
        out.push_str("null");
    }
    write!(out, ",{},", fields.at_ms).expect("writing to a String cannot fail");
    gson_string(&mut out, fields.metadata_json);
    out.push(']');
    out
}

pub fn chain_hash(fields: &ChainFields<'_>) -> String {
    hex::encode(Sha256::digest(chain_input(fields).as_bytes()))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditEntry {
    pub seq: i64,
    pub server_id: String,
    pub principal_id: String,
    pub device_id: String,
    pub source: String,
    pub action: String,
    pub target: String,
    pub outcome: String,
    pub operation_id: Option<String>,
    pub at_ms: i64,
    pub metadata_json: String,
    pub prev_hash: String,
    pub hash: String,
}

impl AuditEntry {
    fn fields(&self) -> ChainFields<'_> {
        ChainFields {
            prev_hash: &self.prev_hash,
            server_id: &self.server_id,
            principal_id: &self.principal_id,
            device_id: &self.device_id,
            source: &self.source,
            action: &self.action,
            target: &self.target,
            outcome: &self.outcome,
            operation_id: self.operation_id.as_deref(),
            at_ms: self.at_ms,
            metadata_json: &self.metadata_json,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditVerification {
    pub valid: bool,
    pub checked: u64,
    pub first_broken_seq: Option<i64>,
}

const COLUMNS: &str = "seq,server_id,principal_id,device_id,source,action,target,outcome,operation_id,at,metadata_json,prev_hash,hash";

fn map_row(row: &Row<'_>) -> rusqlite::Result<AuditEntry> {
    Ok(AuditEntry {
        seq: row.get(0)?,
        server_id: row.get(1)?,
        principal_id: row.get(2)?,
        device_id: row.get(3)?,
        source: row.get(4)?,
        action: row.get(5)?,
        target: row.get(6)?,
        outcome: row.get(7)?,
        operation_id: row.get(8)?,
        at_ms: row.get(9)?,
        metadata_json: row.get(10)?,
        prev_hash: row.get(11)?,
        hash: row.get(12)?,
    })
}

#[derive(Clone)]
pub struct AuditLog {
    db: StateDatabase,
}

impl AuditLog {
    pub fn new(db: StateDatabase) -> Result<Self> {
        let log = Self { db };
        if !log.verify_chain()?.valid {
            return Err(StateError::new("state_audit_chain_corrupt"));
        }
        Ok(log)
    }

    /// The connection is trusted, in-process and owned by the caller's write transaction.
    /// An append failure must propagate so the sensitive mutation is rolled back.
    pub fn append(&self, c: &Connection, record: &AuditRecord) -> Result<i64> {
        use rusqlite::OptionalExtension;
        let prev: Option<String> = c
            .query_row(
                "SELECT hash FROM audit_events ORDER BY seq DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let prev = prev.as_deref().unwrap_or(GENESIS_HASH);
        let at_ms = self.db.clock().millis();
        let metadata = record.metadata_json();
        let hash = chain_hash(&ChainFields {
            prev_hash: prev,
            server_id: self.db.server_id(),
            principal_id: record.actor.principal_id(),
            device_id: record.actor.device_id(),
            source: record.actor.source(),
            action: &record.action,
            target: &record.target,
            outcome: &record.outcome,
            operation_id: record.operation_id.as_deref(),
            at_ms,
            metadata_json: &metadata,
        });
        c.execute(
            "INSERT INTO audit_events (server_id,principal_id,device_id,source,action,target,outcome,operation_id,at,metadata_json,prev_hash,hash) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
            params![self.db.server_id(), record.actor.principal_id(), record.actor.device_id(),
                record.actor.source(), record.action, record.target, record.outcome,
                record.operation_id, at_ms, metadata, prev, hash],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn append_record(&self, record: &AuditRecord) -> Result<i64> {
        self.db.write(|c| self.append(c, record))
    }

    pub fn verify_chain(&self) -> Result<AuditVerification> {
        self.db.read(|c| {
            let mut statement =
                c.prepare(&format!("SELECT {COLUMNS} FROM audit_events ORDER BY seq"))?;
            let mut rows = statement.query([])?;
            let mut expected = GENESIS_HASH.to_owned();
            let mut checked = 0;
            while let Some(row) = rows.next()? {
                let entry = map_row(row)?;
                if entry.prev_hash != expected
                    || entry.server_id != self.db.server_id()
                    || entry.hash != chain_hash(&entry.fields())
                {
                    return Ok(AuditVerification {
                        valid: false,
                        checked,
                        first_broken_seq: Some(entry.seq),
                    });
                }
                expected = entry.hash;
                checked += 1;
            }
            Ok(AuditVerification {
                valid: true,
                checked,
                first_broken_seq: None,
            })
        })
    }

    pub fn page(&self, after_seq: i64, limit: i32) -> Result<Vec<AuditEntry>> {
        self.db.read(|c| {
            let mut statement = c.prepare(&format!(
                "SELECT {COLUMNS} FROM audit_events WHERE seq > ? ORDER BY seq LIMIT ?"
            ))?;
            let rows = statement
                .query_map(params![after_seq, limit.clamp(1, MAX_PAGE as i32)], map_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }
}
