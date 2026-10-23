//! Presentation only. Structured replies and command effects stay unchanged.
use crate::{control::Op, local_files};
use serde_json::Value;
use std::fmt::Write;

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Format {
    Human,
    Json,
}

// JSON previously escaped these bytes. Human output must not turn a device name
// or other stored string into terminal escape sequences, extra lines or bidi controls.
fn safe(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for c in value.chars() {
        if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
            let _ = write!(result, "{}", c.escape_default());
        } else {
            result.push(c);
        }
    }
    result
}
fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "Unknown".into(),
        Value::Bool(value) => if *value { "Yes" } else { "No" }.into(),
        Value::String(value) => safe(value),
        Value::Number(value) => value.to_string(),
        _ => "Unknown".into(),
    }
}
fn label(key: &str) -> String {
    match key {
        "minecraft_uuid" => "Minecraft UUID".into(),
        "tls_sha256_fingerprint" => "TLS SHA-256 fingerprint".into(),
        "expires_at_ms" => "Expires at (Unix ms)".into(),
        "password" => "Password role".into(),
        _ => {
            let words = key.replace('_', " ");
            let mut chars = words.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        }
    }
}
fn record(out: &mut String, data: &Value, indent: usize) {
    let pad = " ".repeat(indent);
    match data {
        Value::Object(fields) => {
            for (key, value) in fields {
                if value.is_object() || value.is_array() {
                    let _ = writeln!(out, "{pad}{}:", safe(&label(key)));
                    record(out, value, indent + 2);
                } else {
                    let _ = writeln!(out, "{pad}{:<22} {}", safe(&label(key)), scalar(value));
                }
            }
        }
        Value::Array(items) if items.is_empty() => {
            let _ = writeln!(out, "{pad}None.");
        }
        Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                record(out, value, indent);
            }
        }
        _ => {
            let _ = writeln!(out, "{pad}{}", scalar(data));
        }
    }
}

pub fn management(format: Format, op: &Op, data: &Value) -> local_files::Result<String> {
    if format == Format::Json {
        return serde_json::to_string(data).map_err(|_| "local_control_invalid_response");
    }
    let mut out = String::new();
    match op {
        Op::Status => {
            out.push_str("Server status\n\n");
            for (label, key) in [
                ("Agent", "agent"),
                ("Minecraft", "state"),
                ("Readiness", "readiness"),
                ("Players", "playerCount"),
                ("Generation", "generation"),
            ] {
                let _ = writeln!(out, "  {label:<14} {}", scalar(&data[key]));
            }
        }
        Op::PlayersList => {
            out.push_str("Players\n\n");
            if data["available"] != true {
                let reason = data["reason"]
                    .as_str()
                    .unwrap_or("players_query_unavailable");
                let message = match reason {
                    "players_query_not_running" => "Minecraft is not running.",
                    "players_query_busy" => "A player query is already in progress.",
                    "players_query_timeout" => {
                        "Minecraft did not answer before the query timed out."
                    }
                    "players_query_stopping" => "The agent is shutting down.",
                    "players_query_stale_generation" => "Minecraft restarted during the query.",
                    _ => "A reliable player list is unavailable.",
                };
                let _ = writeln!(
                    out,
                    "  {message}\n  Player count is unknown.\n  Reason: {}",
                    safe(reason)
                );
            } else {
                let names = data["players"]
                    .as_array()
                    .ok_or("local_control_invalid_response")?;
                let _ = writeln!(
                    out,
                    "  Online: {} / {}",
                    scalar(&data["online"]),
                    scalar(&data["max"])
                );
                if names.is_empty() {
                    out.push_str("  No players online.\n");
                }
                for name in names {
                    let _ = writeln!(out, "  - {}", scalar(name));
                }
            }
        }
        _ => {
            let title = match op {
                Op::Pair { .. } => "Pair a device",
                Op::DevicesList => "Devices",
                Op::DevicesRevoke { .. } => "Device revoked",
                Op::OwnerList => "Linked Minecraft accounts",
                Op::OwnerLink { .. } => "Minecraft account linked",
                Op::OwnerUnlink { .. } => "Minecraft account unlinked",
                Op::AccessShow | Op::AccessSet { .. } => "Access settings",
                Op::Superadmin { .. } => "Administrator access",
                _ => return Err("local_control_invalid_response"),
            };
            let _ = writeln!(out, "{title}\n");
            record(&mut out, data, 2);
            if matches!(op, Op::Pair { .. }) {
                out.push_str(
                    "\n  This invitation grants access. Share it only with the intended device.\n",
                );
            }
        }
    }
    Ok(out.trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_observations_are_never_presented_as_zero_or_ready() {
        let status = serde_json::json!({"agent":"online","state":"stopped","readiness":null,"playerCount":null,"generation":null});
        let text = management(Format::Human, &Op::Status, &status).unwrap();
        assert!(text.contains("Server status") && text.contains("Unknown"));
        assert!(!text.contains('0') && !text.contains('{'));
        let unavailable =
            serde_json::json!({"available":false,"reason":"players_query_timeout","players":null});
        let text = management(Format::Human, &Op::PlayersList, &unavailable).unwrap();
        assert!(text.contains("timed out") && text.contains("count is unknown"));
        assert!(!text.contains("No players online"));
        assert_eq!(
            serde_json::from_str::<Value>(
                &management(Format::Json, &Op::PlayersList, &unavailable).unwrap()
            )
            .unwrap(),
            unavailable
        );
    }
    #[test]
    fn player_and_device_names_cannot_control_the_terminal() {
        let players = serde_json::json!({"available":true,"online":1,"max":20,"players":["player\u{1b}[2J\r\n"]});
        let text = management(Format::Human, &Op::PlayersList, &players).unwrap();
        assert!(text.contains("1 / 20") && text.contains("\\u{1b}[2J\\r\\n"));
        assert!(!text.contains('\u{1b}') && !text.contains('\r'));
        let devices = serde_json::json!([{"device_name":"hello\u{202e}","revoked":false}]);
        assert!(
            !management(Format::Human, &Op::DevicesList, &devices)
                .unwrap()
                .contains('\u{202e}')
        );
    }
    #[test]
    fn known_empty_list_and_unknown_list_are_distinct() {
        let empty = serde_json::json!({"available":true,"online":0,"max":20,"players":[]});
        assert!(
            management(Format::Human, &Op::PlayersList, &empty)
                .unwrap()
                .contains("No players online")
        );
        assert!(
            management(Format::Human, &Op::DevicesList, &serde_json::json!([]))
                .unwrap()
                .contains("None.")
        );
    }
}
