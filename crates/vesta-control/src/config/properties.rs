//! Structured Minecraft properties edits, never edits to agent host configuration.
use crate::{
    files::{FileBrowser, FileError, RelativePath, WritePrecondition},
    identity::{Capability, Subject},
    minecraft_admission::{self, MAX_SERVER_PROPERTIES_BYTES},
    operations::{self, ConfirmationToken, PropertiesApproval},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropertiesPatch {
    #[serde(deserialize_with = "unique_changes")]
    pub changes: BTreeMap<String, Value>,
    pub expected_sha256: String,
    #[serde(default)]
    pub confirmation_token: Option<String>,
}
fn unique_changes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Value>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = BTreeMap<String, Value>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a bounded object with unique properties")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, Value>()? {
                if values.len() >= 32 || values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("invalid changes"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Visitor)
}
pub struct ServerPropertiesEditor<'a> {
    files: &'a FileBrowser,
}
struct Document {
    values: BTreeMap<String, String>,
    blocks: Vec<(Option<String>, String)>,
}
/// Uses the same Java-properties decoding and duplicate-key checks as edits.
pub(crate) fn game_bind_address(bytes: &[u8]) -> Option<std::net::SocketAddr> {
    let document = Document::parse(bytes).ok()?;
    let port = document
        .values
        .get("server-port")?
        .trim()
        .parse::<u16>()
        .ok()?;
    if port == 0 {
        return None;
    }
    let ip = document
        .values
        .get("server-ip")
        .map_or("", String::as_str)
        .trim();
    let ip = if ip.is_empty() {
        std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
    } else {
        ip.parse().ok()?
    };
    Some(std::net::SocketAddr::new(ip, port))
}
impl Document {
    fn parse(bytes: &[u8]) -> Result<Self, FileError> {
        if bytes.len() > MAX_SERVER_PROPERTIES_BYTES {
            return Err(FileError::TooLarge);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| FileError::InvalidPath)?;
        if text.starts_with('\u{feff}') || text.contains('\0') {
            return Err(FileError::InvalidPath);
        }
        let physical: Vec<&str> = text.split_inclusive('\n').collect();
        let normalized: Vec<&str> = physical
            .iter()
            .map(|s| {
                let s = s.strip_suffix('\n').unwrap_or(s);
                s.strip_suffix('\r').unwrap_or(s)
            })
            .collect();
        let mut lines = normalized.iter().copied();
        let mut values = BTreeMap::new();
        let mut blocks = Vec::new();
        while let Some(first) = lines.next() {
            let start = normalized.len() - lines.len() - 1;
            if minecraft_admission::is_ignored(first) {
                blocks.push((None, physical[start].to_owned()));
                continue;
            }
            let (logical, _) =
                minecraft_admission::logical_line(first.trim_ascii_start(), &mut lines);
            let end = normalized.len() - lines.len();
            let (key, value) = minecraft_admission::split_pair(&logical);
            let key = minecraft_admission::unescape(key).map_err(|_| FileError::InvalidPath)?;
            let value = minecraft_admission::unescape(value).map_err(|_| FileError::InvalidPath)?;
            if key.is_empty()
                || key.len() > 128
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
                || value.len() > 4096
                || values.len() >= 256
                || values.insert(key.clone(), value).is_some()
            {
                return Err(FileError::InvalidPath);
            }
            blocks.push((Some(key), physical[start..end].concat()));
        }
        minecraft_admission::ServerProperties::parse(bytes).map_err(|_| FileError::InvalidPath)?;
        Ok(Self { values, blocks })
    }
    fn edited(&self, changes: &BTreeMap<String, String>) -> Result<Vec<u8>, FileError> {
        let mut remaining = changes.clone();
        let mut result = String::new();
        for (key, raw) in &self.blocks {
            if let Some(key) = key
                && let Some(value) = remaining.remove(key)
            {
                let ending = if raw.ends_with("\r\n") {
                    "\r\n"
                } else if raw.ends_with('\n') {
                    "\n"
                } else {
                    ""
                };
                result.push_str(&format!("{key}={}{ending}", escape(&value)));
            } else {
                result.push_str(raw);
            }
        }
        for (key, value) in remaining {
            if !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            result.push_str(&format!("{key}={}\n", escape(&value)));
        }
        let bytes = result.into_bytes();
        Self::parse(&bytes)?;
        Ok(bytes)
    }
}
/// Bounded world selection using the same Java-properties parser as editing.
pub(crate) fn world_values(bytes: &[u8]) -> Result<(String, String), FileError> {
    let document = Document::parse(bytes)?;
    Ok((
        document
            .values
            .get("level-name")
            .cloned()
            .unwrap_or_else(|| "world".into()),
        document
            .values
            .get("level-seed")
            .cloned()
            .unwrap_or_default(),
    ))
}
pub(crate) fn select_new_world(bytes: &[u8], name: &str, seed: &str) -> Result<Vec<u8>, FileError> {
    Document::parse(bytes)?.edited(&BTreeMap::from([
        ("level-name".into(), name.into()),
        ("level-seed".into(), seed.into()),
    ]))
}
fn escape(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '\n' => "\\n".to_owned(),
            '\r' => "\\r".to_owned(),
            '\t' => "\\t".to_owned(),
            '\\' => "\\\\".to_owned(),
            ' ' => "\\ ".to_owned(),
            c if !c.is_ascii() => c
                .encode_utf16(&mut [0; 2])
                .iter()
                .map(|u| format!("\\u{u:04x}"))
                .collect(),
            c => c.to_string(),
        })
        .collect()
}
#[derive(Clone, Copy)]
enum Kind {
    Boolean,
    Integer(i64, i64),
    Enum(&'static [&'static str]),
    Text(usize),
    Secret(usize),
}
fn schema(key: &str) -> Option<Kind> {
    Some(match key {
        "online-mode"
        | "white-list"
        | "enforce-whitelist"
        | "enforce-secure-profile"
        | "prevent-proxy-connections"
        | "enable-rcon"
        | "enable-query"
        | "enable-command-block"
        | "pvp"
        | "allow-flight"
        | "spawn-monsters"
        | "spawn-animals"
        | "spawn-npcs"
        | "allow-nether"
        | "force-gamemode"
        | "hardcore"
        | "hide-online-players"
        | "log-ips"
        | "enable-status" => Kind::Boolean,
        "max-players" => Kind::Integer(1, 1000),
        "view-distance" | "simulation-distance" => Kind::Integer(2, 32),
        "server-port" | "rcon.port" | "query.port" => Kind::Integer(1, 65535),
        "spawn-protection" => Kind::Integer(0, 1024),
        "player-idle-timeout" => Kind::Integer(0, 10080),
        "op-permission-level" | "function-permission-level" => Kind::Integer(1, 4),
        "difficulty" => Kind::Enum(&["peaceful", "easy", "normal", "hard"]),
        "gamemode" => Kind::Enum(&["survival", "creative", "adventure", "spectator"]),
        "motd" => Kind::Text(1024),
        "rcon.password" => Kind::Secret(128),
        _ => return None,
    })
}
/// Reading a known vanilla setting does not grant remote editing authority.
/// Unknown/modded keys remain hidden because they may contain credentials.
fn read_schema(key: &str) -> Option<Kind> {
    schema(key).or_else(|| {
        Some(match key {
            "accepts-transfers"
            | "broadcast-console-to-ops"
            | "broadcast-rcon-to-ops"
            | "enable-code-of-conduct"
            | "enable-jmx-monitoring"
            | "generate-structures"
            | "require-resource-pack"
            | "sync-chunk-writes"
            | "use-native-transport"
            | "management-server-enabled"
            | "management-server-tls-enabled" => Kind::Boolean,
            "entity-broadcast-range-percentage" => Kind::Integer(10, 1000),
            "max-chained-neighbor-updates" | "max-tick-time" => Kind::Integer(-1, i64::MAX),
            "max-world-size" => Kind::Integer(1, 29_999_984),
            "network-compression-threshold" => Kind::Integer(-1, i32::MAX as i64),
            "rate-limit"
            | "pause-when-empty-seconds"
            | "chat-spam-threshold-seconds"
            | "command-spam-threshold-seconds"
            | "status-heartbeat-interval" => Kind::Integer(0, i32::MAX as i64),
            "management-server-port" => Kind::Integer(0, 65535),
            "region-file-compression" => Kind::Enum(&["deflate", "lz4", "none"]),
            "level-name" | "level-seed" | "level-type" | "server-ip" => Kind::Text(256),
            "generator-settings"
            | "initial-disabled-packs"
            | "initial-enabled-packs"
            | "bug-report-link" => Kind::Text(4096),
            _ => return None,
        })
    })
}
fn validate(key: &str, value: &Value) -> Result<String, FileError> {
    if key == "motd" {
        let text = value.as_str().ok_or(FileError::InvalidPath)?;
        if text.len() > 1024
            || text.encode_utf16().count() > 512
            || text.matches('\n').count() > 1
            || text
                .chars()
                .any(|c| (c.is_control() && c != '\n') || matches!(c, '\\' | '/'))
        {
            return Err(FileError::InvalidPath);
        }
        crate::motd::component(text).map_err(|_| FileError::InvalidPath)?;
        return Ok(text.to_owned());
    }
    let kind = schema(key).ok_or(FileError::InvalidPath)?;
    Ok(match kind {
        Kind::Boolean => value.as_bool().ok_or(FileError::InvalidPath)?.to_string(),
        Kind::Integer(min, max) => {
            let n = value
                .as_i64()
                .filter(|n| (min..=max).contains(n))
                .ok_or(FileError::InvalidPath)?;
            n.to_string()
        }
        Kind::Enum(choices) => value
            .as_str()
            .filter(|v| choices.contains(v))
            .ok_or(FileError::InvalidPath)?
            .to_owned(),
        Kind::Text(max) | Kind::Secret(max) => {
            let value = value.as_str().ok_or(FileError::InvalidPath)?;
            if (matches!(kind, Kind::Secret(_)) && value.is_empty())
                || value.len() > max
                || !value.is_ascii()
                || value
                    .bytes()
                    .any(|b| b.is_ascii_control() || matches!(b, b'\\' | b'/'))
            {
                return Err(FileError::InvalidPath);
            }
            value.to_owned()
        }
    })
}
fn weakens(key: &str, value: &str, previous: Option<&String>) -> bool {
    if key == "spawn-protection" {
        return value.parse::<i64>().unwrap_or(0)
            < previous.and_then(|v| v.parse::<i64>().ok()).unwrap_or(16);
    }
    if matches!(key, "op-permission-level" | "function-permission-level") {
        let default = if key == "op-permission-level" { 4 } else { 2 };
        return value.parse::<i64>().unwrap_or(4)
            > previous
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(default);
    }
    let previous = previous.and_then(|s| match s.to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    });
    match key {
        "online-mode" | "enforce-secure-profile" => value == "false" && previous.unwrap_or(true),
        "white-list" | "enforce-whitelist" | "prevent-proxy-connections" => {
            value == "false" && previous.unwrap_or(false)
        }
        "enable-rcon" | "enable-query" | "enable-command-block" | "allow-flight" => {
            value == "true" && !previous.unwrap_or(false)
        }
        _ => false,
    }
}
impl<'a> ServerPropertiesEditor<'a> {
    pub fn new(files: &'a FileBrowser) -> Self {
        Self { files }
    }
    pub fn read(&self, subject: &Subject) -> Result<Value, FileError> {
        let path = RelativePath::parse("server.properties")?;
        let read = self.files.for_properties(None).read_with_capability(
            subject,
            &path,
            MAX_SERVER_PROPERTIES_BYTES,
            Some(Capability::ConfigView),
        )?;
        let parsed = Document::parse(read.bytes())?;
        let values: BTreeMap<String, Value> = parsed
            .values
            .into_iter()
            .map(|(key, value)| {
                let projection = match read_schema(&key) {
                    Some(Kind::Boolean) => match value.to_ascii_lowercase().as_str() {
                        "true" => json!(true),
                        "false" => json!(false),
                        _ => json!("[redacted]"),
                    },
                    Some(Kind::Integer(min, max)) => value
                        .parse::<i64>()
                        .ok()
                        .filter(|n| (min..=max).contains(n))
                        .map_or(json!("[redacted]"), |n| json!(n)),
                    Some(Kind::Enum(choices)) if choices.contains(&value.as_str()) => json!(value),
                    Some(Kind::Text(_))
                        if key == "motd" && validate("motd", &json!(value)).is_ok() =>
                    {
                        json!(value)
                    }
                    Some(Kind::Text(max))
                        if value.len() <= max
                            && value.is_ascii()
                            && !value
                                .bytes()
                                .any(|b| b.is_ascii_control() || matches!(b, b'\\' | b'/')) =>
                    {
                        json!(value)
                    }
                    _ => json!("[redacted]"),
                };
                (key, projection)
            })
            .collect();
        Ok(json!({"values":values,"sha256":read.sha256()}))
    }
    pub fn patch(
        &self,
        subject: &Subject,
        key: &str,
        patch: PropertiesPatch,
    ) -> Result<Value, FileError> {
        if patch.changes.is_empty()
            || patch.changes.len() > 32
            || patch.expected_sha256.len() != 64
            || !patch
                .expected_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        {
            return Err(FileError::InvalidPath);
        }
        let changes: BTreeMap<String, String> = patch
            .changes
            .iter()
            .map(|(key, value)| validate(key, value).map(|value| (key.clone(), value)))
            .collect::<Result<_, _>>()?;
        let input = json!(["properties", patch.expected_sha256, patch.changes]);
        let payload_hash = format!("{:x}", Sha256::digest(input.to_string().as_bytes()));
        self.files
            .check_global_authority(subject, Capability::ConfigEdit)?;
        operations::check_properties_intent(self.files.database(), subject, &payload_hash, key)?;
        let approval = patch
            .confirmation_token
            .map(|token| {
                let token = ConfirmationToken::try_from(token)
                    .map_err(|_| FileError::ConfirmationInvalid)?;
                Ok::<_, FileError>(PropertiesApproval {
                    token_hash: token.hash().to_string(),
                    payload_hash: payload_hash.clone(),
                    expected_sha256: patch.expected_sha256.clone(),
                })
            })
            .transpose()?;
        let files = self.files.for_properties(approval.clone());
        files.execute_remote(subject,key,input,|| Ok(vec![
            (Capability::ConfigEdit.id().into(),String::new()),
            (Capability::FilesRead.id().into(),"server.properties".into()),
            (Capability::FilesOverwrite.id().into(),"server.properties".into()),
        ]),|files| {
            let path = RelativePath::parse("server.properties")?;
            let read = files.read(subject,&path,MAX_SERVER_PROPERTIES_BYTES)?;
            if read.sha256()!=patch.expected_sha256 { return Err(FileError::Conflict(Some(read.sha256().into()))); }
            let parsed = Document::parse(read.bytes())?;
            let weakening = changes.iter().any(|(key,value)| weakens(key,value,parsed.values.get(key)));
            if weakening && approval.is_none() { return Err(FileError::ConfirmationRequired); }
            let bytes = parsed.edited(&changes)?;
            let change = files.write(subject,&path,&WritePrecondition::Matches(patch.expected_sha256.clone()),&bytes)?;
            Ok(json!({"outcome":"succeeded","sha256":change.sha256,"size":change.size.to_string(),"trash_id":change.trash,"restart_needed":true}))
        }).or_else(|error| {
            if error==FileError::ConfirmationRequired {
                let receipt = operations::prepare_properties_confirmation(self.files.database(),subject,&payload_hash,&patch.expected_sha256,key)?;
                Ok(json!({"operation_id":receipt.operation_id.as_str(),"state":"awaiting_confirmation","confirmation_token":receipt.token.expose_secret(),"expires_at_ms":receipt.expires_at_ms}))
            } else {
                if error == FileError::ConfirmationInvalid && let Some(approval) = &approval {
                    operations::refuse_properties_confirmation(self.files.database(),subject,approval)?;
                }
                Err(error)
            }
        })
    }
}
/// Raw edits retain unknown keys, but validate syntax and every known typed key.
pub(crate) fn validate_raw_properties(bytes: &[u8]) -> Result<(), FileError> {
    let document = Document::parse(bytes)?;
    for (key, value) in document.values {
        if let Some(kind) = schema(&key) {
            let typed = match kind {
                Kind::Boolean => Value::Bool(value.parse().map_err(|_| FileError::InvalidPath)?),
                Kind::Integer(_, _) => {
                    json!(value.parse::<i64>().map_err(|_| FileError::InvalidPath)?)
                }
                _ => Value::String(value),
            };
            validate(&key, &typed)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn game_endpoint_uses_canonical_java_keys_without_widening_escaped_bind_addresses() {
        assert_eq!(
            crate::operations::readiness::probe_from_properties(
                "server\\u002dip=127.0.0.1\nserver\\u002dport=25570\n"
            )
            .unwrap(),
            "127.0.0.1:25570".parse().unwrap()
        );
        assert_eq!(
            crate::operations::readiness::probe_from_properties(
                "server-ip=0.0.0.0\nserver-port=25565\n"
            )
            .unwrap(),
            "127.0.0.1:25565".parse().unwrap()
        );
        assert_eq!(
            game_bind_address(b"server\\u002dip=127.0.0.1\nserver\\u002dport=25570\n").unwrap(),
            "127.0.0.1:25570".parse().unwrap()
        );
        assert!(game_bind_address(b"server-ip=127.0.0.1\nserver\\u002dip=0.0.0.0\n").is_none());
        assert!(game_bind_address(b"server-port=0\n").is_none());
    }
    #[test]
    fn live_motd_round_trips_styles_unicode_and_two_lines_without_property_injection() {
        let text = "§aHello 😀\n§lWelcome";
        assert_eq!(validate("motd", &json!(text)).unwrap(), text);
        let doc = Document::parse(b"# retained\r\nmotd=old\r\nunknown=keep\r\n").unwrap();
        let output = doc
            .edited(&BTreeMap::from([("motd".into(), text.into())]))
            .unwrap();
        assert_eq!(Document::parse(&output).unwrap().values["motd"], text);
        assert!(String::from_utf8(output).unwrap().contains("\\n"));
        for text in ["a\nb\nc", "x\ronline-mode=false", "§z", "§"] {
            assert!(validate("motd", &json!(text)).is_err());
        }
        assert!(crate::minecraft_admission::unescape("\\ud800").is_err());
        assert!(crate::minecraft_admission::unescape("\\ud800\\u0041").is_err());
        assert!(crate::minecraft_admission::unescape("\\udc00").is_err());
    }
    #[test]
    fn schema_rejects_unknown_wrong_types_and_paths() {
        assert!(
            serde_json::from_str::<PropertiesPatch>(
                r#"{"changes":{"online-mode":true,"online-mode":false},"expected_sha256":"hash"}"#
            )
            .is_err()
        );
        assert!(validate("online-mode", &json!("false")).is_err());
        assert!(validate("unknown", &json!(true)).is_err());
        assert!(validate("view-distance", &json!(33)).is_err());
        assert!(validate("motd", &json!("C:\\secret")).is_err());
        assert_eq!(validate("difficulty", &json!("hard")).unwrap(), "hard");
    }
    #[test]
    fn read_only_vanilla_settings_do_not_become_editable_or_expose_secrets() {
        for key in [
            "accepts-transfers",
            "broadcast-console-to-ops",
            "generate-structures",
            "level-name",
            "generator-settings",
            "initial-enabled-packs",
            "pause-when-empty-seconds",
            "status-heartbeat-interval",
        ] {
            assert!(read_schema(key).is_some(), "readable: {key}");
            assert!(schema(key).is_none(), "still read-only: {key}");
            assert!(validate(key, &json!(true)).is_err());
        }
        for key in [
            "management-server-secret",
            "management-server-tls-keystore-password",
            "management-server-tls-keystore",
            "resource-pack",
            "text-filtering-config",
            "unknown-plugin-secret",
        ] {
            assert!(read_schema(key).is_none(), "hidden: {key}");
        }
        assert!(matches!(
            read_schema("rcon.password"),
            Some(Kind::Secret(_))
        ));
    }
    #[test]
    fn edits_preserve_unknown_comments_crlf_and_decode_aliases() {
        let input =
            b"# kept\r\nmotd=old\r\nunknown=keep\\\r\n  continued\r\nonline\\u002dmode=true\r\n";
        let doc = Document::parse(input).unwrap();
        let output = doc
            .edited(&BTreeMap::from([
                ("motd".into(), "new title".into()),
                ("online-mode".into(), "true".into()),
            ]))
            .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "# kept\r\nmotd=new\\ title\r\nunknown=keep\\\r\n  continued\r\nonline-mode=true\r\n"
        );
        assert!(Document::parse(b"online-mode=true\nonline\\u002dmode=false\n").is_err());
    }
    #[test]
    fn security_transitions_require_confirmation() {
        assert!(weakens("online-mode", "false", None));
        for key in ["enable-rcon", "enable-query", "enable-command-block"] {
            assert!(weakens(key, "true", None));
        }
        assert!(!weakens("online-mode", "true", None));
        assert!(!weakens("enable-rcon", "false", None));
        assert!(weakens("function-permission-level", "4", None));
        for (old, new, weak) in [
            (None, "15", true),
            (None, "16", false),
            (None, "17", false),
            (Some("32"), "16", true),
            (Some("0"), "0", false),
            (Some("16"), "0", true),
        ] {
            let old = old.map(str::to_owned);
            assert_eq!(weakens("spawn-protection", new, old.as_ref()), weak);
        }
        assert!(weakens("allow-flight", "true", None));
        assert!(!weakens("allow-flight", "false", None));
        for key in [
            "white-list",
            "enforce-whitelist",
            "enforce-secure-profile",
            "prevent-proxy-connections",
        ] {
            assert!(weakens(key, "false", Some(&"true".into())));
            assert!(!weakens(key, "true", Some(&"false".into())));
        }
        assert!(weakens("online-mode", "false", Some(&"malformed".into())));
        assert!(weakens("op-permission-level", "4", Some(&"3".into())));
        assert!(!weakens("op-permission-level", "3", Some(&"4".into())));
    }
}
