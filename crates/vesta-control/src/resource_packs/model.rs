use serde::{Deserialize, Serialize};
use std::{fmt, fs::File, time::Duration};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidInput,
    InvalidArchive,
    InvalidMetadata,
    IncompatibleFormat,
    UnsupportedMetadata,
    TooLarge,
    Busy,
    TimedOut,
    ChangedInput,
    Unavailable,
}
impl Error {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "resource_pack_input_invalid",
            Self::InvalidArchive => "resource_pack_archive_invalid",
            Self::InvalidMetadata => "resource_pack_metadata_invalid",
            Self::IncompatibleFormat => "resource_pack_format_incompatible",
            Self::UnsupportedMetadata => "resource_pack_metadata_unsupported",
            Self::TooLarge => "resource_pack_limit_exceeded",
            Self::Busy => "resource_pack_build_busy",
            Self::TimedOut => "resource_pack_build_timed_out",
            Self::ChangedInput => "resource_pack_input_changed",
            Self::Unavailable => "resource_pack_storage_unavailable",
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        error
            .get_ref()
            .and_then(|cause| cause.downcast_ref::<Self>())
            .copied()
            .unwrap_or(Self::Unavailable)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackFormat {
    pub major: u32,
    pub minor: u32,
}
impl PackFormat {
    pub fn new(major: u32, minor: u32) -> super::Result<Self> {
        if major == 0 || major > i32::MAX as u32 || minor > i32::MAX as u32 {
            return Err(Error::InvalidMetadata);
        }
        Ok(Self { major, minor })
    }
}

/// Pinned provider/upload bytes. IDs are private stable identifiers, not filenames.
pub struct Input {
    pub(crate) id: String,
    pub(crate) file: File,
    pub(crate) sha256: String,
}
impl Input {
    pub fn new(id: String, file: File, sha256: String) -> super::Result<Self> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || sha256.len() != 64
            || !sha256
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(Error::InvalidInput);
        }
        Ok(Self { id, file, sha256 })
    }
}

/// Service budgets, not promises about Minecraft client download-size limits.
#[derive(Clone, Copy, Debug)]
pub struct BuildLimits {
    pub inputs: usize,
    pub entries: usize,
    pub compressed_bytes: u64,
    pub expanded_bytes: u64,
    pub member_bytes: u64,
    pub json_bytes: usize,
    pub output_bytes: u64,
    pub time: Duration,
}
impl Default for BuildLimits {
    fn default() -> Self {
        Self {
            inputs: 16,
            entries: 8192,
            compressed_bytes: 256 * 1024 * 1024,
            expanded_bytes: 512 * 1024 * 1024,
            member_bytes: 64 * 1024 * 1024,
            json_bytes: 2 * 1024 * 1024,
            output_bytes: 128 * 1024 * 1024,
            time: Duration::from_secs(120),
        }
    }
}
impl BuildLimits {
    pub(crate) fn validate(self) -> super::Result<Self> {
        let max = Self::default();
        if self.inputs == 0
            || self.inputs > max.inputs
            || self.entries == 0
            || self.entries > max.entries
            || self.compressed_bytes == 0
            || self.compressed_bytes > max.compressed_bytes
            || self.expanded_bytes == 0
            || self.expanded_bytes > max.expanded_bytes
            || self.member_bytes == 0
            || self.member_bytes > max.member_bytes
            || self.json_bytes == 0
            || self.json_bytes > max.json_bytes
            || self.output_bytes == 0
            || self.output_bytes > max.output_bytes
            || self.time.is_zero()
            || self.time > max.time
        {
            return Err(Error::InvalidInput);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    HigherPriority,
    LanguageKeys,
    SoundEvents,
    AtlasSources,
    FontProviders,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedPath {
    pub path: String,
    pub layers: Vec<String>,
    pub resolution: Resolution,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildReport {
    /// Low to high priority, exactly the supplied order. No implicit sorting of packs.
    pub inputs: Vec<String>,
    pub target: PackFormat,
    pub resolved: Vec<ResolvedPath>,
}
