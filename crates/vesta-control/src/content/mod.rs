//! Provider-resolved, bounded content installs for trusted in-process composition.
//!
//! Requesters supply identifiers, never download locations, digests or paths. Provider
//! clients and the catalogue registry are trusted server-side adapters, not request DTOs.
//! Installs reuse `files.create` / `files.overwrite` (including canonical scopes), and
//! `FileBrowser::write` owns the completion transaction, replacement trash and audit.
//! The mods directory must already exist. No archive extraction or dependency install.
mod http;
mod model;
mod provider;
pub use model::*;

pub use http::{DownloadHttp, HttpResponse, UreqDownloadHttp};
pub use provider::UreqProviderClient;
pub(crate) use provider::curseforge_document_source;

use crate::{
    files::{FileBrowser, FileChange, FileError, MAX_WRITE_BYTES, RelativePath, WritePrecondition},
    identity::Subject,
};
use sha1::Sha1;
use sha2::{Digest, Sha512};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    io::Read,
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};
use url::Url;

pub type Result<T> = std::result::Result<T, ContentError>;
const MAX_REDIRECTS: usize = 5;
const MAX_URL_BYTES: usize = 8_192;
pub const REMOTE_INSTALL_TIMEOUT: Duration = Duration::from_secs(120);
static INSTALLS: InstallGate = InstallGate::new(2);
struct InstallGate {
    count: Mutex<usize>,
    available: Condvar,
    limit: usize,
}
impl InstallGate {
    const fn new(limit: usize) -> Self {
        Self {
            count: Mutex::new(0),
            available: Condvar::new(),
            limit,
        }
    }
    fn acquire(&self, deadline: Option<Instant>) -> Result<InstallPermit<'_>> {
        let mut count = self
            .count
            .lock()
            .map_err(|_| ContentError::DownloadFailed)?;
        loop {
            if let Some(deadline) = deadline {
                deadline_check(deadline)?;
            }
            if *count < self.limit {
                *count += 1;
                return Ok(InstallPermit { gate: self });
            }
            let deadline = deadline.ok_or(ContentError::DownloadFailed)?;
            count = self
                .available
                .wait_timeout(count, deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| ContentError::DownloadFailed)?
                .0;
        }
    }
}
struct InstallPermit<'a> {
    gate: &'a InstallGate,
}
impl InstallPermit<'static> {
    fn acquire() -> Result<Self> {
        INSTALLS.acquire(None)
    }
}
impl Drop for InstallPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut count) = self.gate.count.lock() {
            *count -= 1;
            self.gate.available.notify_one();
        }
    }
}
pub(super) fn deadline_check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        Err(ContentError::DownloadFailed)
    } else {
        Ok(())
    }
}
struct DeadlineRead {
    reader: Box<dyn Read>,
    deadline: Instant,
}
impl Read for DeadlineRead {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let check = || {
            if Instant::now() >= self.deadline {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "install deadline",
                ))
            } else {
                Ok(())
            }
        };
        check()?;
        let n = self.reader.read(bytes)?;
        check()?;
        Ok(n)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provider {
    Modrinth,
    Curseforge,
}

/// The complete untrusted desired change. No URL/hash/path can be supplied here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallRequest {
    Provider {
        provider: Provider,
        project_id: String,
        version_id: String,
    },
    /// Lookup key of a hashed source already registered by trusted pack composition.
    Catalogue { source_id: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExpectedHash {
    Sha512(String),
    Sha1(String),
}
impl ExpectedHash {
    fn validate(&self) -> Result<()> {
        let (text, length) = match self {
            Self::Sha512(text) => (text, 128),
            Self::Sha1(text) => (text, 40),
        };
        if text.len() != length || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ContentError::InvalidMetadata);
        }
        Ok(())
    }
}

/// Trusted provider metadata or a trusted manifest entry; never deserialize a caller
/// request into this type. Catalogue entries are restricted to the same CDN policy.
#[derive(Clone)]
pub struct ResolvedSource {
    pub provider: Provider,
    pub filename: String,
    pub download_url: String,
    pub expected_hash: ExpectedHash,
}
impl fmt::Debug for ResolvedSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResolvedSource[<redacted>]")
    }
}

pub trait ProviderClient {
    /// Identity for archive ingestion only; does not grant download or filesystem authority.
    fn archive_filename(&self, provider: Provider, project: &str, version: &str) -> Result<String> {
        self.resolve(provider, project, version).map(|source| source.filename)
    }
    /// Optional bounded warm-up; individual resolution remains authoritative.
    fn prefetch_curseforge(&self, _files: &[(String, String)]) -> Result<()> {
        Ok(())
    }
    /// Must bind the returned file to both identifiers and obtain its expected digest
    /// from authoritative provider metadata. Implementations are trusted composition.
    fn resolve(
        &self,
        provider: Provider,
        project_id: &str,
        version_id: &str,
    ) -> Result<ResolvedSource>;
    /// Trusted adapters must bound blocking work by this whole-install deadline.
    fn resolve_with_deadline(
        &self,
        provider: Provider,
        project_id: &str,
        version_id: &str,
        deadline: Instant,
    ) -> Result<ResolvedSource> {
        deadline_check(deadline)?;
        let result = self.resolve(provider, project_id, version_id)?;
        deadline_check(deadline)?;
        Ok(result)
    }
}

/// Populated only by trusted pack catalogue/manifest composition, never by requests.
#[derive(Default)]
pub struct TrustedSourceRegistry {
    sources: BTreeMap<String, ResolvedSource>,
}
impl TrustedSourceRegistry {
    pub fn register(&mut self, source_id: &str, source: ResolvedSource) -> Result<()> {
        validate_id(source_id)?;
        validate_source(&source)?;
        if self.sources.contains_key(source_id) {
            return Err(ContentError::InvalidRequest);
        }
        self.sources.insert(source_id.to_owned(), source);
        Ok(())
    }
}

pub struct ContentInstaller<'a> {
    files: &'a FileBrowser,
    providers: &'a dyn ProviderClient,
    http: &'a dyn DownloadHttp,
    registry: &'a TrustedSourceRegistry,
}
impl<'a> ContentInstaller<'a> {
    /// Managed server target; requesters still supply provider identifiers only.
    #[allow(clippy::too_many_arguments)]
    pub fn install_content(
        &self,
        runtime: &crate::operations::RuntimeHandle,
        subject: &Subject,
        key: &str,
        request: &InstallRequest,
        kind: ContentKind,
        directory: &str,
        replace: Option<(&str, &str)>,
    ) -> Result<ContentChange> {
        let InstallRequest::Provider {
            provider,
            project_id,
            version_id,
        } = request
        else {
            return Err(ContentError::InvalidRequest);
        };
        validate_id(project_id)?;
        validate_id(version_id)?;
        self.files
            .check_global_authority(subject, crate::identity::Capability::ContentInstall)
            .map_err(ContentError::File)?;
        let condition = replace.map_or(WritePrecondition::Absent, |(_, hash)| {
            WritePrecondition::Matches(hash.into())
        });
        self.files
            .check_content_directory_admission(
                subject,
                &condition,
                directory,
                if kind == ContentKind::Datapack {
                    ".zip"
                } else {
                    ".jar"
                },
            )
            .map_err(ContentError::File)?;
        let deadline = Instant::now() + REMOTE_INSTALL_TIMEOUT;
        let _permit = InstallPermit::acquire()?;
        let source = self
            .providers
            .resolve_with_deadline(*provider, project_id, version_id, deadline)?;
        if source.provider != *provider {
            return Err(ContentError::InvalidMetadata);
        }
        let (_, mut url) = validate_source(&source)?;
        let path = if let Some((path, _)) = replace {
            path.to_owned()
        } else {
            format!("{directory}/{}", source.filename)
        };
        let expected = replace.map(|(_, hash)| hash.to_owned());
        let relative = RelativePath::parse(&path).map_err(ContentError::File)?;
        if !path.starts_with(&format!("{directory}/")) || path[directory.len() + 1..].contains('/')
        {
            return Err(ContentError::InvalidRequest);
        }
        self.files
            .check_write_authority(
                subject,
                &relative,
                &expected
                    .clone()
                    .map_or(WritePrecondition::Absent, WritePrecondition::Matches),
            )
            .map_err(ContentError::File)?;
        let bytes = self.download_source(&source, &mut url, Some(deadline))?;
        self.files
            .content_install_bytes(
                runtime,
                subject,
                key,
                kind,
                path,
                expected,
                ContentProvider {
                    provider: match provider {
                        Provider::Modrinth => "modrinth",
                        Provider::Curseforge => "curseforge",
                    }
                    .into(),
                    project_id: project_id.clone(),
                    version_id: version_id.clone(),
                },
                &bytes,
            )
            .map_err(ContentError::File)
    }
    pub fn new(
        files: &'a FileBrowser,
        providers: &'a dyn ProviderClient,
        http: &'a dyn DownloadHttp,
        registry: &'a TrustedSourceRegistry,
    ) -> Self {
        Self {
            files,
            providers,
            http,
            registry,
        }
    }
    pub fn install(
        &self,
        subject: &Subject,
        request: &InstallRequest,
        precondition: &WritePrecondition,
    ) -> Result<FileChange> {
        self.install_with_files(self.files, subject, request, precondition, None)
    }
    /// Remote requests are create-only, provider identifiers only. The provider
    /// intent and exact resource authority survive completion in the file ledger.
    pub fn install_remote(
        &self,
        subject: &Subject,
        request: &InstallRequest,
        key: &str,
    ) -> Result<serde_json::Value> {
        let InstallRequest::Provider {
            provider,
            project_id,
            version_id,
        } = request
        else {
            return Err(ContentError::InvalidRequest);
        };
        validate_id(project_id)?;
        validate_id(version_id)?;
        let provider = match provider {
            Provider::Modrinth => "modrinth",
            Provider::Curseforge => "curseforge",
        };
        let input = serde_json::json!(["install", provider, project_id, version_id]);
        let deadline = Instant::now() + REMOTE_INSTALL_TIMEOUT;
        self.files.for_content(deadline).execute_remote(subject,key,input,|| {
            self.files.check_content_admission(subject,&WritePrecondition::Absent)?;
            Ok(vec![(crate::identity::Capability::ContentInstall.id().into(),String::new())])
        },|files| {
            let _permit = InstallPermit::acquire().map_err(|_| FileError::Unavailable)?;
            self.install_with_files(files,subject,request,&WritePrecondition::Absent,Some(deadline))
                .map(|v| serde_json::json!({"outcome":"succeeded","sha256":v.sha256,"size":v.size.to_string(),"trash_id":v.trash}))
                .map_err(|e| match e { ContentError::File(e) => e, ContentError::TooLarge => FileError::TooLarge, _ => FileError::Unavailable })
        }).map_err(ContentError::File)
    }
    fn install_with_files(
        &self,
        files: &FileBrowser,
        subject: &Subject,
        request: &InstallRequest,
        precondition: &WritePrecondition,
        deadline: Option<Instant>,
    ) -> Result<FileChange> {
        files
            .check_content_admission(subject, precondition)
            .map_err(ContentError::File)?;
        let source = match request {
            InstallRequest::Provider {
                provider,
                project_id,
                version_id,
            } => {
                validate_id(project_id)?;
                validate_id(version_id)?;
                let source = match deadline {
                    Some(d) => self
                        .providers
                        .resolve_with_deadline(*provider, project_id, version_id, d)?,
                    None => self.providers.resolve(*provider, project_id, version_id)?,
                };
                if source.provider != *provider {
                    return Err(ContentError::InvalidMetadata);
                }
                match (provider, &source.expected_hash) {
                    (Provider::Modrinth, ExpectedHash::Sha512(_))
                    | (Provider::Curseforge, ExpectedHash::Sha1(_)) => {}
                    _ => return Err(ContentError::InvalidMetadata),
                }
                source
            }
            InstallRequest::Catalogue { source_id } => {
                validate_id(source_id)?;
                self.registry
                    .sources
                    .get(source_id)
                    .cloned()
                    .ok_or(ContentError::UnknownSource)?
            }
        };
        let (path, mut url) = validate_source(&source)?;
        if !source.filename.to_ascii_lowercase().ends_with(".jar") {
            return Err(ContentError::UnsafeFilename);
        }
        files
            .check_write_authority(subject, &path, precondition)
            .map_err(ContentError::File)?;
        files
            .remote_content_authority(subject, &path)
            .map_err(ContentError::File)?;
        let bytes = self.download_source(&source, &mut url, deadline)?;
        let _admission = files
            .remote_effect_admission()
            .map_err(ContentError::File)?;
        files
            .write(subject, &path, precondition, &bytes)
            .map_err(ContentError::File)
    }
    fn download_source(
        &self,
        source: &ResolvedSource,
        url: &mut Url,
        deadline: Option<Instant>,
    ) -> Result<Vec<u8>> {
        download_verified(source, self.http, url, deadline)
    }
}

/// Shared bounded provider download for protected pack staging and live content.
pub fn fetch_verified_source(
    source: &ResolvedSource,
    http: &dyn DownloadHttp,
    deadline: Instant,
) -> Result<Vec<u8>> {
    let _permit = InstallPermit::acquire()?;
    fetch_after_admission(source, http, deadline)
}
fn fetch_after_admission(
    source: &ResolvedSource,
    http: &dyn DownloadHttp,
    deadline: Instant,
) -> Result<Vec<u8>> {
    let (_, mut url) = validate_source(source)?;
    download_verified(source, http, &mut url, Some(deadline))
}
fn download_verified(
    source: &ResolvedSource,
    http: &dyn DownloadHttp,
    url: &mut Url,
    deadline: Option<Instant>,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    download_verified_into(
        source,
        http,
        url,
        deadline,
        &mut bytes,
        Some(MAX_WRITE_BYTES as u64),
    )?;
    Ok(bytes)
}
/// Pack workers wait within the same process-wide two-transfer budget.
pub(crate) fn fetch_verified_source_into(
    source: &ResolvedSource,
    http: &dyn DownloadHttp,
    deadline: Instant,
    output: &mut dyn std::io::Write,
) -> Result<()> {
    let _permit = INSTALLS.acquire(Some(deadline))?;
    let (_, mut url) = validate_source(source)?;
    download_verified_into(source, http, &mut url, Some(deadline), output, None)
}

pub(crate) fn verify_source_reader(source: &ResolvedSource, reader: impl Read) -> Result<()> {
    source.expected_hash.validate()?;
    verified_copy(reader, &source.expected_hash, &mut std::io::sink(), None).map(|_| ())
}

fn download_verified_into(
    source: &ResolvedSource,
    http: &dyn DownloadHttp,
    url: &mut Url,
    deadline: Option<Instant>,
    output: &mut dyn std::io::Write,
    maximum: Option<u64>,
) -> Result<()> {
    for redirects in 0..=MAX_REDIRECTS {
        // Never let the HTTP adapter follow redirects implicitly.
        validate_download_url(source.provider, url)?;
        let response = match deadline {
            Some(d) => http.get_with_deadline(url, d)?,
            None => http.get(url)?,
        };
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            if redirects == MAX_REDIRECTS {
                return Err(ContentError::TooManyRedirects);
            }
            let location = response.location.ok_or(ContentError::DownloadFailed)?;
            if location.len() > MAX_URL_BYTES {
                return Err(ContentError::DisallowedUrl);
            }
            *url = url
                .join(&location)
                .map_err(|_| ContentError::DisallowedUrl)?;
            validate_download_url(source.provider, url)?;
            continue;
        }
        if source.provider == Provider::Curseforge && matches!(response.status, 403 | 404) {
            return Err(ContentError::DownloadUnavailable {
                filename: source.filename.clone(),
            });
        }
        if response.status != 200
            || response
                .content_encoding
                .as_deref()
                .is_some_and(|e| !e.eq_ignore_ascii_case("identity"))
        {
            return Err(ContentError::DownloadFailed);
        }
        if response
            .content_length
            .is_some_and(|length| maximum.is_some_and(|maximum| length > maximum))
        {
            return Err(ContentError::TooLarge);
        }
        let body: Box<dyn Read> = match deadline {
            Some(d) => Box::new(DeadlineRead {
                reader: response.body,
                deadline: d,
            }),
            None => response.body,
        };
        let count = verified_copy(body, &source.expected_hash, output, maximum)?;
        if response
            .content_length
            .is_some_and(|length| length != count)
        {
            return Err(ContentError::DownloadFailed);
        }
        if let Some(deadline) = deadline {
            deadline_check(deadline)?;
        }
        return Ok(());
    }
    Err(ContentError::TooManyRedirects)
}

fn validate_id(text: &str) -> Result<()> {
    if text.is_empty()
        || text.len() > 128
        || !text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(ContentError::InvalidRequest);
    }
    Ok(())
}
fn validate_source(source: &ResolvedSource) -> Result<(RelativePath, Url)> {
    source.expected_hash.validate()?;
    let path = validate_filename(&source.filename)?;
    if source.download_url.len() > MAX_URL_BYTES {
        return Err(ContentError::DisallowedUrl);
    }
    let url = Url::parse(&source.download_url).map_err(|_| ContentError::DisallowedUrl)?;
    validate_download_url(source.provider, &url)?;
    Ok((path, url))
}
pub(crate) fn provider_storage_name(filename: &str) -> Result<String> {
    if filename.contains('/') || filename.contains('\\') || filename == "." || filename == ".."
        || filename.as_bytes().get(1) == Some(&b':') || filename.is_empty() {
        return Err(ContentError::UnsafeFilename);
    }
    if RelativePath::parse(&format!("mods/{filename}")).is_ok() && vesta_protocol::pack::catalogue::RelativePath::parse(&format!("mods/{filename}")).is_ok() { return Ok(filename.into()); }
    // The alias must pass both OS storage and catalogue validators. Percent escapes
    // are forbidden by the catalogue, even when the OS accepts them.
    let extension = filename.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).filter(|ext| matches!(ext.as_str(), "jar" | "zip" | "mrpack")).ok_or(ContentError::UnsafeFilename)?;
    let name = format!("provider-{}.{}", hex::encode(sha2::Sha256::digest(filename.as_bytes())), extension);
    vesta_protocol::pack::catalogue::RelativePath::parse(&format!("mods/{name}")).map_err(|_| ContentError::UnsafeFilename)?;
    Ok(name)
}
fn validate_filename(filename: &str) -> Result<RelativePath> {
    // Provider archives use private staging; their display name is never a filesystem path.
    if filename.to_ascii_lowercase().ends_with(".zip") || filename.to_ascii_lowercase().ends_with(".mrpack") {
        if filename.contains('/') || filename.contains('\\')
            || filename.as_bytes().get(1) == Some(&b':') {
            return Err(ContentError::UnsafeFilename);
        }
        return RelativePath::parse("mods/provider-archive.zip").map_err(|_| ContentError::UnsafeFilename);
    }
    // A provider filename is a leaf, not a provider-controlled relative path.
    if filename.contains('/')
        || filename.contains('\\')
        || !(filename.to_ascii_lowercase().ends_with(".jar")
            || filename.to_ascii_lowercase().ends_with(".zip")
            || filename.to_ascii_lowercase().ends_with(".mrpack"))
    {
        return Err(ContentError::UnsafeFilename);
    }
    RelativePath::parse(&format!("mods/{}", provider_storage_name(filename)?)).map_err(|_| ContentError::UnsafeFilename)
}
fn validate_download_url(_provider: Provider, url: &Url) -> Result<()> {
    // API-resolved sources are trusted; CDN migrations are not safety failures.
    if url.scheme() != "https" || url.host_str().is_none()
        || !url.username().is_empty() || url.password().is_some()
    { return Err(ContentError::DisallowedUrl); }
    Ok(())
}

fn verified_copy(
    mut reader: impl Read,
    expected: &ExpectedHash,
    output: &mut dyn std::io::Write,
    maximum: Option<u64>,
) -> Result<u64> {
    let mut sha512 = Sha512::new();
    let mut sha1 = Sha1::new();
    let mut total = 0u64;
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        // Probe no more than one byte beyond the cap, even for an infinite peer.
        let remaining = maximum.map_or(buffer.len(), |maximum| {
            maximum
                .saturating_add(1)
                .saturating_sub(total)
                .min(buffer.len() as u64) as usize
        });
        let count = match reader.read(&mut buffer[..remaining]) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(ContentError::DownloadFailed),
        };
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or(ContentError::TooLarge)?;
        if maximum.is_some_and(|maximum| total > maximum) {
            return Err(ContentError::TooLarge);
        }
        match expected {
            ExpectedHash::Sha512(_) => sha512.update(&buffer[..count]),
            ExpectedHash::Sha1(_) => sha1.update(&buffer[..count]),
        }
        output.write_all(&buffer[..count]).map_err(|error| {
            ContentError::File(if error.kind() == std::io::ErrorKind::StorageFull {
                FileError::InsufficientSpace
            } else {
                FileError::Unavailable
            })
        })?;
    }
    let (actual, expected) = match expected {
        ExpectedHash::Sha512(expected) => (hex::encode(sha512.finalize()), expected),
        ExpectedHash::Sha1(expected) => (hex::encode(sha1.finalize()), expected),
    };
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(ContentError::HashMismatch);
    }
    Ok(total)
}

#[derive(Debug, Eq, PartialEq)]
pub enum ContentError {
    ProviderNotConfigured,
    /// The provider's CDN refused or no longer serves this file.
    DownloadUnavailable {
        filename: String,
    },
    ModsUnsupported,
    PluginsUnsupported,
    InvalidRequest,
    InvalidMetadata,
    UnknownSource,
    UnsafeFilename,
    DisallowedUrl,
    TooManyRedirects,
    TooLarge,
    HashMismatch,
    DownloadFailed,
    File(FileError),
}
impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Content installation failed")
    }
}
impl Error for ContentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::File(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod trusted_pack_policy_tests {
    use super::*;
    #[test]
    fn provider_archive_names_and_https_hosts_are_trusted() {
        for name in ["Cobblemon 3D Pok\u{00e9} Rods 1.0.zip", "Cobblemon 3D Pok? Rods 1.0.zip"] {
            let stored = provider_storage_name(name).unwrap();
            assert!(RelativePath::parse(&format!("mods/{stored}")).is_ok());
            assert!(vesta_protocol::pack::catalogue::RelativePath::parse(&format!("mods/{stored}")).is_ok());
        }
        for name in ["COBBLEVERSE 1.7.42.zip", "Pok?mon: pack.zip"] { assert!(validate_filename(name).is_ok()); }
        for name in ["../pack.zip", "/pack.zip", "C:pack.zip", "nested\\pack.zip"] { assert!(validate_filename(name).is_err()); }
        assert!(validate_download_url(Provider::Curseforge, &Url::parse("https://new-cdn.example/pack.zip").unwrap()).is_ok());
        for url in ["http://cdn.example/pack.zip", "https://user:secret@cdn.example/pack.zip"] { assert!(validate_download_url(Provider::Curseforge, &Url::parse(url).unwrap()).is_err()); }
    }
}
