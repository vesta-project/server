//! Credential-storage-free recovery documents, optionally passphrase encrypted.
//! Columns are an explicit schema-reviewed allowlist. BLOBs are never guessed.
use crate::{Result, StateDatabase, StateError};
use rusqlite::types::ValueRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
};
use zeroize::Zeroizing;

#[path = "export_crypto.rs"]
mod crypto;
#[path = "export_import.rs"]
mod importing;
pub use importing::{ImportSummary, import_recovery_export};

const MAX_ROWS: u64 = 5_000_000;

// Public-key digests and audit chain hashes are not hashes of secrets. The one
// explicitly specified binary encoding is management_device_keys.public_key_hex.
const INCLUDED: &[(&str, &[&str])] = &[
    (
        "moderation_actions",
        &[
            "id",
            "server_id",
            "principal_id",
            "device_id",
            "operation_id",
            "action",
            "player",
            "reason",
            "duration_seconds",
            "expires_at",
            "created_at",
            "outcome",
            "expiry_state",
            "expiry_operation_id",
        ],
    ),
    ("server_identity", &["singleton", "server_id", "created_at"]),
    (
        "principals",
        &[
            "id",
            "server_id",
            "display_name",
            "status",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "user_minecraft_accounts",
        &["server_id", "minecraft_uuid", "principal_id"],
    ),
    ("user_minecraft_names", &["principal_id", "name"]),
    ("user_default_roles", &["principal_id", "role_id"]),
    (
        "roles",
        &["id", "name", "rank", "builtin", "created_by", "created_at"],
    ),
    ("role_capabilities", &["role_id", "capability", "scope"]),
    (
        "role_assignments",
        &[
            "id",
            "principal_id",
            "role_id",
            "granted_by",
            "created_at",
            "expires_at",
            "revoked_at",
            "revoked_by",
            "expiry_recorded_at",
        ],
    ),
    (
        "principal_overrides",
        &[
            "id",
            "principal_id",
            "capability",
            "scope",
            "effect",
            "granted_by",
            "created_at",
            "expires_at",
            "revoked_at",
            "revoked_by",
            "expiry_recorded_at",
        ],
    ),
    ("access_role_sources", &["singleton", "settings_json"]),
    (
        "devices",
        &[
            "id",
            "server_id",
            "principal_id",
            "label",
            "public_key_sha256",
            "created_at",
            "revoked_at",
            "revoked_by",
        ],
    ),
    (
        "management_device_keys",
        &[
            "device_id",
            "server_id",
            "public_key_hex",
            "linked_minecraft_uuid",
        ],
    ),
    (
        "workflows",
        &[
            "id",
            "name",
            "enabled",
            "principal_id",
            "definition_json",
            "created_at",
            "updated_at",
            "revision",
        ],
    ),
    ("workflow_schedule", &["workflow_id", "last_occurrence"]),
    (
        "notifications",
        &[
            "cursor",
            "id",
            "created_at",
            "severity",
            "title",
            "body",
            "source",
        ],
    ),
    (
        "notification_state",
        &[
            "principal_id",
            "notification_id",
            "read_at",
            "acknowledged_at",
        ],
    ),
    ("pack_minimum_policy", &["server_id", "minimum_revision"]),
    (
        "settings",
        &[
            "server_id",
            "key",
            "desired_value",
            "desired_revision",
            "apply_mode",
            "effective_value",
            "effective_revision",
            "updated_at",
        ],
    ),
    (
        "backup_policy",
        &[
            "singleton",
            "policy_json",
            "revision",
            "updated_at",
            "updated_by",
        ],
    ),
    (
        "audit_events",
        &[
            "seq",
            "server_id",
            "principal_id",
            "device_id",
            "source",
            "action",
            "target",
            "outcome",
            "operation_id",
            "at",
            "metadata_json",
            "prev_hash",
            "hash",
        ],
    ),
];

#[cfg(test)]
const EXCLUDED: &[&str] = &[
    "pack_passwords",
    "device_password_grants",
    "bootstrap_codes",
    "management_pairing_codes",
    "management_invitation_issuers",
    "management_invitation_ids",
    "management_owner_links",
    "confirmations",
    "management_permission_receipts",
    "local_authority_uses",
    "events",
    "operations",
    "operation_resources",
    "resource_locks",
    "file_intents",
    "file_trash",
    "file_uploads",
    "management_file_outcomes",
    "management_pack_outcomes",
    "backup_jobs",
    "metrics_raw",
    "metrics_5m",
    "metrics_daily",
    "metrics_activity",
    "metrics_status",
    "metrics_policy",
    "workflow_runs",
    "run_steps",
    "pack_releases",
    "pack_workspace",
    "pack_workspace_blobs",
    "pack_workspace_receipts",
    "pack_workspace_published",
    "pack_publication_history",
    "pack_workspace_uploads",
    "meta",
    // SQLite's AUTOINCREMENT bookkeeping is runtime state, not application data.
    "sqlite_sequence",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExportSummary {
    pub tables: usize,
    pub rows: u64,
}

/// Stream one consistent read snapshot. Errors may leave partial JSON in `out`;
/// callers must publish only after success. At most five million rows and 256 MiB
/// of plaintext are emitted, matching the recovery reader's limits.
pub fn write_recovery_export(
    db: &StateDatabase,
    out: &mut impl Write,
    passphrase: Option<&[u8]>,
) -> Result<ExportSummary> {
    if let Some(passphrase) = passphrase {
        let mut encrypted = crypto::EncryptWriter::new(out, passphrase)?;
        let summary = write_snapshot(db, &mut encrypted, MAX_ROWS)?;
        encrypted.finish()?;
        Ok(summary)
    } else {
        write_plain_snapshot(db, out, crypto::MAX_BYTES)
    }
}

fn write_plain_snapshot(
    db: &StateDatabase,
    out: &mut impl Write,
    max_bytes: usize,
) -> Result<ExportSummary> {
    struct Bounded<'a, W> {
        out: &'a mut W,
        remaining: usize,
        overflowed: bool,
    }
    impl<W: Write> Write for Bounded<'_, W> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.remaining {
                self.overflowed = true;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    "export too large",
                ));
            }
            let written = self.out.write(bytes)?;
            self.remaining -= written;
            Ok(written)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.out.flush()
        }
    }
    let mut bounded = Bounded {
        out,
        remaining: max_bytes,
        overflowed: false,
    };
    let result = write_snapshot(db, &mut bounded, MAX_ROWS);
    if bounded.overflowed {
        Err(StateError::new("state_export_size_limit"))
    } else {
        result
    }
}

/// Parsed, schema-validated recovery state. Import validates again before mutation.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryExport {
    pub format: String,
    pub version: u32,
    pub schema_version: u32,
    pub server_id: String,
    pub exported_at_ms: i64,
    #[serde(deserialize_with = "read_tables")]
    pub tables: BTreeMap<String, RecoveryTable>,
}

impl std::fmt::Debug for RecoveryExport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryExport")
            .field("tables", &self.tables.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryTable {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

fn read_tables<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<BTreeMap<String, RecoveryTable>, D::Error> {
    struct Tables;
    impl<'de> serde::de::Visitor<'de> for Tables {
        type Value = BTreeMap<String, RecoveryTable>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("unique recovery tables")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut tables = BTreeMap::new();
            while let Some((name, table)) = map.next_entry::<String, RecoveryTable>()? {
                if tables.len() >= INCLUDED.len() || tables.insert(name, table).is_some() {
                    return Err(serde::de::Error::custom("invalid recovery tables"));
                }
            }
            Ok(tables)
        }
    }
    d.deserialize_map(Tables)
}

/// Read at most 256 MiB of plaintext, rejecting unauthenticated STREAM input.
pub fn read_recovery_export(
    input: &mut impl Read,
    passphrase: Option<&[u8]>,
) -> Result<RecoveryExport> {
    let mut prefix = [0u8; 8];
    let mut length = 0;
    while length < prefix.len() {
        let count = input.read(&mut prefix[length..])?;
        if count == 0 {
            break;
        }
        length += count;
    }
    let bytes = if length == prefix.len() && &prefix == crypto::MAGIC {
        crypto::decrypt(input, &prefix, passphrase)?
    } else {
        let mut bytes = Zeroizing::new(Vec::new());
        bytes.extend_from_slice(&prefix[..length]);
        input
            .take((crypto::MAX_BYTES - length + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > crypto::MAX_BYTES {
            return Err(StateError::new("state_export_size_limit"));
        }
        bytes
    };
    let export: RecoveryExport =
        serde_json::from_slice(&bytes).map_err(|_| StateError::new("state_export_invalid"))?;
    export.validate()?;
    Ok(export)
}

impl RecoveryExport {
    pub(super) fn validate(&self) -> Result<()> {
        let invalid = || StateError::new("state_export_invalid");
        if self.format != "vesta-recovery-export"
            || self.version != 1
            || uuid::Uuid::parse_str(&self.server_id).is_err()
            || self.tables.len() != INCLUDED.len()
        {
            return Err(invalid());
        }
        let mut count = 0u64;
        for &(name, columns) in INCLUDED {
            let table = self.tables.get(name).ok_or_else(invalid)?;
            if table
                .columns
                .iter()
                .map(String::as_str)
                .ne(columns.iter().copied())
            {
                return Err(invalid());
            }
            count = count
                .checked_add(table.rows.len() as u64)
                .filter(|n| *n <= MAX_ROWS)
                .ok_or_else(|| StateError::new("state_export_row_limit"))?;
            for row in &table.rows {
                // Configuration is portable; queued lifecycle intent and native
                // observation state must never be resurrected by an import.
                if name == "settings"
                    && matches!(
                        row.get(1).and_then(Value::as_str),
                        Some("sleep-runtime" | "whitelist-runtime")
                    )
                {
                    return Err(invalid());
                }
                if row.len() != columns.len() {
                    return Err(invalid());
                }
                for (column, value) in columns.iter().zip(row) {
                    if !(value.is_null() || value.is_string() || value.is_number()) {
                        return Err(invalid());
                    }
                    if *column == "server_id" && value.as_str() != Some(&self.server_id) {
                        return Err(invalid());
                    }
                    if let Value::Number(number) = value
                        && ((number.is_u64() && number.as_i64().is_none())
                            || number.as_f64().is_none_or(|n| !n.is_finite()))
                    {
                        return Err(invalid());
                    }
                }
            }
        }
        let identity = &self.tables["server_identity"].rows;
        if identity.len() != 1
            || identity[0][0].as_i64() != Some(1)
            || identity[0][1].as_str() != Some(&self.server_id)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

fn write_snapshot(
    db: &StateDatabase,
    out: &mut impl Write,
    max_rows: u64,
) -> Result<ExportSummary> {
    db.read(|c| {
        let schema_version: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let server_id: String = c.query_row(
            "SELECT server_id FROM server_identity WHERE singleton = 1",
            [],
            |r| r.get(0),
        )?;
        out.write_all(b"{\"format\":\"vesta-recovery-export\",\"version\":1,\"schema_version\":")?;
        write!(out, "{schema_version},\"server_id\":")?;
        write_string(out, &server_id)?;
        write!(
            out,
            ",\"exported_at_ms\":{},\"tables\":{{",
            db.clock().millis()
        )?;
        let mut summary = ExportSummary { tables: 0, rows: 0 };
        for &(table, columns) in INCLUDED {
            if summary.tables != 0 {
                out.write_all(b",")?;
            }
            write_string(out, table)?;
            out.write_all(b":{\"columns\":[")?;
            for (index, column) in columns.iter().enumerate() {
                if index != 0 {
                    out.write_all(b",")?;
                }
                write_string(out, column)?;
            }
            out.write_all(b"],\"rows\":[")?;
            // Only trusted program constants form this query; never SELECT *.
            let selection = if table == "management_device_keys" {
                "device_id,server_id,lower(hex(public_key)) AS public_key_hex,linked_minecraft_uuid"
                    .to_owned()
            } else {
                columns.join(",")
            };
            let filter = if table == "settings" {
                " WHERE key NOT IN ('sleep-runtime','whitelist-runtime')"
            } else {
                ""
            };
            let sql = format!("SELECT {selection} FROM {table}{filter}");
            let mut statement = c.prepare(&sql)?;
            let mut rows = statement.query([])?;
            let mut first = true;
            while let Some(row) = rows.next()? {
                if summary.rows >= max_rows {
                    return Err(StateError::new("state_export_row_limit"));
                }
                if !first {
                    out.write_all(b",")?;
                }
                first = false;
                out.write_all(b"[")?;
                for index in 0..columns.len() {
                    if index != 0 {
                        out.write_all(b",")?;
                    }
                    write_value(out, row.get_ref(index)?)?;
                }
                out.write_all(b"]")?;
                summary.rows += 1;
            }
            out.write_all(b"]}")?;
            summary.tables += 1;
        }
        out.write_all(b"}}\n")?;
        Ok(summary)
    })
}

fn write_value(out: &mut impl Write, value: ValueRef<'_>) -> Result<()> {
    match value {
        ValueRef::Null => out.write_all(b"null")?,
        ValueRef::Integer(value) => write!(out, "{value}")?,
        ValueRef::Real(value) if value.is_finite() => write!(out, "{value}")?,
        ValueRef::Real(_) => return Err(StateError::new("state_export_nonfinite")),
        ValueRef::Text(value) => write_string(
            out,
            std::str::from_utf8(value).map_err(|_| StateError::new("state_export_invalid_text"))?,
        )?,
        ValueRef::Blob(_) => return Err(StateError::new("state_export_blob")),
    }
    Ok(())
}

fn write_string(out: &mut impl Write, value: &str) -> Result<()> {
    out.write_all(b"\"")?;
    let mut start = 0;
    for (index, byte) in value.bytes().enumerate() {
        if byte == b'"' || byte == b'\\' || byte < 0x20 {
            out.write_all(&value.as_bytes()[start..index])?;
            match byte {
                b'"' => out.write_all(b"\\\"")?,
                b'\\' => out.write_all(b"\\\\")?,
                _ => write!(out, "\\u{byte:04x}")?,
            }
            start = index + 1;
        }
    }
    out.write_all(&value.as_bytes()[start..])?;
    out.write_all(b"\"")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StateDirectory, SystemClock};
    use std::{collections::BTreeSet, sync::Arc};

    #[test]
    fn every_migrated_table_has_exactly_one_classification() {
        let root = tempfile::tempdir().unwrap();
        let db = StateDatabase::open(
            StateDirectory::new(root.path().join("state"), None).unwrap(),
            Arc::new(SystemClock),
        )
        .unwrap();
        db.read(|c| {
            let included: BTreeSet<_> = INCLUDED.iter().map(|(table, _)| *table).collect();
            let excluded: BTreeSet<_> = EXCLUDED.iter().copied().collect();
            assert_eq!(included.len(), INCLUDED.len());
            assert_eq!(excluded.len(), EXCLUDED.len());
            assert!(included.is_disjoint(&excluded));
            let mut statement = c.prepare("SELECT name FROM sqlite_schema WHERE type = 'table'")?;
            let actual = statement
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<BTreeSet<_>>>()?;
            let classified = included.union(&excluded).map(|s| (*s).to_owned()).collect();
            assert_eq!(
                actual, classified,
                "new tables must be explicitly classified"
            );
            Ok(())
        })
        .unwrap();
        db.close().unwrap();
    }

    #[test]
    fn plaintext_export_refuses_output_beyond_the_reader_limit() {
        let root = tempfile::tempdir().unwrap();
        let db = StateDatabase::open(
            StateDirectory::new(root.path().join("state"), None).unwrap(),
            Arc::new(SystemClock),
        )
        .unwrap();
        let mut full = Vec::new();
        write_recovery_export(&db, &mut full, None).unwrap();
        read_recovery_export(&mut full.as_slice(), None).unwrap();

        let mut exact = Vec::new();
        write_plain_snapshot(&db, &mut exact, full.len()).unwrap();
        assert_eq!(exact.len(), full.len());
        let mut too_small = Vec::new();
        assert_eq!(
            write_plain_snapshot(&db, &mut too_small, full.len() - 1)
                .unwrap_err()
                .code(),
            "state_export_size_limit"
        );
        assert!(too_small.len() < full.len());
        assert!(read_recovery_export(&mut too_small.as_slice(), None).is_err());
        assert_eq!(
            write_plain_snapshot(&db, &mut std::io::sink(), 0)
                .unwrap_err()
                .code(),
            "state_export_size_limit"
        );
        db.close().unwrap();
    }

    #[test]
    fn json_values_escape_and_reject_unsupported_representations() {
        let mut bytes = Vec::new();
        write_string(&mut bytes, "é\"\\\n\t\0").unwrap();
        assert_eq!(
            serde_json::from_slice::<String>(&bytes).unwrap(),
            "é\"\\\n\t\0"
        );
        for value in [
            ValueRef::Null,
            ValueRef::Integer(i64::MIN),
            ValueRef::Real(1.25),
        ] {
            bytes.clear();
            write_value(&mut bytes, value).unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
        }
        assert_eq!(
            write_value(&mut bytes, ValueRef::Blob(&[1]))
                .unwrap_err()
                .code(),
            "state_export_blob"
        );
        assert_eq!(
            write_value(&mut bytes, ValueRef::Real(f64::INFINITY))
                .unwrap_err()
                .code(),
            "state_export_nonfinite"
        );
        assert_eq!(
            write_value(&mut bytes, ValueRef::Text(&[0xff]))
                .unwrap_err()
                .code(),
            "state_export_invalid_text"
        );
    }

    #[test]
    fn export_uses_one_snapshot_even_while_another_writer_commits() {
        let root = tempfile::tempdir().unwrap();
        let db = StateDatabase::open(
            StateDirectory::new(root.path().join("state"), None).unwrap(),
            Arc::new(SystemClock),
        )
        .unwrap();
        struct ConcurrentWriter {
            bytes: Vec<u8>,
            db: Option<StateDatabase>,
        }
        impl Write for ConcurrentWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(db) = self.db.take() {
                    std::thread::spawn(move || db.write(|c| {
                        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES ('later',?,'Later','active',0,0)", [db.server_id()])?;
                        Ok(())
                    }).unwrap()).join().unwrap();
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut out = ConcurrentWriter {
            bytes: Vec::new(),
            db: Some(db.clone()),
        };
        let summary = write_recovery_export(&db, &mut out, None).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out.bytes).unwrap();
        assert!(
            json["tables"]["principals"]["rows"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let exported_rows: u64 = json["tables"]
            .as_object()
            .unwrap()
            .values()
            .map(|table| table["rows"].as_array().unwrap().len() as u64)
            .sum();
        assert_eq!(summary.rows, exported_rows);
        db.read(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM principals", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .unwrap();
        db.close().unwrap();
    }

    #[test]
    fn row_limit_and_writer_failure_are_closed_errors() {
        let root = tempfile::tempdir().unwrap();
        let db = StateDatabase::open(
            StateDirectory::new(root.path().join("state"), None).unwrap(),
            Arc::new(SystemClock),
        )
        .unwrap();
        assert_eq!(
            write_snapshot(&db, &mut Vec::new(), 0).unwrap_err().code(),
            "state_export_row_limit"
        );
        struct FailedWriter;
        impl Write for FailedWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("fixture"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            write_recovery_export(&db, &mut FailedWriter, None)
                .unwrap_err()
                .code(),
            "state_io"
        );
        db.close().unwrap();
    }
}
