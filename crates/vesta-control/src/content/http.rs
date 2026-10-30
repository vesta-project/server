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
        Self { agent: agent() }
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
        .into()
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
            agent: job_agent(Duration::from_secs(60)),
        }
    }
    fn request(&self, url: &Url, timeout: Duration) -> Result<HttpResponse> {
        // Defense in depth; provider-specific host validation is before this call.
        if url.scheme() != "https" {
            return Err(ContentError::DisallowedUrl);
        }
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
