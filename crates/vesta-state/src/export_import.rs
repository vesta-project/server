//! Atomic disaster recovery into an exclusively owned fresh control plane.
use super::{INCLUDED, RecoveryExport};
use crate::{
    Clock, Result, StateDatabase, StateDirectory, StateError,
    audit::{self, AuditActor, AuditRecord},
};
use rusqlite::{
    Connection, params, params_from_iter,
    types::{Value as SqlValue, ValueRef},
};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ImportSummary {
    pub server_id: String,
    pub tables: usize,
    pub rows: u64,
}

/// Opens privately, imports in one write transaction, and closes before returning.
/// Reopen the directory afterwards to acquire the adopted cached server identity.
/// The runner lock excludes live runtimes; callers also hold their local server leases.
pub fn import_recovery_export(
    directory: StateDirectory,
    clock: Arc<dyn Clock>,
    export: &RecoveryExport,
) -> Result<ImportSummary> {
    export.validate()?;
    if export.schema_version != crate::CURRENT_SCHEMA_VERSION {
        return Err(StateError::new("state_import_schema_mismatch"));
    }
    directory.prepare()?;
    let _runner = directory.acquire_runner_lock()?;
    let db = StateDatabase::open(directory, clock.clone())?;
    let result = db.write(|c| import_transaction(c, clock.millis(), export));
    let closed = db.close();
    let summary = result?;
    closed?;
    Ok(summary)
}

const MUST_BE_EMPTY: &[&str] = &[
    "principals",
    "devices",
    "role_assignments",
    "principal_overrides",
    "user_minecraft_accounts",
    "user_minecraft_names",
    "user_default_roles",
    "management_device_keys",
    "workflows",
    "workflow_schedule",
    "notifications",
    "notification_state",
    "backup_policy",
    "moderation_actions",
    "pack_minimum_policy",
    "settings",
    "audit_events",
    "pack_passwords",
    "device_password_grants",
    "bootstrap_codes",
    "management_pairing_codes",
    "management_invitation_issuers",
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
    "workflow_runs",
    "run_steps",
    "pack_releases",
];

fn nonempty() -> StateError {
    StateError::new("state_import_target_not_empty")
}
fn builtins_mismatch() -> StateError {
    StateError::new("state_import_builtin_mismatch")
}

fn fresh(c: &Connection) -> Result<()> {
    for table in MUST_BE_EMPTY {
        if c.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table})"), [], |r| {
            r.get::<_, bool>(0)
        })? {
            return Err(nonempty());
        }
    }
    if c.query_row("SELECT count(*) FROM roles WHERE builtin=0", [], |r| {
        r.get::<_, i64>(0)
    })? != 2
        || c.query_row("SELECT count(*) FROM meta", [], |r| r.get::<_, i64>(0))? != 1
        || c.query_row(
            "SELECT count(*) FROM role_capabilities WHERE role_id IN ('public','player')",
            [],
            |r| r.get::<_, i64>(0),
        )? != 4
    {
        return Err(nonempty());
    }
    let actual = query_rows(
        c,
        "roles",
        &["id", "name", "rank", "builtin", "created_by", "created_at"],
        "WHERE builtin=0",
    )?;
    let expected = vec![
        vec![
            Value::from("player"),
            Value::from("Player"),
            Value::from(10),
            Value::from(0),
            Value::from("migration"),
            Value::from(0),
        ],
        vec![
            Value::from("public"),
            Value::from("Public"),
            Value::from(1),
            Value::from(0),
            Value::from("migration"),
            Value::from(0),
        ],
    ];
    let sorted = |rows: Vec<Vec<Value>>| -> BTreeSet<String> {
        rows.iter()
            .map(|row| serde_json::to_string(row).expect("JSON values are serializable"))
            .collect()
    };
    if sorted(actual) != sorted(expected) {
        return Err(nonempty());
    }
    let capabilities = query_rows(
        c,
        "role_capabilities",
        &["role_id", "capability", "scope"],
        "WHERE role_id IN ('public','player')",
    )?;
    let expected = vec![
        vec![
            Value::from("public"),
            Value::from("public.info"),
            Value::from("*"),
        ],
        vec![
            Value::from("player"),
            Value::from("public.info"),
            Value::from("*"),
        ],
        vec![
            Value::from("player"),
            Value::from("pack.download"),
            Value::from("*"),
        ],
        vec![
            Value::from("player"),
            Value::from("pack.realtime"),
            Value::from("*"),
        ],
    ];
    if sorted(capabilities) != sorted(expected) {
        return Err(nonempty());
    }
    let access: Vec<Vec<Value>> = query_rows(
        c,
        "access_role_sources",
        &["singleton", "settings_json"],
        "",
    )?;
    if access.len() != 1
        || access[0][0] != 1
        || serde_json::from_str::<Value>(access[0][1].as_str().ok_or_else(nonempty)?)
            .map_err(|_| nonempty())?
            != serde_json::json!({"anonymous":"public","new_user":"public","whitelist":"player","whitelist_enabled":true,"password":"player"})
    {
        return Err(nonempty());
    }
    let meta = query_rows(c, "meta", &["key", "value"], "")?;
    if meta != vec![vec![Value::from("events_pruned_through"), Value::from("0")]]
        || c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_sequence WHERE seq != 0)",
            [],
            |r| r.get::<_, bool>(0),
        )?
    {
        return Err(nonempty());
    }
    Ok(())
}

fn compare_builtins(c: &Connection, export: &RecoveryExport) -> Result<BTreeSet<String>> {
    let columns = &["id", "name", "rank", "builtin"];
    let actual = query_rows(c, "roles", columns, "WHERE builtin=1")?;
    let to_map = |rows: Vec<Vec<Value>>| -> Result<BTreeMap<String, Vec<Value>>> {
        let mut result = BTreeMap::new();
        for row in rows {
            let id = row[0].as_str().ok_or_else(builtins_mismatch)?.to_owned();
            if result.insert(id, row).is_some() {
                return Err(builtins_mismatch());
            }
        }
        Ok(result)
    };
    let expected = to_map(actual)?;
    let exported = to_map(
        export.tables["roles"]
            .rows
            .iter()
            .filter(|r| r[3].as_i64() == Some(1))
            .map(|r| r[..4].to_vec())
            .collect(),
    )?;
    if expected != exported {
        return Err(builtins_mismatch());
    }
    let ids: BTreeSet<_> = expected.into_keys().collect();
    let actual = query_rows(
        c,
        "role_capabilities",
        &["role_id", "capability", "scope"],
        "",
    )?;
    let capabilities = |rows: &[Vec<Value>]| -> Result<BTreeSet<(String, String, String)>> {
        let mut result = BTreeSet::new();
        let mut count = 0;
        for row in rows {
            let id = row[0].as_str().ok_or_else(builtins_mismatch)?;
            if ids.contains(id) {
                count += 1;
                result.insert((
                    id.to_owned(),
                    row[1].as_str().ok_or_else(builtins_mismatch)?.to_owned(),
                    row[2].as_str().ok_or_else(builtins_mismatch)?.to_owned(),
                ));
            }
        }
        if result.len() != count {
            return Err(builtins_mismatch());
        }
        Ok(result)
    };
    if capabilities(&actual)? != capabilities(&export.tables["role_capabilities"].rows)? {
        return Err(builtins_mismatch());
    }
    Ok(ids)
}

fn import_transaction(c: &Connection, now: i64, export: &RecoveryExport) -> Result<ImportSummary> {
    fresh(c)?;
    let builtins = compare_builtins(c, export)?;
    // Deferred FK checking permits parent/child insertion in the documented table
    // order. Neither foreign-key enforcement nor any immutable trigger is disabled.
    c.execute_batch("PRAGMA defer_foreign_keys=ON; DELETE FROM access_role_sources; DELETE FROM role_capabilities WHERE role_id IN ('public','player'); DELETE FROM roles WHERE id IN ('public','player');")?;
    let identity = &export.tables["server_identity"].rows[0];
    c.execute(
        "UPDATE server_identity SET server_id=?,created_at=? WHERE singleton=1",
        params![export.server_id, sql_value(&identity[2])?],
    )?;
    let mut summary = ImportSummary {
        server_id: export.server_id.clone(),
        tables: INCLUDED.len(),
        rows: 0,
    };
    for &(name, columns) in INCLUDED {
        let table = &export.tables[name];
        summary.rows += table.rows.len() as u64;
        if name == "server_identity" {
            continue;
        }
        let column_names = if name == "management_device_keys" {
            "device_id,server_id,public_key,linked_minecraft_uuid".to_owned()
        } else {
            columns.join(",")
        };
        let placeholders = vec!["?"; columns.len()].join(",");
        let mut insert = c.prepare(&format!(
            "INSERT INTO {name} ({column_names}) VALUES ({placeholders})"
        ))?;
        let mut rows: Vec<_> = table.rows.iter().collect();
        if name == "audit_events" {
            rows.sort_by_key(|row| row[0].as_i64().unwrap_or(i64::MIN));
        }
        for row in rows {
            if (name == "roles" || name == "role_capabilities")
                && row[0].as_str().is_some_and(|id| builtins.contains(id))
            {
                continue;
            }
            let mut values = row.iter().map(sql_value).collect::<Result<Vec<_>>>()?;
            if name == "management_device_keys" {
                let key = row[2]
                    .as_str()
                    .ok_or_else(|| StateError::new("state_export_invalid"))?;
                if key.len() != 64
                    || key
                        .bytes()
                        .any(|b| !b.is_ascii_hexdigit() || b.is_ascii_uppercase())
                {
                    return Err(StateError::new("state_export_invalid"));
                }
                values[2] = SqlValue::Blob(
                    hex::decode(key).map_err(|_| StateError::new("state_export_invalid"))?,
                );
            }
            insert.execute(params_from_iter(values))?;
        }
    }
    if !audit::verify_on_connection(c, &export.server_id)?.valid {
        return Err(StateError::new("state_audit_chain_corrupt"));
    }
    let record = AuditRecord::new(
        AuditActor::new(
            "local-os-owner".into(),
            "local".into(),
            "protected-local-control".into(),
        )?,
        "recovery.import".into(),
        format!("server:{}", export.server_id),
        "accepted".into(),
        None,
        [],
    )?;
    audit::append_on_connection(c, &export.server_id, now, &record)?;
    Ok(summary)
}

fn sql_value(value: &Value) -> Result<SqlValue> {
    match value {
        Value::Null => Ok(SqlValue::Null),
        Value::String(text) => Ok(SqlValue::Text(text.clone())),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Ok(SqlValue::Integer(value))
            } else {
                number
                    .as_f64()
                    .filter(|n| n.is_finite())
                    .map(SqlValue::Real)
                    .ok_or_else(|| StateError::new("state_export_invalid"))
            }
        }
        _ => Err(StateError::new("state_export_invalid")),
    }
}

fn query_rows(
    c: &Connection,
    table: &str,
    columns: &[&str],
    suffix: &str,
) -> Result<Vec<Vec<Value>>> {
    let mut statement = c.prepare(&format!(
        "SELECT {} FROM {table} {suffix}",
        columns.join(",")
    ))?;
    let mut cursor = statement.query([])?;
    let mut rows = Vec::new();
    while let Some(row) = cursor.next()? {
        let mut values = Vec::new();
        for index in 0..columns.len() {
            values.push(match row.get_ref(index)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(value) => Value::from(value),
                ValueRef::Real(value) => Value::from(value),
                ValueRef::Text(text) => Value::from(
                    std::str::from_utf8(text)
                        .map_err(|_| StateError::new("state_export_invalid_text"))?,
                ),
                ValueRef::Blob(_) => return Err(StateError::new("state_export_blob")),
            });
        }
        rows.push(values);
    }
    Ok(rows)
}
