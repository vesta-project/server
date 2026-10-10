//! Provider-resolved, bounded content installs for trusted in-process composition.
//!
//! Requesters supply identifiers, never download locations, digests or paths. Provider
//! clients and the catalogue registry are trusted server-side adapters, not request DTOs.
//! Installs reuse `files.create` / `files.overwrite` (including canonical scopes), and
//! `FileBrowser::write` owns the completion transaction, replacement trash and audit.
//! The mods directory must already exist. No archive extraction or dependency install.
mod http;
mod provider;

pub use http::{DownloadHttp, HttpResponse, UreqDownloadHttp};
pub use provider::UreqProviderClient;

use crate::{
    files::{FileBrowser, FileChange, FileError, MAX_WRITE_BYTES, RelativePath, WritePrecondition},
    identity::Subject,
};
use sha1::Sha1;
use sha2::{Digest, Sha512};
use std::{collections::BTreeMap, error::Error, fmt, io::Read};
use url::Url;

pub type Result<T> = std::result::Result<T, ContentError>;
const MAX_REDIRECTS: usize = 5;
const MAX_URL_BYTES: usize = 8_192;

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
        self.files
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
                let source = self.providers.resolve(*provider, project_id, version_id)?;
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
        self.files
            .check_write_authority(subject, &path, precondition)
            .map_err(ContentError::File)?;
        for redirects in 0..=MAX_REDIRECTS {
            // Never let the HTTP adapter follow redirects implicitly.
            validate_download_url(source.provider, &url)?;
            let response = self.http.get(&url)?;
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                if redirects == MAX_REDIRECTS {
                    return Err(ContentError::TooManyRedirects);
                }
                let location = response.location.ok_or(ContentError::DownloadFailed)?;
                if location.len() > MAX_URL_BYTES {
                    return Err(ContentError::DisallowedUrl);
                }
                url = url
                    .join(&location)
                    .map_err(|_| ContentError::DisallowedUrl)?;
                validate_download_url(source.provider, &url)?;
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
            let bytes = verified_bytes(response.body, &source.expected_hash)?;
            return self
                .files
                .write(subject, &path, precondition, &bytes)
                .map_err(ContentError::File);
        }
        Err(ContentError::TooManyRedirects)
    }
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
        || !source.filename.to_ascii_lowercase().ends_with(".jar")
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
