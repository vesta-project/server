//! Small outbound JSON delivery using the installer's hardened public transport.
//! The URL is a secret-bearing local configuration value, never logged here.
use crate::{transfer::Transport, Limits, Result, TransportConfig};

#[derive(Clone, Copy, Debug)]
pub struct Response {
    pub status: u16,
    pub retry_after_seconds: Option<u64>,
    /// HTTP-date/oversize/invalid delays are not guessed or shortened.
    pub retry_after_unsupported: bool,
}

/// Performs exactly one attempt. Redirects are returned as status, never
/// followed. A transport failure may be after remote acceptance; callers must
/// retain that ambiguity rather than claiming exactly-once remote delivery.
pub async fn post_public_json(url: &str, body: &[u8], limits: Limits) -> Result<Response> {
    let limits = limits.validate()?;
    Transport::new(TransportConfig::direct())
        .post_json(url, body, limits)
        .await
}
