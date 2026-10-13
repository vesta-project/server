//! Public-only, bounded status text. Templates cannot select paths, secrets,
//! commands, arbitrary JSON components, or recursive substitutions.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
};

pub const MAX_TEMPLATE_BYTES: usize = 1024;
pub const MAX_RENDERED_UNITS: usize = 512;
pub const PLACEHOLDERS: [&str; 13] = [
    "server_name",
    "state",
    "online_players",
    "max_players",
    "minecraft_version",
    "loader_version",
    "pack_name",
    "pack_version",
    "maintenance_reason",
    "queued_operation",
    "elapsed_seconds",
    "estimated_ready_seconds",
    "public_failure",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Online,
    Sleeping,
    Waking,
    Starting,
    Maintenance,
    Stopping,
    CrashLoop,
    RecoveryRequired,
    Offline,
}
impl State {
    pub const ALL: [Self; 9] = [
        Self::Online,
        Self::Sleeping,
        Self::Waking,
        Self::Starting,
        Self::Maintenance,
        Self::Stopping,
        Self::CrashLoop,
        Self::RecoveryRequired,
        Self::Offline,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Sleeping => "sleeping",
            Self::Waking => "waking",
            Self::Starting => "starting",
            Self::Maintenance => "maintenance",
            Self::Stopping => "stopping",
            Self::CrashLoop => "crash_loop",
            Self::RecoveryRequired => "recovery_required",
            Self::Offline => "offline",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Invalid;
impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("motd_invalid")
    }
}
impl std::error::Error for Invalid {}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presentation {
    pub template: String,
    pub version_name: String,
    pub maximum_players: u16,
    /// Base64 PNG bytes, never an arbitrary URL or on-disk file path.
    pub icon_png: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    #[serde(deserialize_with = "unique_states")]
    pub states: BTreeMap<State, Presentation>,
    pub normal_icon_png: Option<String>,
    pub login_message: String,
}
fn unique_states<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<State, Presentation>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = BTreeMap<State, Presentation>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("unique lifecycle states")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry()? {
                if values.len() >= 9 || values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("invalid lifecycle states"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Visitor)
}
impl Default for Document {
    fn default() -> Self {
        Self {
            states: State::ALL
                .into_iter()
                .map(|state| {
                    (
                        state,
                        Presentation {
                            template: format!(
                                "{{server_name}}\n{}",
                                match state {
                                    State::Online => "Welcome",
                                    State::Sleeping => "Sleeping - join to start",
                                    State::Waking | State::Starting =>
                                        "Starting - reconnect shortly",
                                    State::Maintenance => "Maintenance: {maintenance_reason}",
                                    State::Stopping => "Stopping",
                                    State::CrashLoop => "Unavailable - operator action required",
                                    State::RecoveryRequired =>
                                        "Recovery required - operator action required",
                                    State::Offline => "Offline",
                                }
                            ),
                            version_name: "Vesta".into(),
                            maximum_players: 0,
                            icon_png: None,
                        },
                    )
                })
                .collect(),
            normal_icon_png: None,
            login_message: "Server start requested; reconnect shortly.".into(),
        }
    }
}
impl Document {
    pub fn valid(&self) -> bool {
        self.states.len() == State::ALL.len()
            && State::ALL.iter().all(|state| {
                self.states.get(state).is_some_and(|p| {
                    validate_template(&p.template).is_ok()
                        && public_label(&p.version_name, 128)
                        && p.maximum_players <= 1000
                        && p.icon_png.as_ref().is_none_or(|v| v.len() <= 90_000)
                })
            })
            && self
                .normal_icon_png
                .as_ref()
                .is_none_or(|v| v.len() <= 90_000)
            && validate_template(&self.login_message).is_ok()
    }
    pub fn render(&self, state: State, inputs: &PublicInputs<'_>) -> Result<Rendered, Invalid> {
        if !self.valid() {
            return Err(Invalid);
        }
        let p = &self.states[&state];
        let text = expand(&p.template, state, inputs, p.maximum_players)?;
        let description = component(&text)?;
        if serde_json::to_vec(&description).map_err(|_| Invalid)?.len() > 4096 {
            return Err(Invalid);
        }
        let plain_text = plain(&text)?;
        let selected = p.icon_png.as_ref().or(self.normal_icon_png.as_ref());
        let icon = selected
            .and_then(|s| STANDARD.decode(s).ok())
            .and_then(|b| normalize_icon(&b))
            .filter(|b| b.len() <= 4096)
            .unwrap_or_else(fallback_icon);
        Ok(Rendered {
            state,
            description,
            plain_lines: plain_text.split('\n').map(str::to_owned).collect(),
            version_name: p.version_name.clone(),
            maximum_players: p.maximum_players,
            favicon_data_uri: format!("data:image/png;base64,{}", STANDARD.encode(icon)),
        })
    }
    pub fn disconnect(&self, state: State, inputs: &PublicInputs<'_>) -> Result<Value, Invalid> {
        if !self.valid() {
            return Err(Invalid);
        }
        component(&expand(&self.login_message, state, inputs, 0)?)
    }
}
#[derive(Default)]
pub struct PublicInputs<'a> {
    pub server_name: &'a str,
    pub online_players: Option<u16>,
    pub minecraft_version: &'a str,
    pub loader_version: &'a str,
    pub pack_name: &'a str,
    pub pack_version: &'a str,
    pub maintenance_reason: &'a str,
    pub queued_operation: Option<PublicOperation>,
    pub elapsed_seconds: u64,
    pub estimated_ready_seconds: Option<u64>,
    pub public_failure: Option<PublicFailure>,
}
#[derive(Clone, Copy)]
pub enum PublicOperation {
    Start,
    Stop,
    Restart,
}
#[derive(Clone, Copy)]
pub enum PublicFailure {
    CrashLoop,
    RecoveryRequired,
    PortUnavailable,
    StartupFailed,
}
#[derive(Serialize)]
pub struct Rendered {
    pub state: State,
    pub description: Value,
    pub plain_lines: Vec<String>,
    pub version_name: String,
    pub maximum_players: u16,
    pub favicon_data_uri: String,
}
fn public_label(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(|c| c.is_control() || c == '\u{a7}')
}
fn insertion(text: &str) -> Result<&str, Invalid> {
    if text.len() > 256 || text.chars().any(|c| c.is_control() || c == '\u{a7}') {
        Err(Invalid)
    } else {
        Ok(text)
    }
}
pub fn validate_template(text: &str) -> Result<(), Invalid> {
    if text.len() > MAX_TEMPLATE_BYTES
        || text.encode_utf16().count() > MAX_RENDERED_UNITS
        || text.matches('\n').count() > 1
        || text.chars().any(|c| c.is_control() && c != '\n')
    {
        return Err(Invalid);
    }
    let mut rest = text;
    while let Some(start) = rest.find(['{', '}']) {
        if rest.as_bytes()[start] == b'}' {
            return Err(Invalid);
        }
        let tail = &rest[start + 1..];
        let end = tail.find('}').ok_or(Invalid)?;
        if !PLACEHOLDERS.contains(&&tail[..end]) {
            return Err(Invalid);
        }
        rest = &tail[end + 1..];
    }
    component(text).map(|_| ())
}
fn expand(text: &str, state: State, i: &PublicInputs<'_>, max: u16) -> Result<String, Invalid> {
    validate_template(text)?;
    let mut rest = text;
    let mut result = String::new();
    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let end = tail.find('}').ok_or(Invalid)?;
        let value = match &tail[..end] {
            "server_name" => insertion(i.server_name)?.to_owned(),
            "state" => state.name().into(),
            "online_players" => {
                if state == State::Online {
                    i.online_players.map_or("unknown".into(), |n| n.to_string())
                } else {
                    "0".into()
                }
            }
            "max_players" => max.to_string(),
            "minecraft_version" => insertion(i.minecraft_version)?.to_owned(),
            "loader_version" => insertion(i.loader_version)?.to_owned(),
            "pack_name" => insertion(i.pack_name)?.to_owned(),
            "pack_version" => insertion(i.pack_version)?.to_owned(),
            "maintenance_reason" => insertion(i.maintenance_reason)?.to_owned(),
            "queued_operation" => match i.queued_operation {
                Some(PublicOperation::Start) => "start",
                Some(PublicOperation::Stop) => "stop",
                Some(PublicOperation::Restart) => "restart",
                None => "none",
            }
            .into(),
            "elapsed_seconds" => i.elapsed_seconds.to_string(),
            "estimated_ready_seconds" => i
                .estimated_ready_seconds
                .map_or("unknown".into(), |n| n.to_string()),
            "public_failure" => match i.public_failure {
                Some(PublicFailure::CrashLoop) => "crash_loop",
                Some(PublicFailure::RecoveryRequired) => "recovery_required",
                Some(PublicFailure::PortUnavailable) => "port_unavailable",
                Some(PublicFailure::StartupFailed) => "startup_failed",
                None => "none",
            }
            .into(),
            _ => return Err(Invalid),
        };
        result.push_str(&value);
        rest = &tail[end + 1..];
    }
    result.push_str(rest);
    if result.encode_utf16().count() > MAX_RENDERED_UNITS {
        return Err(Invalid);
    }
    Ok(result)
}
fn color(c: char) -> Option<&'static str> {
    Some(match c.to_ascii_lowercase() {
        '0' => "black",
        '1' => "dark_blue",
        '2' => "dark_green",
        '3' => "dark_aqua",
        '4' => "dark_red",
        '5' => "dark_purple",
        '6' => "gold",
        '7' => "gray",
        '8' => "dark_gray",
        '9' => "blue",
        'a' => "green",
        'b' => "aqua",
        'c' => "red",
        'd' => "light_purple",
        'e' => "yellow",
        'f' => "white",
        _ => return None,
    })
}
/// Structured text only. Never creates click/hover actions or parses inserted JSON.
pub fn component(text: &str) -> Result<Value, Invalid> {
    let mut spans = Vec::new();
    let mut buffer = String::new();
    let mut style = serde_json::Map::new();
    let flush =
        |buffer: &mut String, style: &serde_json::Map<String, Value>, spans: &mut Vec<Value>| {
            if !buffer.is_empty() {
                let mut span = style.clone();
                span.insert("text".into(), json!(std::mem::take(buffer)));
                spans.push(Value::Object(span));
            }
        };
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{a7}' {
            buffer.push(c);
            continue;
        }
        flush(&mut buffer, &style, &mut spans);
        let code = chars.next().ok_or(Invalid)?.to_ascii_lowercase();
        if let Some(color) = color(code) {
            style.clear();
            style.insert("color".into(), json!(color));
        } else if code == 'r' {
            style.clear();
        } else {
            let name = match code {
                'k' => "obfuscated",
                'l' => "bold",
                'm' => "strikethrough",
                'n' => "underlined",
                'o' => "italic",
                _ => return Err(Invalid),
            };
            style.insert(name.into(), json!(true));
        }
    }
    flush(&mut buffer, &style, &mut spans);
    Ok(json!({"text":"","extra":spans}))
}
fn plain(text: &str) -> Result<String, Invalid> {
    let mut chars = text.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == '\u{a7}' {
            chars.next().ok_or(Invalid)?;
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

fn crc(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for b in bytes {
        c ^= *b as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ if c & 1 != 0 { 0xedb88320 } else { 0 };
        }
    }
    !c
}
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut checked = kind.to_vec();
    checked.extend_from_slice(data);
    out.extend_from_slice(&crc(&checked).to_be_bytes());
}
fn encode_icon(header: &[u8], raw: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(raw).expect("bounded in-memory PNG");
    let data = encoder.finish().expect("bounded in-memory PNG");
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", header);
    chunk(&mut out, b"IDAT", &data);
    chunk(&mut out, b"IEND", &[]);
    out
}
pub fn fallback_icon() -> Vec<u8> {
    let mut raw = Vec::with_capacity(16448);
    for y in 0i32..64 {
        raw.push(0);
        for x in 0i32..64 {
            let mark = (14..50).contains(&y)
                && ((x - (16 + y / 2)).abs() < 4 || (x - (48 - y / 2)).abs() < 4);
            raw.extend_from_slice(if mark {
                &[245, 241, 255, 255]
            } else {
                &[100, 70, 180, 255]
            });
        }
    }
    encode_icon(&[0, 0, 0, 64, 0, 0, 0, 64, 8, 6, 0, 0, 0], &raw)
}
/// Re-encode validated pixels to strip private text/metadata from selected icons.
pub fn normalize_icon(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() > 65536 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut at = 8;
    let mut header = None;
    let mut compressed = Vec::new();
    let mut ended = false;
    let mut data_ended = false;
    while at < bytes.len() {
        let len = u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
        let end = at.checked_add(12)?.checked_add(len)?;
        let kind = bytes.get(at + 4..at + 8)?;
        let payload = bytes.get(at + 8..end - 4)?;
        if crc(bytes.get(at + 4..end - 4)?)
            != u32::from_be_bytes(bytes.get(end - 4..end)?.try_into().ok()?)
        {
            return None;
        }
        match kind {
            b"IHDR" if at == 8 && len == 13 => {
                if payload[..8] != [0, 0, 0, 64, 0, 0, 0, 64]
                    || payload[8] != 8
                    || !matches!(payload[9], 2 | 6)
                    || payload[10..] != [0, 0, 0]
                {
                    return None;
                }
                header = Some(payload);
            }
            b"IDAT" if header.is_some() && !data_ended => compressed.extend_from_slice(payload),
            b"IEND" if len == 0 && !compressed.is_empty() && end == bytes.len() => {
                ended = true;
            }
            _ if kind[0] & 32 != 0 && header.is_some() => {
                if !compressed.is_empty() {
                    data_ended = true;
                }
            }
            _ => return None,
        }
        at = end;
    }
    if !ended {
        return None;
    }
    let header = header?;
    let row = 1 + 64 * if header[9] == 6 { 4 } else { 3 };
    let expected = 64 * row;
    let mut decoder = flate2::read::ZlibDecoder::new(compressed.as_slice());
    let mut raw = Vec::new();
    (&mut decoder)
        .take(expected as u64 + 1)
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() != expected
        || decoder.total_in() != compressed.len() as u64
        || raw.chunks_exact(row).any(|r| r[0] > 4)
    {
        return None;
    }
    Some(encode_icon(header, &raw))
}

#[cfg(test)]
mod tests;
