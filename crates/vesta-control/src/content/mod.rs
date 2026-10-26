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
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use url::Url;

pub type Result<T> = std::result::Result<T, ContentError>;
const MAX_REDIRECTS: usize = 5;
const MAX_URL_BYTES: usize = 8_192;
pub const REMOTE_INSTALL_TIMEOUT: Duration = Duration::from_secs(120);
static INSTALLS: AtomicUsize = AtomicUsize::new(0);
struct InstallPermit;
impl InstallPermit {
    fn acquire() -> Result<Self> {
        INSTALLS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 2).then_some(n + 1)
            })
            .map_err(|_| ContentError::DownloadFailed)?;
        Ok(Self)
    }
}
impl Drop for InstallPermit {
    fn drop(&mut self) {
        INSTALLS.fetch_sub(1, Ordering::AcqRel);
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
    let (_, mut url) = validate_source(source)?;
    download_verified(source, http, &mut url, Some(deadline))
}
fn download_verified(
    source: &ResolvedSource,
    http: &dyn DownloadHttp,
    url: &mut Url,
    deadline: Option<Instant>,
) -> Result<Vec<u8>> {
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
            .is_some_and(|length| length > MAX_WRITE_BYTES as u64)
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
        let bytes = verified_bytes(body, &source.expected_hash)?;
        if let Some(deadline) = deadline {
            deadline_check(deadline)?;
        }
        return Ok(bytes);
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
    // A provider filename is a leaf, not a provider-controlled relative path.
    if source.filename.contains('/')
        || source.filename.contains('\\')
        || !(source.filename.to_ascii_lowercase().ends_with(".jar")
            || source.filename.to_ascii_lowercase().ends_with(".zip")
            || source.filename.to_ascii_lowercase().ends_with(".mrpack"))
    {
        return Err(ContentError::UnsafeFilename);
    }
    let path = RelativePath::parse(&format!("mods/{}", source.filename))
        .map_err(|_| ContentError::UnsafeFilename)?;
    if source.download_url.len() > MAX_URL_BYTES {
        return Err(ContentError::DisallowedUrl);
    }
    let url = Url::parse(&source.download_url).map_err(|_| ContentError::DisallowedUrl)?;
    validate_download_url(source.provider, &url)?;
    Ok((path, url))
}
fn validate_download_url(provider: Provider, url: &Url) -> Result<()> {
    let allowed = match provider {
        Provider::Modrinth => matches!(url.host_str(), Some("cdn.modrinth.com")),
        Provider::Curseforge => matches!(
            url.host_str(),
            Some("edge.forgecdn.net" | "mediafilez.forgecdn.net")
        ),
    };
    if !allowed
        || url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.fragment().is_some()
        || url.as_str().len() > MAX_URL_BYTES
    {
        return Err(ContentError::DisallowedUrl);
    }
    Ok(())
}
fn verified_bytes(mut reader: Box<dyn Read>, expected: &ExpectedHash) -> Result<Vec<u8>> {
    let mut sha512 = Sha512::new();
    let mut sha1 = Sha1::new();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        // Probe no more than one byte beyond the cap, even for an infinite peer.
        let remaining = (MAX_WRITE_BYTES + 1 - bytes.len()).min(buffer.len());
        let count = match reader.read(&mut buffer[..remaining]) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(ContentError::DownloadFailed),
        };
        if count == 0 {
            break;
        }
        if bytes.len() + count > MAX_WRITE_BYTES {
            return Err(ContentError::TooLarge);
        }
        match expected {
            ExpectedHash::Sha512(_) => sha512.update(&buffer[..count]),
            ExpectedHash::Sha1(_) => sha1.update(&buffer[..count]),
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let (actual, expected) = match expected {
        ExpectedHash::Sha512(expected) => (hex::encode(sha512.finalize()), expected),
        ExpectedHash::Sha1(expected) => (hex::encode(sha1.finalize()), expected),
    };
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(ContentError::HashMismatch);
    }
    Ok(bytes)
}

#[derive(Debug, Eq, PartialEq)]
pub enum ContentError {
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
