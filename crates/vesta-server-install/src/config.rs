use crate::{InstallError, Result};
use std::fmt;

/// Immutable trusted host transport choice. No ambient proxy or custom client/origin.
/// Proxy credentials are private and never serialized into a plan or receipt.
#[derive(Clone)]
pub struct TransportConfig {
    proxy: Option<String>,
}
impl TransportConfig {
    pub fn direct() -> Self {
        Self { proxy: None }
    }

    /// Reject invalid configuration rather than silently bypassing the configured proxy.
    pub fn proxy(url: String) -> Result<Self> {
        let trimmed = url.trim();
        let authority_start = trimmed.find("://").ok_or(InstallError::TransportFailed)?;
        let authority = &trimmed[authority_start + 3..];
        if authority.is_empty() || authority.starts_with(['/', '?', '#']) {
            return Err(InstallError::TransportFailed);
        }
        let parsed = url::Url::parse(trimmed).map_err(|_| InstallError::TransportFailed)?;
        if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h")
            || parsed.host_str().is_none()
            || !matches!(parsed.path(), "" | "/")
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(InstallError::TransportFailed);
        }
        reqwest::Proxy::all(trimmed).map_err(|_| InstallError::TransportFailed)?;
        Ok(Self {
            proxy: Some(trimmed.to_owned()),
        })
    }

    pub(crate) fn into_proxy(self) -> Option<String> {
        self.proxy
    }
}
impl fmt::Debug for TransportConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransportConfig")
            .field("proxy_configured", &self.proxy.is_some())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_is_explicit_and_has_no_proxy() {
        assert!(TransportConfig::direct().into_proxy().is_none());
    }

    #[test]
    fn proxy_schemes_credentials_and_trimming_are_preserved_without_debug_disclosure() {
        for scheme in ["http", "https", "socks5", "socks5h"] {
            let url = format!("{scheme}://user:proxy-secret-f98c@localhost:1080/");
            let config = TransportConfig::proxy(format!("  {url}  ")).unwrap();
            let debug = format!("{config:?}");
            assert!(debug.contains("proxy_configured: true"));
            assert!(!debug.contains("proxy-secret-f98c") && !debug.contains("localhost"));
            assert_eq!(config.clone().into_proxy().as_deref(), Some(url.as_str()));
            assert_eq!(config.into_proxy().as_deref(), Some(url.as_str()));
        }
    }

    #[test]
    fn malformed_proxy_never_falls_back_and_errors_disclose_no_inputs() {
        for url in [
            "",
            "  ",
            "localhost:8080",
            "http:///",
            "ftp://proxy-secret-f98c@host",
            "http://host/path",
            "https://host?proxy-secret-f98c",
            "socks5://host#proxy-secret-f98c",
            "http://host:99999",
        ] {
            let error = TransportConfig::proxy(url.to_owned()).unwrap_err();
            assert_eq!(error, InstallError::TransportFailed);
            assert!(!format!("{error:?} {error}").contains("proxy-secret-f98c"));
        }
    }
}
