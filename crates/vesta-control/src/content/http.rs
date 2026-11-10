//! One-hop HTTP seam. Policy and redirects live in the installer, not the adapter.
use super::{ContentError, Result};
use std::{
    io::Read,
    time::{Duration, Instant},
};
use url::Url;

pub struct HttpResponse {
    pub status: u16,
    pub location: Option<String>,
    pub content_length: Option<u64>,
    pub content_encoding: Option<String>,
    pub body: Box<dyn Read>,
}
/// Trusted composition must supply an adapter which performs exactly one GET and
/// does not follow redirects. Bodies must have bounded read timeouts.
pub trait DownloadHttp {
    fn get(&self, url: &Url) -> Result<HttpResponse>;
    fn get_with_deadline(&self, url: &Url, deadline: Instant) -> Result<HttpResponse> {
        super::deadline_check(deadline)?;
        let response = self.get(url)?;
        super::deadline_check(deadline)?;
        Ok(response)
    }
}

pub struct UreqDownloadHttp {
    agent: ureq::Agent,
}
impl Default for UreqDownloadHttp {
    fn default() -> Self {
        Self {
            agent: artifact_agent(Duration::from_secs(60), 0),
        }
    }
}
pub(super) fn agent() -> ureq::Agent {
    agent_with_timeout(Duration::from_secs(60))
}
pub(super) fn agent_with_timeout(timeout: Duration) -> ureq::Agent {
    configured_agent(timeout, 0)
}
/// A pool belongs to one bounded job and is dropped with it, not held globally.
pub(super) fn job_agent(timeout: Duration) -> ureq::Agent {
    configured_agent(timeout, 2)
}
fn configured_agent(timeout: Duration, idle: usize) -> ureq::Agent {
    configuration(timeout, idle).into()
}
fn configuration(timeout: Duration, idle: usize) -> ureq::config::Config {
    ureq::Agent::config_builder()
        .https_only(true)
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_body(Some(Duration::from_secs(10)))
        .max_response_header_size(16 * 1_024)
        .max_idle_connections(idle)
        .max_idle_connections_per_host(idle)
        .build()
}
fn artifact_agent(timeout: Duration, idle: usize) -> ureq::Agent {
    use ureq::unversioned::transport::{Connector, RustlsConnector};
    ureq::Agent::with_parts(configuration(timeout,idle),super::public_transport::PublicConnector.chain(RustlsConnector::default()),super::public_transport::PublicResolver::<ureq::unversioned::resolver::DefaultResolver>::default())
}
impl DownloadHttp for UreqDownloadHttp {
    fn get_with_deadline(&self, url: &Url, deadline: Instant) -> Result<HttpResponse> {
        super::deadline_check(deadline)?;
        let response = self.request(url, deadline.saturating_duration_since(Instant::now()))?;
        super::deadline_check(deadline)?;
        Ok(response)
    }
    fn get(&self, url: &Url) -> Result<HttpResponse> {
        self.request(url, Duration::from_secs(60))
    }
}
impl UreqDownloadHttp {
    pub(crate) fn for_job() -> Self {
        Self {
            agent: artifact_agent(Duration::from_secs(60), 2),
        }
    }
    fn request(&self, url: &Url, timeout: Duration) -> Result<HttpResponse> {
        // The artifact resolver/connector independently enforce public destinations.
        super::validate_download_url(super::Provider::Modrinth, url)?;
        let response = self
            .agent
            .get(url.as_str())
            .header("Accept-Encoding", "identity")
            .config()
            .timeout_global(Some(timeout))
            .build()
            .call()
            .map_err(|_| ContentError::DownloadFailed)?;
        let status = response.status().as_u16();
        let text = |name| -> Result<Option<String>> {
            response
                .headers()
                .get(name)
                .map(|v| {
                    v.to_str()
                        .map(str::to_owned)
                        .map_err(|_| ContentError::DownloadFailed)
                })
                .transpose()
        };
        let location = text("location")?;
        let content_encoding = text("content-encoding")?;
        let content_length = text("content-length")?
            .map(|v| v.parse::<u64>().map_err(|_| ContentError::DownloadFailed))
            .transpose()?;
        Ok(HttpResponse {
            status,
            location,
            content_length,
            content_encoding,
            body: Box::new(response.into_body().into_reader()),
        })
    }
}

/// Bounded ZIP metadata reads; a CDN without ranges cannot trigger a full download.
pub(crate) struct RemoteZip {
    agent: ureq::Agent,
    url: Url,
    size: u64,
    position: u64,
    start: u64,
    cache: Vec<u8>,
    requests: usize,
}
impl RemoteZip {
    pub(crate) fn open(source: &super::ResolvedSource) -> Result<Self> {
        let url = Url::parse(&source.download_url).map_err(|_| ContentError::DisallowedUrl)?;
        super::validate_download_url(source.provider, &url)?;
        let agent = artifact_agent(Duration::from_secs(8), 0);
        // Some provider CDNs advertise ranges but reject suffix ranges (501).
        // Discover the size with a one-byte explicit range, never a full GET.
        let probe = agent
            .get(url.as_str())
            .header("Range", "bytes=0-0")
            .header("Accept-Encoding", "identity")
            .call()
            .map_err(|_| ContentError::DownloadFailed)?;
        let range = probe
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .ok_or(ContentError::DownloadFailed)?;
        let size = range_size(probe.status().as_u16(), range, 0, 0)?;
        let mut byte = Vec::new();
        probe
            .into_body()
            .into_reader()
            .take(2)
            .read_to_end(&mut byte)
            .map_err(|_| ContentError::DownloadFailed)?;
        if byte.len() != 1 {
            return Err(ContentError::DownloadFailed);
        }
        let start = size.saturating_sub(65536);
        let response = agent
            .get(url.as_str())
            .header("Range", &format!("bytes={}-{}", start, size - 1))
            .header("Accept-Encoding", "identity")
            .call()
            .map_err(|_| ContentError::DownloadFailed)?;
        let range = response
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .ok_or(ContentError::DownloadFailed)?;
        if range_size(response.status().as_u16(), range, start, size - 1)? != size {
            return Err(ContentError::DownloadFailed);
        }
        let mut cache = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(65537)
            .read_to_end(&mut cache)
            .map_err(|_| ContentError::DownloadFailed)?;
        if cache.len() as u64 != size.min(65536) {
            return Err(ContentError::DownloadFailed);
        }
        Ok(Self {
            agent,
            url,
            size,
            position: 0,
            start: size - cache.len() as u64,
            cache,
            requests: 2,
        })
    }
}
fn range_size(status: u16, header: &str, start: u64, end: u64) -> Result<u64> {
    let size = header
        .split('/')
        .next_back()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0 && *v <= i64::MAX as u64)
        .ok_or(ContentError::DownloadFailed)?;
    if status != 206
        || end >= size
        || start > end
        || header != format!("bytes {start}-{end}/{size}")
    {
        return Err(ContentError::DownloadFailed);
    }
    Ok(size)
}
impl std::io::Seek for RemoteZip {
    fn seek(&mut self, from: std::io::SeekFrom) -> std::io::Result<u64> {
        let position = match from {
            std::io::SeekFrom::Start(n) => n as i128,
            std::io::SeekFrom::End(n) => self.size as i128 + n as i128,
            std::io::SeekFrom::Current(n) => self.position as i128 + n as i128,
        };
        if position < 0 || position > self.size as i128 {
            return Err(std::io::Error::other("invalid ZIP seek"));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}
impl Read for RemoteZip {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() || self.position == self.size {
            return Ok(0);
        }
        if self.position < self.start || self.position >= self.start + self.cache.len() as u64 {
            self.requests += 1;
            if self.requests > 48 {
                return Err(std::io::Error::other("manifest range budget exceeded"));
            }
            self.start = self.position;
            let end = (self.start + 65535).min(self.size - 1);
            let response = self
                .agent
                .get(self.url.as_str())
                .header("Range", &format!("bytes={}-{}", self.start, end))
                .header("Accept-Encoding", "identity")
                .call()
                .map_err(std::io::Error::other)?;
            if response.status().as_u16() != 206
                || response
                    .headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    != Some(format!("bytes {}-{}/{}", self.start, end, self.size).as_str())
            {
                return Err(std::io::Error::other("invalid ZIP range"));
            }
            self.cache.clear();
            response
                .into_body()
                .into_reader()
                .take(65537)
                .read_to_end(&mut self.cache)?;
            if self.cache.len() as u64 != end - self.start + 1 {
                return Err(std::io::Error::other("short ZIP range"));
            }
        }
        let offset = (self.position - self.start) as usize;
        let n = out.len().min(self.cache.len() - offset);
        out[..n].copy_from_slice(&self.cache[offset..offset + n]);
        self.position += n as u64;
        Ok(n)
    }
}

#[cfg(test)]
mod range_tests {
    use super::*;
    use std::io::{Cursor, Seek, SeekFrom, Write};
    #[test]
    fn explicit_range_metadata_requires_exact_bounds_and_partial_response() {
        assert_eq!(
            range_size(206, "bytes 0-0/104086874", 0, 0).unwrap(),
            104086874
        );
        assert_eq!(
            range_size(
                206,
                "bytes 104021338-104086873/104086874",
                104021338,
                104086873
            )
            .unwrap(),
            104086874
        );
        assert!(range_size(200, "bytes 0-0/10", 0, 0).is_err());
        assert!(range_size(206, "bytes 1-1/10", 0, 0).is_err());
        assert!(range_size(206, "bytes 0-0/0", 0, 0).is_err());
        assert!(range_size(206, "bytes 0-0/18446744073709551615", 0, 0).is_err());
        assert!(range_size(206, "bytes 0-10/10", 0, 10).is_err());
    }
    #[test]
    #[ignore = "real Modrinth CDN range requests; no full archive download"]
    fn homestead_cdn_runtime_manifest_uses_explicit_ranges() {
        let source = super::super::ResolvedSource {
            provider: super::super::Provider::Modrinth,
            filename: "Homestead 2.0.1.mrpack".into(),
            download_url:
                "https://cdn.modrinth.com/data/6HvKwSky/versions/WjiUurYs/Homestead%202.0.1.mrpack"
                    .into(),
            expected_hash: super::super::ExpectedHash::Sha1(
                "554468613c025e1e7148c5fe7c0e802f7ada9aaa".into(),
            ),
        };
        let remote = RemoteZip::open(&source).unwrap();
        let mut zip = zip::ZipArchive::new(remote).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_reader(zip.by_name("modrinth.index.json").unwrap()).unwrap();
        assert_eq!(manifest["game"], "minecraft");
        assert!(
            manifest["dependencies"]["minecraft"]
                .as_str()
                .is_some_and(|version| !version.is_empty())
        );
    }
    #[test]
    fn manifest_inspection_reads_only_the_cached_tail_of_a_large_zip() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        archive.start_file("overrides/large.bin", options).unwrap();
        archive.write_all(&vec![7; 1024 * 1024]).unwrap();
        archive.start_file("manifest.json", options).unwrap();
        archive
            .write_all(br#"{"minecraft":{"version":"1.12.2"}}"#)
            .unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        let start = bytes.len() - 65536;
        let mut remote = RemoteZip {
            agent: agent(),
            url: Url::parse("https://unused.invalid/pack.zip").unwrap(),
            size: bytes.len() as u64,
            position: 0,
            start: start as u64,
            cache: bytes[start..].to_vec(),
            requests: 48,
        };
        assert!(remote.seek(SeekFrom::End(1)).is_err());
        let mut zip = zip::ZipArchive::new(remote).unwrap();
        let document: serde_json::Value =
            serde_json::from_reader(zip.by_name("manifest.json").unwrap()).unwrap();
        assert_eq!(document["minecraft"]["version"], "1.12.2");
        // Any read of the large override would require another network range.
        if let Ok(mut file) = zip.by_name("overrides/large.bin") {
            assert!(file.read(&mut [0; 1]).is_err());
        }
    }
}

#[cfg(test)]
#[path = "public_compat.rs"]
mod public_compat;
