//! Human-readable replies for host-model CLI commands. JSON stays compact and unchanged.
use crate::output::Format;
use serde_json::Value;
use std::fmt::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Format a host-local command reply. `now` is injectable for tests.
pub fn format_reply(
    format: Format,
    command: &str,
    data: &Value,
    now: SystemTime,
    addresses: &[String],
    port: u16,
) -> Result<String, &'static str> {
    if format == Format::Json {
        return serde_json::to_string(data).map_err(|_| "host_local_invalid_response");
    }
    Ok(match command {
        "setup-code" => setup_code(data, now, addresses, port)?,
        "servers-removed" => {
            let mut text = String::from("Removed servers (recoverable for 7 days):\n");
            for r in data.as_array().into_iter().flatten() {
                text.push_str(&format!(
                    "{}  {}  removed {}  restore until {}\n",
                    r["serverId"].as_str().unwrap_or("?"),
                    r["name"].as_str().unwrap_or("?"),
                    r["removedAt"].as_str().unwrap_or("?"),
                    r["restoreUntil"].as_str().unwrap_or("?")
                ));
            }
            text
        }
        "servers-restored" => format!(
            "Restored {} ({}). The server is stopped and ready to start.\n",
            data["name"].as_str().unwrap_or("server"),
            data["serverId"].as_str().unwrap_or("?")
        ),
        "host-status" => host_status(data),
        "link" => link_code(data, now)?,
        "link-approve" | "link-inspect" => link_review(data),
        "link-approved" => link_approved(data),
        "link-recover" => link_recover(data),
        "not-approved" => "Device request was not approved.".into(),
        _ => return Err("host_local_invalid_response"),
    })
}

pub fn explain_error(code: &str) -> String {
    match code {
        "already_claimed" => {
            "This Vesta Server is already claimed. To add another device, run: sudo vesta link"
                .into()
        }
        "pairing_failed" => {
            "This Vesta Server is not ready for that step yet. Claim it with sudo vesta setup-code first, or use sudo vesta link after it is claimed."
                .into()
        }
        "setup_code_expired_or_replaced" => {
            "That setup code has expired or was replaced. Run: sudo vesta setup-code"
                .into()
        }
        "host_local_unavailable" => {
            "Vesta Server is not running. Start it with: sudo systemctl start vesta".into()
        }
        "host_local_acl_refused" => {
            "This command needs root. Run it with sudo.".into()
        }
        "host_local_invalid_arguments" => {
            "Those arguments are not valid. Run: vesta --help".into()
        }
        "host_local_no_request_to_approve" => {
            "No device is waiting for approval. Ask the other device to join with the link code, then run: sudo vesta link --approve"
                .into()
        }
        "host_local_request_refused" => {
            "Vesta Server refused that request. Check sudo vesta host-status, then try again.".into()
        }
        "host_local_outcome_unknown" | "host_local_io_failed" => {
            "Could not reach the local Vesta Server control channel. Is the service running? Try: sudo systemctl status vesta"
                .into()
        }
        "host_local_invalid_response" => {
            "Vesta Server returned an unexpected reply. Try: sudo systemctl restart vesta".into()
        }
        "host_local_runtime_unavailable" => {
            "Could not start the local command runtime. Reinstall Vesta Server and try again.".into()
        }
        other => format!(
            "Something went wrong ({other}). See: sudo journalctl -u vesta"
        ),
    }
}

fn setup_code(
    data: &Value,
    now: SystemTime,
    addresses: &[String],
    port: u16,
) -> Result<String, &'static str> {
    let code = data["code"].as_str().ok_or("host_local_invalid_response")?;
    let expires = data["expiresAt"]
        .as_str()
        .ok_or("host_local_invalid_response")?;
    let mut out = String::new();
    let _ = writeln!(out, "Setup code:  {code}");
    let _ = writeln!(out, "Valid for:   {}", validity(expires, now)?);
    let where_to = match addresses {
        [] => format!("type this machine's address and port {port}"),
        [one] if port == 25500 => format!("pick this machine or type {one}"),
        [one] => format!("pick this machine or type {one}:{port}"),
        many => {
            let list = many.join(", ");
            if port == 25500 {
                format!("pick this machine or type one of: {list}")
            } else {
                format!("pick this machine or type one of: {list} (port {port})")
            }
        }
    };
    let _ = writeln!(
        out,
        "In the Vesta launcher: Servers, Set up a Vesta Server, {where_to}"
    );
    let _ = write!(out, "Need a new code? Run: sudo vesta setup-code");
    Ok(out)
}

fn host_status(data: &Value) -> String {
    let host = data["hostId"].as_str().unwrap_or("Unknown");
    let claimed = data["claimed"].as_bool().unwrap_or(false);
    let mut out = String::new();
    let _ = writeln!(out, "Vesta Server status");
    out.push('\n');
    let _ = writeln!(out, "  Host ID:    {host}");
    let _ = writeln!(out, "  Claimed:    {}", if claimed { "Yes" } else { "No" });
    if claimed {
        let _ = write!(
            out,
            "\nThis machine is claimed. To add another device, run: sudo vesta link"
        );
    } else {
        let _ = write!(out, "\nNot claimed yet. Run: sudo vesta setup-code");
    }
    out
}

fn link_code(data: &Value, now: SystemTime) -> Result<String, &'static str> {
    let code = data["code"].as_str().ok_or("host_local_invalid_response")?;
    let expires = data["expiresAt"]
        .as_str()
        .ok_or("host_local_invalid_response")?;
    let link_id = data["linkId"].as_str().unwrap_or("");
    let mut out = String::new();
    let _ = writeln!(out, "Link code:   {code}");
    let _ = writeln!(out, "Valid for:   {}", validity(expires, now)?);
    if !link_id.is_empty() {
        let _ = writeln!(out, "Link ID:     {link_id}");
    }
    let _ = writeln!(
        out,
        "In the Vesta launcher: add this Vesta Server with the link code."
    );
    let _ = write!(
        out,
        "When a device asks to join, review it with: sudo vesta link --approve"
    );
    Ok(out)
}

fn link_review(data: &Value) -> String {
    let mut out = String::new();
    let state = data["state"].as_str().unwrap_or("unknown");
    let _ = writeln!(out, "Device link request");
    out.push('\n');
    let _ = writeln!(out, "  State:         {state}");
    if let Some(name) = data["deviceName"].as_str() {
        let _ = writeln!(out, "  Device name:   {name}");
    }
    if let Some(account) = data["accountId"].as_str() {
        let _ = writeln!(out, "  Account:       {account}");
    }
    if let Some(hash) = data["requestHash"].as_str() {
        let _ = writeln!(out, "  Request hash:  {hash}");
    }
    if let Some(id) = data["linkId"].as_str() {
        let _ = writeln!(out, "  Link ID:       {id}");
    }
    if state == "awaiting_request" {
        let _ = write!(
            out,
            "\nWaiting for a device to use the link code. Keep this terminal ready for approval."
        );
    } else {
        let _ = write!(out, "\nApprove only if this matches the device you expect.");
    }
    out
}

fn link_approved(data: &Value) -> String {
    let device = data["deviceId"].as_str().unwrap_or("Unknown");
    format!("Device approved.\n\n  Device ID:  {device}")
}

fn link_recover(data: &Value) -> String {
    let device = data["deviceId"].as_str().unwrap_or("Unknown");
    format!("Recovery complete.\n\n  Device ID:  {device}")
}

fn validity(expires_at: &str, now: SystemTime) -> Result<String, &'static str> {
    let until = parse_rfc3339(expires_at).ok_or("host_local_invalid_response")?;
    let secs = until
        .duration_since(now)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let minutes = secs.div_ceil(60);
    let label = if minutes <= 1 {
        "1 minute".into()
    } else {
        format!("{minutes} minutes")
    };
    Ok(format!("{label} (until {})", local_hm(until)))
}

fn parse_rfc3339(value: &str) -> Option<SystemTime> {
    // Expect the host's Secs/UTC form: 2026-11-13T09:52:00Z
    let value = value.strip_suffix('Z')?;
    let (date, time) = value.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let hour: u32 = t.next()?.parse().ok()?;
    let minute: u32 = t.next()?.parse().ok()?;
    let second: u32 = t.next()?.parse().ok()?;
    let days = days_from_civil(year, month, day)?;
    let secs = days * 86400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second);
    if secs < 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
}

fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400) as u64;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp as u64 + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe as i64 - 719468)
}

fn local_hm(time: SystemTime) -> String {
    // Prefer local wall clock when the platform offset is available; else UTC.
    let Ok(duration) = time.duration_since(UNIX_EPOCH) else {
        return "??:??".into();
    };
    let utc_secs = duration.as_secs() as i64;
    let offset = local_offset_secs();
    let local = utc_secs + offset;
    let day = local.rem_euclid(86400) as u32;
    format!("{:02}:{:02}", day / 3600, (day % 3600) / 60)
}

fn local_offset_secs() -> i64 {
    // Difference between local and UTC for "now"; good enough for HH:MM labels.
    let now = SystemTime::now();
    let Ok(utc) = now.duration_since(UNIX_EPOCH) else {
        return 0;
    };
    let utc_secs = utc.as_secs() as i64;
    match std::process::Command::new("date").arg("+%z").output() {
        Ok(o) if o.status.success() => parse_offset(std::str::from_utf8(&o.stdout).unwrap_or("")),
        _ => {
            // Fallback: treat as UTC when `date` is unavailable.
            let _ = utc_secs;
            0
        }
    }
}

fn parse_offset(text: &str) -> i64 {
    let text = text.trim();
    if text.len() < 5 {
        return 0;
    }
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let hours: i64 = text[1..3].parse().unwrap_or(0);
    let mins: i64 = text[3..5].parse().unwrap_or(0);
    sign * (hours * 3600 + mins * 60)
}

/// IPv4 addresses on global-scope interfaces (excludes loopback).
pub fn lan_addresses() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("ip")
            .args(["-o", "-4", "addr", "show", "scope", "global"])
            .output();
        let Ok(output) = output else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let mut addresses = Vec::new();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let iface = parts.nth(1).unwrap_or("");
            if iface == "lo" {
                continue;
            }
            if let Some(cidr) = parts.nth(1) {
                let ip = cidr.split('/').next().unwrap_or("");
                if !ip.is_empty() && !addresses.iter().any(|a| a == ip) {
                    addresses.push(ip.to_owned());
                }
            }
        }
        addresses
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Best-effort primary outbound IPv4 for non-Linux operator machines.
        let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") else {
            return Vec::new();
        };
        if socket.connect("1.1.1.1:80").is_err() {
            return Vec::new();
        }
        match socket.local_addr() {
            Ok(addr) if !addr.ip().is_loopback() => vec![addr.ip().to_string()],
            _ => Vec::new(),
        }
    }
}

/// Management port from host.toml beside the host state directory.
pub fn management_port(state: &std::path::Path) -> u16 {
    let Some(root) = state.parent() else {
        return 25500;
    };
    let Ok(text) = std::fs::read_to_string(root.join("host.toml")) else {
        return 25500;
    };
    for line in text.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("port") {
            let value = value.trim().trim_start_matches('=').trim();
            if let Ok(port) = value.parse()
                && port > 0
            {
                return port;
            }
        }
    }
    25500
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::Format;

    fn now_fixed() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_763_308_320) // 2025-11-16T12:32:00Z
    }

    #[test]
    fn setup_code_human_uses_real_expiry_and_addresses() {
        let now = now_fixed();
        let expires = now + Duration::from_secs(30 * 60);
        let expires_at = format_rfc3339(expires);
        let data = serde_json::json!({
            "code": "K7Q4-MX29-P3",
            "expiresAt": expires_at
        });
        let text = format_reply(
            Format::Human,
            "setup-code",
            &data,
            now,
            &["192.168.1.67".into()],
            25500,
        )
        .unwrap();
        assert!(text.contains("Setup code:  K7Q4-MX29-P3"), "{text}");
        assert!(text.contains("Valid for:   30 minutes (until "), "{text}");
        assert!(text.contains("pick this machine or type 192.168.1.67"));
        assert!(text.contains("Need a new code? Run: sudo vesta setup-code"));
        assert!(!text.contains('{'));
    }

    fn format_rfc3339(time: SystemTime) -> String {
        let secs = time.duration_since(UNIX_EPOCH).unwrap().as_secs();
        let days = (secs / 86400) as i64;
        let day_secs = (secs % 86400) as u32;
        let (y, m, d) = civil_from_days(days);
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            day_secs / 3600,
            (day_secs % 3600) / 60,
            day_secs % 60
        )
    }

    fn civil_from_days(days: i64) -> (i64, u32, u32) {
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let doe = z.rem_euclid(146097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
        let y = if m <= 2 { y + 1 } else { y };
        (y, m, d)
    }

    #[test]
    fn setup_code_json_is_byte_identical_to_compact_value() {
        let data = serde_json::json!({
            "code": "K7Q4-MX29-P3",
            "expiresAt": "2025-11-16T13:02:00Z"
        });
        let json =
            format_reply(Format::Json, "setup-code", &data, now_fixed(), &[], 25500).unwrap();
        assert_eq!(json, serde_json::to_string(&data).unwrap());
        assert_eq!(
            json,
            serde_json::to_string(&serde_json::json!({
                "code":"K7Q4-MX29-P3",
                "expiresAt":"2025-11-16T13:02:00Z"
            }))
            .unwrap()
        );
    }

    #[test]
    fn host_status_and_link_json_stay_compact() {
        let status =
            serde_json::json!({"hostId":"11111111-1111-4111-8111-111111111111","claimed":false});
        assert_eq!(
            format_reply(
                Format::Json,
                "host-status",
                &status,
                now_fixed(),
                &[],
                25500
            )
            .unwrap(),
            serde_json::to_string(&status).unwrap()
        );
        let human = format_reply(
            Format::Human,
            "host-status",
            &status,
            now_fixed(),
            &[],
            25500,
        )
        .unwrap();
        assert!(human.contains("Claimed:    No"));
        assert!(human.contains("sudo vesta setup-code"));

        let now = now_fixed();
        let link = serde_json::json!({
            "linkId":"22222222-2222-4222-8222-222222222222",
            "code":"ABCD-EFGH-IJKL",
            "expiresAt": format_rfc3339(now + Duration::from_secs(10 * 60))
        });
        assert_eq!(
            format_reply(Format::Json, "link", &link, now, &[], 25500).unwrap(),
            serde_json::to_string(&link).unwrap()
        );
        let human = format_reply(Format::Human, "link", &link, now, &[], 25500).unwrap();
        assert!(human.contains("Link code:   ABCD-EFGH-IJKL"), "{human}");
        assert!(human.contains("Valid for:   10 minutes"), "{human}");
    }

    #[test]
    fn claimed_status_points_to_link() {
        let status =
            serde_json::json!({"hostId":"11111111-1111-4111-8111-111111111111","claimed":true});
        let text = format_reply(
            Format::Human,
            "host-status",
            &status,
            now_fixed(),
            &[],
            25500,
        )
        .unwrap();
        assert!(text.contains("Claimed:    Yes"));
        assert!(text.contains("sudo vesta link"));
    }

    #[test]
    fn errors_are_one_clear_sentence() {
        let text = explain_error("already_claimed");
        assert!(text.contains("already claimed"));
        assert!(text.contains("sudo vesta link"));
        assert!(!text.contains("host_local_"));
        assert!(explain_error("host_local_unavailable").contains("systemctl start vesta"));
    }
}
