//! Bounded, deterministic preparation of an in-memory hybrid Vesta ZIP.
//!
//! This is not publication, installation authority, a provider resolver or a route.
//! Every byte comes from the retained snapshot; paths are never reopened. Provider
//! descriptions undergo admission here because the protocol projection is data-only.
use super::{MAX_PINNED_BYTES, MAX_PINNED_FILE_BYTES, PreparedSnapshot, PrivateFileEntry};
use crate::config::{LoaderKind, PackMetadata};
use sha1::Sha1;
use sha2::{Digest, Sha256 as DigestSha256};
use std::{
    error::Error,
    fmt,
    io::{self, Seek, SeekFrom, Write},
};
use url::Url;
use vesta_protocol::pack::{
    FORMAT, INDEX_NAME,
    catalogue::{FileSource, MAX_FILES, PackContent, Platform, Sha256, SourceKind},
    model::{BuilderFile, BuilderFileSource, BuilderManifest, BuilderModloader},
    project_index,
};
use zip::{CompressionMethod, DateTime, System, ZipWriter, write::SimpleFileOptions};

pub const MAX_VESTA_INDEX_BYTES: usize = 1_048_576;
pub const MAX_VESTA_ARCHIVE_BYTES: usize = 68 * 1_024 * 1_024;
pub const MAX_VESTA_ENTRIES: usize = 4_097;

/// Output budgets, inclusive. Entries count actual ZIP members, including the
/// root index; provider references do not consume a ZIP member. Snapshot work is
/// independently bounded by the validated catalogue and retained-body ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VestaBuildLimits {
    index_bytes: usize,
    archive_bytes: usize,
    entries: usize,
}
impl VestaBuildLimits {
    pub fn new(
        index_bytes: usize,
        archive_bytes: usize,
        entries: usize,
    ) -> Result<Self, VestaBuildError> {
        if index_bytes == 0
            || index_bytes > MAX_VESTA_INDEX_BYTES
            || archive_bytes == 0
            || archive_bytes > MAX_VESTA_ARCHIVE_BYTES
            || entries == 0
            || entries > MAX_VESTA_ENTRIES
        {
            return Err(VestaBuildError::InvalidLimit);
        }
        Ok(Self {
            index_bytes,
            archive_bytes,
            entries,
        })
    }
    pub const fn production() -> Self {
        Self {
            index_bytes: MAX_VESTA_INDEX_BYTES,
            archive_bytes: MAX_VESTA_ARCHIVE_BYTES,
            entries: MAX_VESTA_ENTRIES,
        }
    }
}
impl Default for VestaBuildLimits {
    fn default() -> Self {
        Self::production()
    }
}

/// Fixed classifications only. Native ZIP/JSON errors and input text are erased.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum VestaBuildError {
    InvalidLimit,
    MetadataMismatch,
    InvalidSnapshot,
    InvalidReference,
    TooLarge,
    Unavailable,
}
impl VestaBuildError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimit => "vesta_build_limit_invalid",
            Self::MetadataMismatch => "vesta_build_metadata_mismatch",
            Self::InvalidSnapshot => "vesta_build_snapshot_invalid",
            Self::InvalidReference => "vesta_build_reference_invalid",
            Self::TooLarge => "vesta_build_too_large",
            Self::Unavailable => "vesta_build_unavailable",
        }
    }
}
impl fmt::Display for VestaBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Vesta pack build failed")
    }
}
impl fmt::Debug for VestaBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VestaBuildError")
            .field("code", &self.code())
            .finish()
    }
}
impl Error for VestaBuildError {}

/// Immutable local preparation, never permission to publish or download.
pub struct BuiltVestaPack {
    private_files: Vec<PrivateFileEntry>,
    bytes: Vec<u8>,
    sha256: Sha256,
    entries: usize,
    version: String,
    revision: u64,
    minimum_revision: u64,
}
impl BuiltVestaPack {
    /// Backend-local build report, never part of the served artifact/index.
    pub fn private_files(&self) -> &[PrivateFileEntry] {
        &self.private_files
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn sha256(&self) -> &Sha256 {
        &self.sha256
    }
    pub fn size(&self) -> u64 {
        self.bytes.len() as u64
    }
    pub const fn format(&self) -> &'static str {
        FORMAT
    }
    /// The display version and revisions embedded in the index.
    pub fn version(&self) -> &str {
        &self.version
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub const fn minimum_revision(&self) -> u64 {
        self.minimum_revision
    }
}
impl fmt::Debug for BuiltVestaPack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuiltVestaPack")
            .field("size", &self.bytes.len())
            .field("entries", &self.entries)
            .finish_non_exhaustive()
    }
}

pub fn build_vesta(
    snapshot: &PreparedSnapshot,
    metadata: &PackMetadata,
    limits: VestaBuildLimits,
) -> Result<BuiltVestaPack, VestaBuildError> {
    let observed = snapshot.manifest.metadata();
    if observed.name() != metadata.name()
        || observed.version() != metadata.version()
        || observed.revision() != metadata.revision()
        || observed.minimum_revision() != metadata.minimum_revision()
    {
        return Err(VestaBuildError::MetadataMismatch);
    }
    let PackContent::Custom(files) = snapshot.manifest.content() else {
        return Err(VestaBuildError::InvalidSnapshot);
    };
    if files.len() > MAX_FILES || files.len() != snapshot.bodies.len() {
        return Err(VestaBuildError::InvalidSnapshot);
    }

    let mut total = 0usize;
    let mut entries = 1usize; // Exactly one root index, including for an empty pack.
    let mut index_files = Vec::new();
    index_files
        .try_reserve_exact(files.len())
        .map_err(|_| VestaBuildError::Unavailable)?;
    for file in files {
        let body = snapshot
            .bodies
            .get(file.path())
            .ok_or(VestaBuildError::InvalidSnapshot)?;
        if body.len() > MAX_PINNED_FILE_BYTES {
            return Err(VestaBuildError::InvalidSnapshot);
        }
        total = total
            .checked_add(body.len())
            .filter(|total| *total <= MAX_PINNED_BYTES)
            .ok_or(VestaBuildError::InvalidSnapshot)?;
        if file.size() != body.len() as u64
            || &DigestSha256::digest(body.as_ref())[..] != file.sha256().as_bytes()
        {
            return Err(VestaBuildError::InvalidSnapshot);
        }
        let actual_sha1 = Sha1::digest(body.as_ref());
        if file
            .sha1()
            .is_some_and(|expected| &actual_sha1[..] != expected)
        {
            return Err(VestaBuildError::InvalidSnapshot);
        }
        let (source, sha1) = match file.source().kind() {
            SourceKind::Platform => {
                if body.is_empty() {
                    return Err(VestaBuildError::InvalidReference);
                }
                (
                    Some(reference_source(file.source())?),
                    Some(hex::encode(actual_sha1)),
                )
            }
            SourceKind::Embedded | SourceKind::Server => {
                entries = entries.checked_add(1).ok_or(VestaBuildError::TooLarge)?;
                (None, file.sha1().map(hex::encode))
            }
        };
        index_files.push(BuilderFile {
            path: Some(file.path().as_str().to_owned()),
            sha1,
            sha256: Some(file.sha256().to_hex()),
            size: i64::try_from(file.size()).map_err(|_| VestaBuildError::InvalidSnapshot)?,
            source,
        });
    }
    if total != snapshot.total {
        return Err(VestaBuildError::InvalidSnapshot);
    }
    if entries > limits.entries {
        return Err(VestaBuildError::TooLarge);
    }
    let manifest = BuilderManifest {
        name: Some(metadata.name().to_owned()),
        version: Some(metadata.version().to_owned()),
        revision: i64::try_from(metadata.revision())
            .map_err(|_| VestaBuildError::MetadataMismatch)?,
        minimum_revision: i64::try_from(metadata.minimum_revision())
            .map_err(|_| VestaBuildError::MetadataMismatch)?,
        minecraft_version: Some(metadata.minecraft_version().to_owned()),
        modloader: metadata.loader().map(|loader| BuilderModloader {
            r#type: Some(
                match loader.kind() {
                    LoaderKind::Fabric => "fabric",
                    LoaderKind::Quilt => "quilt",
                    LoaderKind::Forge => "forge",
                    LoaderKind::Neoforge => "neoforge",
                }
                .to_owned(),
            ),
            version: Some(loader.version().to_owned()),
        }),
        // Projection combines and sorts both collections. Categories themselves
        // carry no admission or extraction meaning; all admitted files go here.
        mods: Some(index_files),
        overrides: None,
    };
    let mut index = project_index(&manifest).map_err(|_| VestaBuildError::InvalidSnapshot)?;
    index.join = metadata.join().cloned();
    let mut index_sink = BoundedBuffer::new(limits.index_bytes);
    let encoded = serde_json::to_writer(&mut index_sink, &index);
    if encoded.is_err() {
        return Err(index_sink.failure.unwrap_or(VestaBuildError::Unavailable));
    }
    let index_bytes = index_sink.finish()?;

    // A conservative Stored-ZIP lower bound: EOCD plus every local/central
    // header, both filename copies and the body. Reject ordinary cap failures
    // before creating a writer. The bounded sink remains authoritative for any
    // additional library overhead; this is not a handwritten archive format.
    let mut archive_lower_bound = 22usize;
    for file in files {
        if file.source().kind() != SourceKind::Platform {
            let name_bytes = "overrides/"
                .len()
                .checked_add(file.path().as_str().len())
                .ok_or(VestaBuildError::TooLarge)?;
            archive_lower_bound = archive_lower_bound
                .checked_add(minimum_stored_member_bytes(
                    name_bytes,
                    snapshot.bodies[file.path()].len(),
                )?)
                .ok_or(VestaBuildError::TooLarge)?;
        }
    }
    archive_lower_bound = archive_lower_bound
        .checked_add(minimum_stored_member_bytes(
            INDEX_NAME.len(),
            index_bytes.len(),
        )?)
        .ok_or(VestaBuildError::TooLarge)?;
    if archive_lower_bound > limits.archive_bytes {
        return Err(VestaBuildError::TooLarge);
    }

    let mut archive_sink = BoundedBuffer::new(limits.archive_bytes);
    let archived = (|| -> zip::result::ZipResult<()> {
        let mut archive = ZipWriter::new(&mut archive_sink);
        let options = SimpleFileOptions::DEFAULT
            .compression_method(CompressionMethod::Stored)
            .last_modified_time(DateTime::DEFAULT)
            .unix_permissions(0o644)
            .system(System::Unix)
            .large_file(false);
        // Manifest construction validated and globally sorted the portable paths.
        // No directory entries, ZIP comments, encryption or extra fields are added.
        for file in files {
            if file.source().kind() != SourceKind::Platform {
                archive.start_file(format!("overrides/{}", file.path().as_str()), options)?;
                archive.write_all(snapshot.bodies[file.path()].as_ref())?;
            }
        }
        archive.start_file(INDEX_NAME, options)?;
        archive.write_all(&index_bytes)?;
        archive.finish().map(|_| ())
    })();
    if archived.is_err() {
        return Err(archive_sink.failure.unwrap_or(VestaBuildError::Unavailable));
    }
    let bytes = archive_sink.finish()?;
    let sha256 = Sha256::parse(&hex::encode(DigestSha256::digest(&bytes)))
        .map_err(|_| VestaBuildError::Unavailable)?;
    Ok(BuiltVestaPack {
        private_files: snapshot.private_files.clone(),
        bytes,
        sha256,
        entries,
        version: metadata.version().to_owned(),
        revision: metadata.revision(),
        minimum_revision: metadata.minimum_revision(),
    })
}

fn minimum_stored_member_bytes(
    name_bytes: usize,
    body_bytes: usize,
) -> Result<usize, VestaBuildError> {
    name_bytes
        .checked_mul(2)
        .and_then(|names| names.checked_add(30 + 46))
        .and_then(|headers| headers.checked_add(body_bytes))
        .ok_or(VestaBuildError::TooLarge)
}

/// Admit immutable provider identity, not just the protocol's descriptive strings.
/// CurseForge supports resolver-only references; Modrinth requires its pinned CDN
/// URL. No URL is fetched, rewritten or inferred from a mutable filename.
fn reference_source(source: &FileSource) -> Result<BuilderFileSource, VestaBuildError> {
    let platform = source
        .platform_name()
        .ok_or(VestaBuildError::InvalidReference)?;
    let project = source
        .project_id()
        .ok_or(VestaBuildError::InvalidReference)?;
    let version = source
        .version_id()
        .ok_or(VestaBuildError::InvalidReference)?;
    let kind = match platform {
        Platform::Modrinth => {
            for id in [project, version] {
                if id.len() != 8 || !id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
                    return Err(VestaBuildError::InvalidReference);
                }
            }
            let raw = source.url().ok_or(VestaBuildError::InvalidReference)?;
            validate_reference_url(raw, platform, project, version)?;
            "modrinth"
        }
        Platform::Curseforge => {
            canonical_positive_id(project)?;
            canonical_positive_id(version)?;
            if let Some(raw) = source.url() {
                validate_reference_url(raw, platform, project, version)?;
            }
            "curseforge"
        }
    };
    Ok(BuilderFileSource {
        r#type: Some(kind.to_owned()),
        platform: Some(kind.to_owned()),
        project_id: Some(project.to_owned()),
        version_id: Some(version.to_owned()),
        url: source.url().map(str::to_owned),
    })
}

fn canonical_positive_id(value: &str) -> Result<u32, VestaBuildError> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(VestaBuildError::InvalidReference);
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|id| *id != 0)
        .ok_or(VestaBuildError::InvalidReference)
}

fn validate_reference_url(
    raw: &str,
    platform: Platform,
    project: &str,
    version: &str,
) -> Result<(), VestaBuildError> {
    // Reject dangerous spelling before the WHATWG parser can repair/normalize it.
    if raw.is_empty()
        || raw.len() > 2_048
        || !raw.is_ascii()
        || raw.bytes().any(|byte| {
            byte.is_ascii_whitespace() || byte.is_ascii_control() || matches!(byte, b'\\' | b'%')
        })
    {
        return Err(VestaBuildError::InvalidReference);
    }
    let (_, authority_and_path) = raw
        .split_once("://")
        .ok_or(VestaBuildError::InvalidReference)?;
    let (authority, _) = authority_and_path
        .split_once('/')
        .ok_or(VestaBuildError::InvalidReference)?;
    if authority.contains('@') {
        return Err(VestaBuildError::InvalidReference);
    }
    let raw_path = &authority_and_path[authority.len()..];
    let parsed = Url::parse(raw).map_err(|_| VestaBuildError::InvalidReference)?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.port().is_some()
        || parsed.path() != raw_path
    {
        return Err(VestaBuildError::InvalidReference);
    }
    let prefix = match platform {
        Platform::Modrinth => {
            if parsed.host_str() != Some("cdn.modrinth.com") {
                return Err(VestaBuildError::InvalidReference);
            }
            format!("/data/{project}/versions/{version}/")
        }
        Platform::Curseforge => {
            if !matches!(
                parsed.host_str(),
                Some("edge.forgecdn.net" | "mediafilez.forgecdn.net")
            ) {
                return Err(VestaBuildError::InvalidReference);
            }
            let file_id = canonical_positive_id(version)?;
            format!("/files/{}/{}/", file_id / 1_000, file_id % 1_000)
        }
    };
    if !parsed
        .path()
        .strip_prefix(&prefix)
        .is_some_and(|filename| !filename.is_empty() && !filename.contains('/'))
    {
        return Err(VestaBuildError::InvalidReference);
    }
    Ok(())
}

/// A bounded owned seekable buffer. Backpatches can only seek within existing
/// bytes; holes and growth by seeking are forbidden. The first failure is sticky,
/// so ZIP drop/finalization cannot turn a partially failed output into an artifact.
struct BoundedBuffer {
    bytes: Vec<u8>,
    position: usize,
    limit: usize,
    failure: Option<VestaBuildError>,
}
impl BoundedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            position: 0,
            limit,
            failure: None,
        }
    }
    fn fail(&mut self, error: VestaBuildError) -> io::Error {
        self.failure.get_or_insert(error);
        io::Error::other("Vesta pack buffer failed")
    }
    fn finish(self) -> Result<Vec<u8>, VestaBuildError> {
        if let Some(error) = self.failure {
            Err(error)
        } else {
            Ok(self.bytes)
        }
    }
}
impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(error) = self.failure {
            return Err(self.fail(error));
        }
        let Some(end) = self.position.checked_add(bytes.len()) else {
            return Err(self.fail(VestaBuildError::TooLarge));
        };
        if end > self.limit {
            return Err(self.fail(VestaBuildError::TooLarge));
        }
        if end > self.bytes.capacity() {
            // Geometric growth avoids repeated JSON-token reallocations, while
            // every requested allocation remains within the explicit budget.
            let capacity = self
                .bytes
                .capacity()
                .checked_mul(2)
                .unwrap_or(self.limit)
                .max(4_096)
                .min(self.limit)
                .max(end);
            if self
                .bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .is_err()
            {
                return Err(self.fail(VestaBuildError::Unavailable));
            }
        }
        if end > self.bytes.len() {
            self.bytes.resize(end, 0);
        }
        self.bytes[self.position..end].copy_from_slice(bytes);
        self.position = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if let Some(error) = self.failure {
            Err(self.fail(error))
        } else {
            Ok(())
        }
    }
}
impl Seek for BoundedBuffer {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        if let Some(error) = self.failure {
            return Err(self.fail(error));
        }
        let position = match from {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::End(delta) => self.bytes.len() as i128 + i128::from(delta),
            SeekFrom::Current(delta) => self.position as i128 + i128::from(delta),
        };
        if position < 0 {
            return Err(self.fail(VestaBuildError::Unavailable));
        }
        if position > self.limit as i128 {
            return Err(self.fail(VestaBuildError::TooLarge));
        }
        if position > self.bytes.len() as i128 {
            return Err(self.fail(VestaBuildError::Unavailable));
        }
        self.position =
            usize::try_from(position).map_err(|_| self.fail(VestaBuildError::TooLarge))?;
        Ok(self.position as u64)
    }
}

#[cfg(test)]
#[path = "build_tests.rs"]
mod tests;
