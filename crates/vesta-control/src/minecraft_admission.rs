//! Strict bounded `server.properties` and `banned-players.json` parsing. These documents
//! describe what the Minecraft server would admit; they grant no authority. An unreadable
//! or ambiguous document refuses instead of guessing, so callers fail closed.
use crate::identity::minecraft::MinecraftUuid;
use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::path::Path;

pub const MAX_SERVER_PROPERTIES_BYTES: usize = 65_536;
pub const MAX_BAN_LIST_BYTES: usize = 1_048_576;
pub const MAX_BAN_ENTRIES: usize = 10_000;

const OPTIONAL_BAN_STRING_FIELDS: [&str; 5] = ["name", "created", "source", "expires", "reason"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionDataError {
    TooLarge,
    Malformed,
    Unavailable,
}

impl AdmissionDataError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "admission_data_too_large",
            Self::Malformed => "admission_data_malformed",
            Self::Unavailable => "admission_data_unavailable",
        }
    }
}

impl fmt::Display for AdmissionDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl Error for AdmissionDataError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerProperties {
    white_list: bool,
    enforce_whitelist: bool,
    online_mode: bool,
}

impl ServerProperties {
    /// A missing file is Unavailable: without it online mode is unknown, so callers fail closed.
    pub fn load(path: &Path) -> Result<Self, AdmissionDataError> {
        match crate::files::read_local_document(path, MAX_SERVER_PROPERTIES_BYTES) {
            Ok(bytes) => Self::parse(&bytes),
            Err(crate::files::FileError::TooLarge) => Err(AdmissionDataError::TooLarge),
            Err(_) => Err(AdmissionDataError::Unavailable),
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, AdmissionDataError> {
        let text = utf8_document(bytes, MAX_SERVER_PROPERTIES_BYTES)?;

        let mut white_list = None;
        let mut enforce_whitelist = None;
        let mut online_mode = None;

        let mut lines = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line));

        while let Some(first) = lines.next() {
            if is_ignored(first) {
                continue;
            }

            let (logical, continued) = logical_line(first.trim_ascii_start(), &mut lines);
            let (key, value) = split_pair(&logical);
            let key = unescape(key)?;
            let value = unescape(value)?;

            let slot = match key.as_str() {
                "white-list" => &mut white_list,
                "enforce-whitelist" => &mut enforce_whitelist,
                "online-mode" => &mut online_mode,
                _ => continue,
            };

            if continued || slot.is_some() {
                return Err(AdmissionDataError::Malformed);
            }
            *slot = Some(parse_flag(&value)?);
        }

        Ok(Self {
            white_list: white_list.unwrap_or(false),
            enforce_whitelist: enforce_whitelist.unwrap_or(false),
            online_mode: online_mode.unwrap_or(true),
        })
    }

    pub const fn whitelist_enabled(&self) -> bool {
        self.white_list
    }

    pub const fn whitelist_enforced(&self) -> bool {
        self.enforce_whitelist
    }

    pub const fn online_mode(&self) -> bool {
        self.online_mode
    }
}

#[derive(Clone, Default, Eq, PartialEq)]
pub struct BanList {
    entries: HashSet<MinecraftUuid>,
}

impl BanList {
    /// Missing file means no bans, as in Minecraft; anything else unreadable refuses.
    pub fn load(path: &Path) -> Result<Self, AdmissionDataError> {
        match crate::files::read_local_document(path, MAX_BAN_LIST_BYTES) {
            Ok(bytes) => Self::parse(&bytes),
            Err(crate::files::FileError::NotFound) => Ok(Self::default()),
            Err(crate::files::FileError::TooLarge) => Err(AdmissionDataError::TooLarge),
            Err(_) => Err(AdmissionDataError::Unavailable),
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, AdmissionDataError> {
        let text = utf8_document(bytes, MAX_BAN_LIST_BYTES)?;

        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|_| AdmissionDataError::Malformed)?;
        let arr = value.as_array().ok_or(AdmissionDataError::Malformed)?;

        if arr.len() > MAX_BAN_ENTRIES {
            return Err(AdmissionDataError::TooLarge);
        }

        let mut entries = HashSet::with_capacity(arr.len());

        for element in arr {
            let obj = element.as_object().ok_or(AdmissionDataError::Malformed)?;

            let uuid_str = obj
                .get("uuid")
                .and_then(serde_json::Value::as_str)
                .ok_or(AdmissionDataError::Malformed)?;
            let uuid = MinecraftUuid::parse(uuid_str).map_err(|_| AdmissionDataError::Malformed)?;

            for field in OPTIONAL_BAN_STRING_FIELDS {
                if let Some(value) = obj.get(field)
                    && !value.is_string()
                {
                    return Err(AdmissionDataError::Malformed);
                }
            }

            // `expires` is deliberately not evaluated: every listed entry stays banned, which
            // fails closed instead of trusting a clock or a stale expiry string.
            entries.insert(uuid);
        }

        Ok(Self { entries })
    }

    pub fn contains(&self, uuid: MinecraftUuid) -> bool {
        self.entries.contains(&uuid)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl fmt::Debug for BanList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BanList")
            .field("entries", &self.entries.len())
            .finish()
    }
}

fn utf8_document(bytes: &[u8], limit: usize) -> Result<&str, AdmissionDataError> {
    if bytes.len() > limit {
        return Err(AdmissionDataError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| AdmissionDataError::Malformed)?;
    if text.starts_with('\u{FEFF}') {
        return Err(AdmissionDataError::Malformed);
    }
    Ok(text)
}

pub(crate) fn is_ignored(line: &str) -> bool {
    let line = line.trim_ascii_start();
    line.is_empty() || line.starts_with(['#', '!'])
}

/// Joins backslash continuations as `java.util.Properties` does. The flag reports whether
/// any were joined, so a continued line can never silently set one of the keys we read.
pub(crate) fn logical_line<'a>(
    first: &'a str,
    rest: &mut impl Iterator<Item = &'a str>,
) -> (String, bool) {
    let mut logical = String::new();
    let mut current = first;
    let mut continued = false;

    while ends_with_continuation(current) {
        continued = true;
        logical.push_str(&current[..current.len() - 1]);
        let Some(next) = rest.next() else {
            return (logical, continued);
        };
        current = next.trim_ascii_start();
    }

    logical.push_str(current);
    (logical, continued)
}

fn ends_with_continuation(line: &str) -> bool {
    let trailing = line.len() - line.trim_end_matches('\\').len();
    trailing % 2 == 1
}

/// The key ends at the first unescaped separator (`=`, `:` or whitespace), as in Java.
pub(crate) fn split_pair(line: &str) -> (&str, &str) {
    let mut escaped = false;
    let key_end = line
        .char_indices()
        .find_map(|(index, c)| {
            if std::mem::take(&mut escaped) {
                None
            } else if c == '\\' {
                escaped = true;
                None
            } else if matches!(c, '=' | ':') || c.is_ascii_whitespace() {
                Some(index)
            } else {
                None
            }
        })
        .unwrap_or(line.len());

    let (key, rest) = line.split_at(key_end);
    let rest = rest.trim_ascii_start();
    let rest = rest.strip_prefix(['=', ':']).unwrap_or(rest).trim_ascii();
    (key, rest)
}

pub(crate) fn unescape(raw: &str) -> Result<String, AdmissionDataError> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();

    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next().ok_or(AdmissionDataError::Malformed)? {
            't' => out.push('\t'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            'f' => out.push('\u{c}'),
            'u' => {
                let hex: String = chars.by_ref().take(4).collect();
                if hex.len() != 4 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(AdmissionDataError::Malformed);
                }
                let mut code =
                    u32::from_str_radix(&hex, 16).map_err(|_| AdmissionDataError::Malformed)?;
                if (0xd800..=0xdbff).contains(&code) {
                    if chars.next() != Some('\\') || chars.next() != Some('u') {
                        return Err(AdmissionDataError::Malformed);
                    }
                    let low: String = chars.by_ref().take(4).collect();
                    if low.len() != 4 || !low.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(AdmissionDataError::Malformed);
                    }
                    let low =
                        u32::from_str_radix(&low, 16).map_err(|_| AdmissionDataError::Malformed)?;
                    if !(0xdc00..=0xdfff).contains(&low) {
                        return Err(AdmissionDataError::Malformed);
                    }
                    code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                }
                out.push(char::from_u32(code).ok_or(AdmissionDataError::Malformed)?);
            }
            other => out.push(other),
        }
    }

    Ok(out)
}

fn parse_flag(value: &str) -> Result<bool, AdmissionDataError> {
    if value.eq_ignore_ascii_case("true") {
        Ok(true)
    } else if value.eq_ignore_ascii_case("false") {
        Ok(false)
    } else {
        Err(AdmissionDataError::Malformed)
    }
}

#[cfg(test)]
#[path = "minecraft_admission_tests.rs"]
mod tests;
