//! Portable lexical file names. This is not native filesystem confinement.
use super::{FileError, Result};
use crate::identity::Resource;
use std::fmt;

pub const MAX_PATH_BYTES: usize = 4_096;
pub const MAX_COMPONENT_BYTES: usize = 255;

pub(super) const PROTECTED_NAMES: &[&str] = &[
    ".vesta",
    ".git",
    ".hg",
    ".svn",
    "wrapper-config.toml",
    "vesta-wrapper",
    "start-vesta-wrapper.sh",
    "start-vesta-wrapper.bat",
];

/// A nonempty canonical ASCII resource. Root is selected separately for listing.
/// Backslashes, absolute paths and ambiguous normalization are never accepted.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RelativePath(String);
impl RelativePath {
    pub fn parse(text: &str) -> Result<Self> {
        if text.is_empty() || text.len() > MAX_PATH_BYTES || !text.is_ascii() {
            return Err(FileError::InvalidPath);
        }
        Resource::parse(text).map_err(|_| FileError::InvalidPath)?;
        for segment in text.split('/') {
            let stem = segment
                .split('.')
                .next()
                .unwrap_or_default()
                .trim_end_matches(' ');
            if segment.len() > MAX_COMPONENT_BYTES
                || (stem.len() == 4
                    && (stem[..3].eq_ignore_ascii_case("com")
                        || stem[..3].eq_ignore_ascii_case("lpt"))
                    && stem.as_bytes()[3] == b'0')
            {
                return Err(FileError::InvalidPath);
            }
            if protected(segment) {
                return Err(FileError::ProtectedPath);
            }
        }
        Ok(Self(text.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub(super) fn resource(&self) -> Resource {
        // The private constructor already applies the identical resource policy.
        Resource::parse(&self.0).expect("validated file resource")
    }
}
impl fmt::Debug for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RelativePath[<redacted>]")
    }
}
pub(super) fn protected(name: &str) -> bool {
    PROTECTED_NAMES.iter().any(|p| p.eq_ignore_ascii_case(name))
}
