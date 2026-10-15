//! Optional, bounded off-network diagnostics. Probe results never grant authority.
use crate::engine::Receipt;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 8443;
pub fn default_port() -> u16 {
    DEFAULT_PORT
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExternalProbe {
    pub url: String,
    pub consent: bool,
}
impl ExternalProbe {
    pub fn validate(&self) -> Result<(), String> {
        let url = url::Url::parse(&self.url).map_err(|_| "setup_probe_url_invalid")?;
        if !self.consent
            || url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || self.url.len() > 2048
            || self.url.chars().any(char::is_control)
        {
            return Err("setup_probe_consent_or_url_invalid".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalStatus {
    #[default]
    NotChecked,
    Reachable,
    Unreachable,
    Unavailable,
}
impl ExternalStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotChecked => "Not checked",
            Self::Reachable => "Reachable from the external probe",
            Self::Unreachable => "Not reachable from the external probe",
            Self::Unavailable => "External probe unavailable",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeRequest {
    pub schema: u8,
    pub port: u16,
    pub installation_id: String,
    pub tls_sha256: String,
    pub nonce: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeResponse {
    pub schema: u8,
    pub port: u16,
    pub installation_id: String,
    pub tls_sha256: String,
    pub nonce: String,
    pub reachable: bool,
}
pub trait ProbeClient {
    fn check(&self, url: &str, request: &ProbeRequest) -> Result<ProbeResponse, String>;
}
pub struct HttpsProbe;
impl ProbeClient for HttpsProbe {
    fn check(&self, url: &str, request: &ProbeRequest) -> Result<ProbeResponse, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(8)))
            .build()
            .into();
        let body = serde_json::to_vec(request).map_err(|_| "setup_probe_failed")?;
        let mut response = agent
            .post(url)
            .header("Content-Type", "application/json")
            .send(body.as_slice())
            .map_err(|_| "setup_probe_failed")?;
        if response.status() != 200 {
            return Err("setup_probe_failed".into());
        }
        let bytes = response
            .body_mut()
            .with_config()
            .limit(2048)
            .read_to_vec()
            .map_err(|_| "setup_probe_failed")?;
        serde_json::from_slice(&bytes).map_err(|_| "setup_probe_response_invalid".into())
    }
}
pub fn diagnose(receipt: &Receipt, client: &impl ProbeClient) -> ExternalStatus {
    let Some(probe) = &receipt.answers.external_probe else {
        return ExternalStatus::NotChecked;
    };
    if probe.validate().is_err() {
        return ExternalStatus::Unavailable;
    }
    let request = ProbeRequest {
        schema: 1,
        port: receipt.answers.port,
        installation_id: receipt.installation_id.clone(),
        tls_sha256: receipt.tls_sha256.clone(),
        nonce: uuid::Uuid::new_v4().simple().to_string(),
    };
    let Ok(response) = client.check(&probe.url, &request) else {
        return ExternalStatus::Unavailable;
    };
    if response.schema != request.schema
        || response.port != request.port
        || response.installation_id != request.installation_id
        || response.tls_sha256 != request.tls_sha256
        || response.nonce != request.nonce
    {
        return ExternalStatus::Unavailable;
    }
    if response.reachable {
        ExternalStatus::Reachable
    } else {
        ExternalStatus::Unreachable
    }
}
