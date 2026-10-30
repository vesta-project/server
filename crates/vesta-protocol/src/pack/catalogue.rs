//! Validated, immutable pack descriptions, independent of the data-only index projection.
//!
//! These are application values, not installation or download authority. Paths are
//! portable **lexical** names only: ASCII components, forward slashes, no rewriting,
//! percent decoding, Unicode normalization, or backslash conversion. Native adapters
//! must separately enforce confinement, including symlinks, junctions and races.
//! URLs are bounded opaque descriptions; this module neither parses them into a
//! trusted endpoint nor authorizes requests (including requests to private networks).
//!
//! `Manifest::decode` accepts a strict Rust-native schema, not the Java manifest or
//! Vesta index schema: `schemaVersion`, `name`, `version`, `revision`,
//! `minimumRevision`, and tagged `content`. Custom content has `mode: "custom"`
//! and `files`; platform/archive content describes a whole pack, never an empty
//! custom inventory. Unknown/duplicate fields and trailing JSON are rejected.

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;
use std::fmt;

pub const SCHEMA_VERSION: u16 = 1;
pub const MAX_JSON_BYTES: usize = 32 * 1_048_576;
pub const MAX_FILES: usize = 65_536;
pub const MAX_PATH_BYTES: usize = 1_024;
pub const MAX_COMPONENT_BYTES: usize = 128;
pub const MAX_PATH_COMPONENTS: usize = 32;
pub const MAX_TEXT_BYTES: usize = 256;
pub const MAX_SOURCE_ID_BYTES: usize = 128;
pub const MAX_SOURCE_URL_BYTES: usize = 2_048;
pub const MAX_TOTAL_TEXT_BYTES: usize = 32 * 1_048_576;
pub const MAX_FILE_SIZE: u64 = 8 * 1_024 * 1_024 * 1_024;
pub const MAX_TOTAL_SIZE: u64 = 128 * 1_024 * 1_024 * 1_024;
/// Largest integer exactly representable by common JSON number consumers.
pub const MAX_REVISION: u64 = (1_u64 << 53) - 1;

/// Input-free failure codes; no parser error, path, hash, text or URL is retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogueError {
    InvalidJson,
    InputTooLarge,
    UnsupportedSchema,
    InvalidText,
    InvalidPath,
    InvalidHash,
    InvalidSource,
    InvalidSize,
    InvalidRevision,
    TooManyFiles,
    BudgetExceeded,
    PathCollision,
}

impl fmt::Display for CatalogueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidJson => "Pack catalogue failed: invalid_json",
            Self::InputTooLarge => "Pack catalogue failed: input_too_large",
            Self::UnsupportedSchema => "Pack catalogue failed: unsupported_schema",
            Self::InvalidText => "Pack catalogue failed: invalid_text",
            Self::InvalidPath => "Pack catalogue failed: invalid_path",
            Self::InvalidHash => "Pack catalogue failed: invalid_hash",
            Self::InvalidSource => "Pack catalogue failed: invalid_source",
            Self::InvalidSize => "Pack catalogue failed: invalid_size",
            Self::InvalidRevision => "Pack catalogue failed: invalid_revision",
            Self::TooManyFiles => "Pack catalogue failed: too_many_files",
            Self::BudgetExceeded => "Pack catalogue failed: budget_exceeded",
            Self::PathCollision => "Pack catalogue failed: path_collision",
        })
    }
}

impl std::error::Error for CatalogueError {}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct RelativePath(String);

impl RelativePath {
    /// Rejects rather than repairs backslashes, edge spaces, dot components and aliases.
    pub fn parse(path: &str) -> Result<Self, CatalogueError> {
        if path.is_empty() || path.len() > MAX_PATH_BYTES {
            return Err(CatalogueError::InvalidPath);
        }
        let mut count = 0;
        for component in path.split('/') {
            count += 1;
            if count > MAX_PATH_COMPONENTS
                || component.is_empty()
                || component.len() > MAX_COMPONENT_BYTES
                || component == "."
                || component == ".."
                || component.ends_with('.')
                || component.starts_with(' ')
                || component.ends_with(' ')
                || !component.replace('?', "").bytes().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(
                            c,
                            b'~' | b',' | b'.' | b'_' | b'-' | b'+' | b'\'' | b' ' | b'(' | b')' | b'[' | b']'
                        )
                })
            {
                return Err(CatalogueError::InvalidPath);
            }
            let stem = component.split('.').next().unwrap_or_default();
            let upper = stem.trim_end_matches(' ').to_ascii_uppercase();
            if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (upper.len() == 4
                    && (upper.starts_with("COM") || upper.starts_with("LPT"))
                    && upper.as_bytes()[3].is_ascii_digit())
            {
                return Err(CatalogueError::InvalidPath);
            }
        }
        Ok(Self(path.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RelativePath([redacted])")
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Sha256([u8; 32]);

impl Sha256 {
    /// Exactly 64 ASCII hex digits. Hexadecimal case is canonicalized as bytes.
    pub fn parse(hex: &str) -> Result<Self, CatalogueError> {
        Ok(Self(parse_hash(hex)?))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            use fmt::Write;
            // Writing to String is infallible.
            let _ = write!(out, "{byte:02x}");
        }
        out
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Sha256([redacted])")
    }
}

fn parse_hash<const N: usize>(hex: &str) -> Result<[u8; N], CatalogueError> {
    if hex.len() != N * 2 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(CatalogueError::InvalidHash);
    }
    let mut hash = [0; N];
    for (out, pair) in hash.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        let digit = |c: u8| match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => c - b'A' + 10,
        };
        *out = digit(pair[0]) * 16 + digit(pair[1]);
    }
    Ok(hash)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Modrinth,
    Curseforge,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    Embedded,
    Server,
    Platform,
}

/// Descriptive provenance. Even a valid URL is not permission to fetch it.
#[derive(Clone, Eq, PartialEq)]
pub struct FileSource {
    kind: SourceKind,
    platform: Option<Platform>,
    project_id: Option<String>,
    version_id: Option<String>,
    url: Option<String>,
}

impl FileSource {
    pub fn embedded() -> Self {
        Self {
            kind: SourceKind::Embedded,
            platform: None,
            project_id: None,
            version_id: None,
            url: None,
        }
    }

    pub fn server(url: Option<&str>) -> Result<Self, CatalogueError> {
        Ok(Self {
            kind: SourceKind::Server,
            url: optional_url(url)?,
            ..Self::embedded()
        })
    }

    pub fn platform(
        platform: Platform,
        project_id: &str,
        version_id: &str,
        url: Option<&str>,
    ) -> Result<Self, CatalogueError> {
        Ok(Self {
            kind: SourceKind::Platform,
            platform: Some(platform),
            project_id: Some(source_id(project_id)?),
            version_id: Some(source_id(version_id)?),
            url: optional_url(url)?,
        })
    }

    pub fn kind(&self) -> SourceKind {
        self.kind
    }

    pub fn platform_name(&self) -> Option<Platform> {
        self.platform
    }

    pub fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }

    pub fn version_id(&self) -> Option<&str> {
        self.version_id.as_deref()
    }

    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    fn text_bytes(&self) -> usize {
        self.project_id.as_ref().map_or(0, String::len)
            + self.version_id.as_ref().map_or(0, String::len)
            + self.url.as_ref().map_or(0, String::len)
    }
}

impl fmt::Debug for FileSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSource")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackFile {
    path: RelativePath,
    sha256: Sha256,
    sha1: Option<[u8; 20]>,
    size: u64,
    source: FileSource,
}

impl PackFile {
    pub fn new(
        path: &str,
        sha256: &str,
        size: u64,
        source: FileSource,
    ) -> Result<Self, CatalogueError> {
        validate_size(size)?;
        Ok(Self {
            path: RelativePath::parse(path)?,
            sha256: Sha256::parse(sha256)?,
            sha1: None,
            size,
            source,
        })
    }

    /// SHA-1 is optional descriptive legacy metadata, never the comparison hash.
    pub fn with_sha1(mut self, hash: &str) -> Result<Self, CatalogueError> {
        self.sha1 = Some(parse_hash(hash)?);
        Ok(self)
    }

    pub fn path(&self) -> &RelativePath {
        &self.path
    }

    pub fn sha256(&self) -> &Sha256 {
        &self.sha256
    }

    pub fn sha1(&self) -> Option<&[u8; 20]> {
        self.sha1.as_ref()
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn source(&self) -> &FileSource {
        &self.source
    }

    fn text_bytes(&self) -> usize {
        self.path.0.len() + 64 + self.sha1.map_or(0, |_| 40) + self.source.text_bytes()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ManifestMetadata {
    name: String,
    version: String,
    revision: u64,
    minimum_revision: u64,
}

impl ManifestMetadata {
    /// Target and minimum revisions are positive bounded integers, with
    /// `1 <= minimum_revision <= revision <= MAX_REVISION`. Invalid metadata is
    /// rejected, never silently clamped. An installed inventory may separately
    /// describe revision zero, which is below every valid manifest's minimum.
    /// Versions are exact bounded labels, not semver or locale-normalized strings.
    pub fn new(
        name: &str,
        version: &str,
        revision: u64,
        minimum_revision: u64,
    ) -> Result<Self, CatalogueError> {
        validate_revision(revision)?;
        validate_revision(minimum_revision)?;
        if minimum_revision == 0 || minimum_revision > revision {
            return Err(CatalogueError::InvalidRevision);
        }
        Ok(Self {
            name: validated_text(name, MAX_TEXT_BYTES, CatalogueError::InvalidText)?,
            version: validated_text(version, MAX_TEXT_BYTES, CatalogueError::InvalidText)?,
            revision,
            minimum_revision,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn minimum_revision(&self) -> u64 {
        self.minimum_revision
    }
}

impl fmt::Debug for ManifestMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManifestMetadata")
            .field("revision", &self.revision)
            .field("minimum_revision", &self.minimum_revision)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformPack(FileSource);

impl PlatformPack {
    pub fn new(
        platform: Platform,
        project_id: &str,
        version_id: &str,
        url: Option<&str>,
    ) -> Result<Self, CatalogueError> {
        Ok(Self(FileSource::platform(
            platform, project_id, version_id, url,
        )?))
    }

    pub fn source(&self) -> &FileSource {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveFormat {
    Zip,
    Mrpack,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ArchivePack {
    path: RelativePath,
    sha256: Sha256,
    size: u64,
    format: ArchiveFormat,
    url: Option<String>,
}

impl ArchivePack {
    pub fn new(
        path: &str,
        sha256: &str,
        size: u64,
        format: ArchiveFormat,
        url: Option<&str>,
    ) -> Result<Self, CatalogueError> {
        validate_size(size)?;
        Ok(Self {
            path: RelativePath::parse(path)?,
            sha256: Sha256::parse(sha256)?,
            size,
            format,
            url: optional_url(url)?,
        })
    }

    pub fn path(&self) -> &RelativePath {
        &self.path
    }

    pub fn sha256(&self) -> &Sha256 {
        &self.sha256
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn format(&self) -> ArchiveFormat {
        self.format
    }

    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }
}

impl fmt::Debug for ArchivePack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArchivePack")
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackContent {
    Custom(Vec<PackFile>),
    Platform(PlatformPack),
    Archive(ArchivePack),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Manifest {
    metadata: ManifestMetadata,
    content: PackContent,
}

impl Manifest {
    /// Custom files are sorted by Rust string (UTF-8 byte) ordering. Duplicates,
    /// ASCII case aliases, directory case aliases and file/directory conflicts fail.
    pub fn new(
        metadata: ManifestMetadata,
        mut content: PackContent,
    ) -> Result<Self, CatalogueError> {
        let mut text_bytes = metadata.name.len() + metadata.version.len();
        match &mut content {
            PackContent::Custom(files) => {
                if files.len() > MAX_FILES {
                    return Err(CatalogueError::TooManyFiles);
                }
                let mut total_size = 0_u64;
                for file in files.iter() {
                    add_text_budget(&mut text_bytes, file.text_bytes())?;
                    total_size = total_size
                        .checked_add(file.size)
                        .ok_or(CatalogueError::BudgetExceeded)?;
                    if total_size > MAX_TOTAL_SIZE {
                        return Err(CatalogueError::BudgetExceeded);
                    }
                }
                validate_namespace(files.iter().map(PackFile::path), false)?;
                files.sort_by(|left, right| left.path.cmp(&right.path));
            }
            PackContent::Platform(pack) => add_text_budget(&mut text_bytes, pack.0.text_bytes())?,
            PackContent::Archive(pack) => add_text_budget(
                &mut text_bytes,
                pack.path.0.len() + 64 + pack.url.as_ref().map_or(0, String::len),
            )?,
        }
        Ok(Self { metadata, content })
    }

    pub fn metadata(&self) -> &ManifestMetadata {
        &self.metadata
    }

    pub fn content(&self) -> &PackContent {
        &self.content
    }

    /// Strict byte-bounded decoding, with no unvalidated Deserialize implementation
    /// on application objects. Byte limits apply before any JSON allocation.
    pub fn decode(input: &[u8]) -> Result<Self, CatalogueError> {
        let wire: WireManifest = decode_json(input)?;
        if wire.schema_version != SCHEMA_VERSION {
            return Err(CatalogueError::UnsupportedSchema);
        }
        let metadata = ManifestMetadata::new(
            &wire.name,
            &wire.version,
            wire.revision,
            wire.minimum_revision,
        )?;
        let content = match wire.content {
            WireContent::Custom { files } => {
                let files = files
                    .0
                    .into_iter()
                    .map(WireFile::validate)
                    .collect::<Result<Vec<_>, _>>()?;
                PackContent::Custom(files)
            }
            WireContent::Platform {
                platform,
                project_id,
                version_id,
                url,
            } => PackContent::Platform(PlatformPack::new(
                platform,
                &project_id,
                &version_id,
                url.as_deref(),
            )?),
            WireContent::Archive {
                path,
                sha256,
                size,
                format,
                url,
            } => PackContent::Archive(ArchivePack::new(
                &path,
                &sha256,
                size,
                format,
                url.as_deref(),
            )?),
        };
        Self::new(metadata, content)
    }
}

pub(crate) fn validate_revision(revision: u64) -> Result<(), CatalogueError> {
    if revision > MAX_REVISION {
        Err(CatalogueError::InvalidRevision)
    } else {
        Ok(())
    }
}

pub(crate) fn validated_text(
    value: &str,
    max: usize,
    error: CatalogueError,
) -> Result<String, CatalogueError> {
    if value.is_empty()
        || value.len() > max
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(error)
    } else {
        Ok(value.to_owned())
    }
}

fn source_id(value: &str) -> Result<String, CatalogueError> {
    if value.is_empty()
        || value.len() > MAX_SOURCE_ID_BYTES
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
    {
        Err(CatalogueError::InvalidSource)
    } else {
        Ok(value.to_owned())
    }
}

fn optional_url(value: Option<&str>) -> Result<Option<String>, CatalogueError> {
    value
        .map(|url| validated_text(url, MAX_SOURCE_URL_BYTES, CatalogueError::InvalidSource))
        .transpose()
}

fn validate_size(size: u64) -> Result<(), CatalogueError> {
    if size > MAX_FILE_SIZE {
        Err(CatalogueError::InvalidSize)
    } else {
        Ok(())
    }
}

pub(crate) fn add_text_budget(total: &mut usize, extra: usize) -> Result<(), CatalogueError> {
    *total = total
        .checked_add(extra)
        .ok_or(CatalogueError::BudgetExceeded)?;
    if *total > MAX_TOTAL_TEXT_BYTES {
        Err(CatalogueError::BudgetExceeded)
    } else {
        Ok(())
    }
}

/// Also validates intermediate directory spellings, not just complete file names.
/// The union of desired and installed namespaces may repeat an *identical* file.
pub(crate) fn validate_namespace<'a>(
    paths: impl IntoIterator<Item = &'a RelativePath>,
    allow_identical_files: bool,
) -> Result<(), CatalogueError> {
    let mut nodes: BTreeMap<String, (&str, bool)> = BTreeMap::new();
    for path in paths {
        let path = path.as_str();
        for end in path
            .match_indices('/')
            .map(|(index, _)| index)
            .chain(std::iter::once(path.len()))
        {
            let original = &path[..end];
            let is_file = end == path.len();
            let folded = original.to_ascii_lowercase();
            match nodes.get(&folded) {
                Some((existing, existing_file)) => {
                    if *existing != original
                        || *existing_file != is_file
                        || (is_file && !allow_identical_files)
                    {
                        return Err(CatalogueError::PathCollision);
                    }
                }
                None => {
                    nodes.insert(folded, (original, is_file));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn decode_json<T: serde::de::DeserializeOwned>(
    input: &[u8],
) -> Result<T, CatalogueError> {
    if input.len() > MAX_JSON_BYTES {
        return Err(CatalogueError::InputTooLarge);
    }
    // A separate streaming pass rejects duplicate keys at every level, including
    // duplicate optional fields whose first value is null. No Value/map projection
    // can silently erase them before the typed schema is checked.
    serde_json::from_slice::<UniqueJson>(input).map_err(|_| CatalogueError::InvalidJson)?;
    serde_json::from_slice(input).map_err(|_| CatalogueError::InvalidJson)
}

struct UniqueJson;

impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueJson;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON without duplicate keys")
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(UniqueJson)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(UniqueJson)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(UniqueJson)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(UniqueJson)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(UniqueJson)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueJson)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<UniqueJson>()?.is_some() {}
                Ok(UniqueJson)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut keys = std::collections::BTreeSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key) {
                        return Err(de::Error::custom("duplicate field"));
                    }
                    map.next_value::<UniqueJson>()?;
                }
                Ok(UniqueJson)
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}

/// Array lengths are checked while reading; attacker-controlled size hints do not
/// drive reservations. All nested strings additionally fit the outer byte budget.
pub(crate) struct BoundedFiles<T>(pub(crate) Vec<T>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for BoundedFiles<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FilesVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for FilesVisitor<T> {
            type Value = BoundedFiles<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded file array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut files = Vec::new();
                while let Some(file) = seq.next_element()? {
                    if files.len() == MAX_FILES {
                        return Err(de::Error::custom("file limit exceeded"));
                    }
                    files.push(file);
                }
                Ok(BoundedFiles(files))
            }
        }
        deserializer.deserialize_seq(FilesVisitor(std::marker::PhantomData))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireManifest {
    schema_version: u16,
    name: String,
    version: String,
    revision: u64,
    minimum_revision: u64,
    content: WireContent,
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
enum WireContent {
    Custom {
        files: BoundedFiles<WireFile>,
    },
    Platform {
        platform: Platform,
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "versionId")]
        version_id: String,
        url: Option<String>,
    },
    Archive {
        path: String,
        sha256: String,
        size: u64,
        format: ArchiveFormat,
        url: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFile {
    path: String,
    sha256: String,
    sha1: Option<String>,
    size: u64,
    source: WireSource,
}

impl WireFile {
    fn validate(self) -> Result<PackFile, CatalogueError> {
        let source = match self.source {
            WireSource::Embedded {} => FileSource::embedded(),
            WireSource::Server { url } => FileSource::server(url.as_deref())?,
            WireSource::Platform {
                platform,
                project_id,
                version_id,
                url,
            } => FileSource::platform(platform, &project_id, &version_id, url.as_deref())?,
        };
        let file = PackFile::new(&self.path, &self.sha256, self.size, source)?;
        match self.sha1 {
            Some(hash) => file.with_sha1(&hash),
            None => Ok(file),
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum WireSource {
    Embedded {},
    Server {
        url: Option<String>,
    },
    Platform {
        platform: Platform,
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "versionId")]
        version_id: String,
        url: Option<String>,
    },
}
