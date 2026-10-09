//! Strict bounded `whitelist.json` parsing. Membership is a pack-access policy input only,
//! never administration; an invalid document refuses whole instead of shrinking silently.
use crate::identity::minecraft::MinecraftUuid;
use std::collections::HashSet;
use std::error::Error;
use std::fmt;

pub const MAX_WHITELIST_BYTES: usize = 1024 * 1024;
pub const MAX_WHITELIST_ENTRIES: usize = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WhitelistError {
    TooLarge,
    Malformed,
}

impl WhitelistError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "whitelist_too_large",
            Self::Malformed => "whitelist_malformed",
        }
    }
}

impl fmt::Display for WhitelistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl Error for WhitelistError {}

#[derive(Clone, Default, Eq, PartialEq)]
pub struct Whitelist {
    entries: HashSet<MinecraftUuid>,
}

impl Whitelist {
    pub fn parse(bytes: &[u8]) -> Result<Self, WhitelistError> {
        if bytes.len() > MAX_WHITELIST_BYTES {
            return Err(WhitelistError::TooLarge);
        }

        let s = std::str::from_utf8(bytes).map_err(|_| WhitelistError::Malformed)?;

        if s.starts_with('\u{FEFF}') {
            return Err(WhitelistError::Malformed);
        }

        let value: serde_json::Value =
            serde_json::from_str(s).map_err(|_| WhitelistError::Malformed)?;

        let arr = value.as_array().ok_or(WhitelistError::Malformed)?;

        if arr.len() > MAX_WHITELIST_ENTRIES {
            return Err(WhitelistError::TooLarge);
        }

        let mut entries = HashSet::new();

        for element in arr {
            let obj = element.as_object().ok_or(WhitelistError::Malformed)?;

            let uuid_str = obj
                .get("uuid")
                .and_then(|v| v.as_str())
                .ok_or(WhitelistError::Malformed)?;

            let uuid = MinecraftUuid::parse(uuid_str).map_err(|_| WhitelistError::Malformed)?;

            if let Some(name_val) = obj.get("name")
                && !name_val.is_string()
            {
                return Err(WhitelistError::Malformed);
            }

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

impl fmt::Debug for Whitelist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!(
            "Whitelist {{ entries: {} }}",
            self.entries.len()
        ))
    }
}

#[cfg(test)]
#[path = "whitelist_tests.rs"]
mod tests;
