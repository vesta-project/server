//! Fixed HTTPS hasJoined adapter. The supported `unversioned` resolver integration
//! is coupled to the exact reviewed ureq 3.3.0 pin, not a semver-stability claim.
use super::{MinecraftError, MinecraftName, MinecraftUuid, ServerHash};
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, Visitor},
};
use std::{
    collections::BTreeSet,
    fmt,
    io::Read,
    net::ToSocketAddrs,
    time::{Duration, Instant},
};
use ureq::{
    Agent,
    config::Config,
    http::Uri,
    tls::{RootCerts, TlsConfig, TlsProvider},
    unversioned::{
        resolver::{ResolvedSocketAddrs, Resolver},
        transport::{DefaultConnector, NextTimeout},
    },
};

const HOST: &str = "sessionserver.mojang.com";
const ENDPOINT: &str = "https://sessionserver.mojang.com/session/minecraft/hasJoined";
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_JSON_DEPTH: usize = 32;
const MAX_ROOT_FIELDS: usize = 32;
const MAX_KEY_BYTES: usize = 128;

#[derive(Debug)]
pub(super) struct JoinedProfile {
    pub(super) uuid: MinecraftUuid,
    pub(super) name: MinecraftName,
}

/// Private provider seam; only the fixed production adapter is publicly composable.
pub(super) trait SessionProvider: Send + Sync {
    fn verify(
        &self,
        name: &MinecraftName,
        hash: &ServerHash,
    ) -> Result<Option<JoinedProfile>, MinecraftError>;
}

pub(super) struct MojangProvider {
    agent: Agent,
}
impl MojangProvider {
    pub(super) fn new() -> Self {
        let config = Agent::config_builder()
            .https_only(true)
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_recv_body(Some(Duration::from_secs(5)))
            .max_response_header_size(MAX_HEADER_BYTES)
            // No pooled stale-connection retries or idle retained sockets.
            .max_idle_connections(0)
            .max_idle_connections_per_host(0)
            .tls_config(
                TlsConfig::builder()
                    .provider(TlsProvider::Rustls)
                    .root_certs(RootCerts::WebPki)
                    .use_sni(true)
                    .disable_verification(false)
                    .build(),
            )
            .build();
        Self {
            agent: Agent::with_parts(config, DefaultConnector::default(), FixedHostResolver),
        }
    }
}
impl SessionProvider for MojangProvider {
    fn verify(
        &self,
        name: &MinecraftName,
        hash: &ServerHash,
    ) -> Result<Option<JoinedProfile>, MinecraftError> {
        let mut response = self
            .agent
            .get(ENDPOINT)
            .query("username", name.as_str())
            .query("serverId", hash.as_str())
            .header("Accept", "application/json")
            .header("Connection", "close")
            .call()
            .map_err(|_| MinecraftError::unavailable())?;
        decode_http_response(&mut response)
    }
}

fn decode_http_response(
    response: &mut ureq::http::Response<ureq::Body>,
) -> Result<Option<JoinedProfile>, MinecraftError> {
    if let Some(encoding) = response.headers().get("content-encoding")
        && encoding.as_bytes() != b"identity"
    {
        return Err(MinecraftError::unavailable());
    }
    let status = response.status().as_u16();
    let reader = response
        .body_mut()
        .with_config()
        // ureq's reader errors at its limit, including an EOF probe. Allow the
        // decoder's one overflow byte; it still rejects every body over the cap.
        .limit((MAX_BODY_BYTES + 1) as u64)
        .reader();
    decode_response(status, reader)
}

#[derive(Debug)]
struct FixedHostResolver;
impl Resolver for FixedHostResolver {
    fn resolve(
        &self,
        uri: &Uri,
        _config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let authority = uri.authority().map(|value| value.as_str());
        if uri.scheme_str() != Some("https")
            || !matches!(
                authority,
                Some("sessionserver.mojang.com" | "sessionserver.mojang.com:443")
            )
            || uri.path() != "/session/minecraft/hasJoined"
        {
            return Err(ureq::Error::HostNotFound);
        }
        let started = Instant::now();
        if !timeout.after.is_not_happening() && timeout.after.is_zero() {
            return Err(ureq::Error::Timeout(timeout.reason));
        }
        // Deliberately synchronous: default ureq DNS can detach a timed-out worker.
        // OS DNS may overrun the deadline or hang. The application's verification
        // slot remains occupied until lookup actually exits; no hard cancellation
        // or hard-duration bound is claimed. No custom transport or DNS protocol.
        let addresses = (HOST, 443).to_socket_addrs()?;
        if !timeout.after.is_not_happening() && started.elapsed() >= *timeout.after {
            return Err(ureq::Error::Timeout(timeout.reason));
        }
        let mut result = self.empty();
        for address in addresses.take(16) {
            result.push(address);
        }
        if result.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(result)
    }
}

/// One bounded response path, shared by production and controlled in-memory tests.
/// No response body is read for 204 or unexpected status codes.
pub(super) fn decode_response(
    status: u16,
    reader: impl Read,
) -> Result<Option<JoinedProfile>, MinecraftError> {
    if status == 204 {
        return Ok(None);
    }
    if status != 200 {
        return Err(MinecraftError::unavailable());
    }
    let mut bytes = Vec::new();
    reader
        .take((MAX_BODY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| MinecraftError::unavailable())?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(MinecraftError::unavailable());
    }
    std::str::from_utf8(&bytes).map_err(|_| MinecraftError::unavailable())?;
    check_json_depth(&bytes)?;
    let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
    let profile =
        ProfileWire::deserialize(&mut deserializer).map_err(|_| MinecraftError::unavailable())?;
    deserializer
        .end()
        .map_err(|_| MinecraftError::unavailable())?;
    // Mojang's provider wire id is compact, not the broader application's UUID parser.
    if profile.id.len() != 32 || !profile.id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(MinecraftError::unavailable());
    }
    let uuid = MinecraftUuid::parse(&profile.id).map_err(|_| MinecraftError::unavailable())?;
    let name = MinecraftName::parse(&profile.name).map_err(|_| MinecraftError::unavailable())?;
    Ok(Some(JoinedProfile { uuid, name }))
}

/// Bound all nesting, including unrelated properties, before allocating JSON values.
/// This is only a depth precheck; serde still validates all syntax and trailing bytes.
fn check_json_depth(bytes: &[u8]) -> Result<(), MinecraftError> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for &byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return Err(MinecraftError::unavailable());
                }
            }
            b'}' | b']' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(MinecraftError::unavailable)?;
            }
            _ => {}
        }
    }
    Ok(())
}

struct ProfileWire {
    id: String,
    name: String,
}
impl<'de> Deserialize<'de> for ProfileWire {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ProfileVisitor;
        impl<'de> Visitor<'de> for ProfileVisitor {
            type Value = ProfileWire;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a bounded profile object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut fields = BTreeSet::new();
                let mut id = None;
                let mut name = None;
                while let Some(key) = map.next_key::<String>()? {
                    if fields.len() >= MAX_ROOT_FIELDS
                        || key.len() > MAX_KEY_BYTES
                        || !fields.insert(key.clone())
                    {
                        return Err(de::Error::custom("invalid profile fields"));
                    }
                    match key.as_str() {
                        "id" => id = Some(map.next_value::<String>()?),
                        "name" => name = Some(map.next_value::<String>()?),
                        _ => {
                            // Value retains serde_json recursion checking; IgnoredAny
                            // can skip its normal recursion guard. These properties
                            // are bounded and never confer authentication or authority.
                            map.next_value::<serde_json::Value>()?;
                        }
                    }
                }
                Ok(ProfileWire {
                    id: id.ok_or_else(|| de::Error::custom("missing profile fields"))?,
                    name: name.ok_or_else(|| de::Error::custom("missing profile fields"))?,
                })
            }
        }
        // Do not accept a positional array as struct deserialization sometimes can.
        deserializer.deserialize_map(ProfileVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(bytes: Vec<u8>) -> ureq::http::Response<ureq::Body> {
        ureq::http::Response::builder()
            .status(200)
            .body(ureq::Body::builder().data(bytes))
            .unwrap()
    }

    #[test]
    fn production_reader_accepts_the_exact_body_bound_but_not_an_overflow() {
        let json = br#"{"id":"11111111222243338444555555555555","name":"PlayerOne"}"#;
        let mut bytes = json.to_vec();
        bytes.resize(MAX_BODY_BYTES, b' ');
        let mut exact = response(bytes.clone());
        let profile = decode_http_response(&mut exact).unwrap().unwrap();
        assert_eq!(profile.name.as_str(), "PlayerOne");
        assert_eq!(
            profile.uuid,
            MinecraftUuid::parse("11111111222243338444555555555555").unwrap()
        );

        bytes.push(b' ');
        let mut overflow = response(bytes);
        assert_eq!(
            decode_http_response(&mut overflow).unwrap_err().code(),
            "minecraft_unavailable"
        );
    }

    #[test]
    fn production_client_configuration_is_closed_without_sending_a_request() {
        let provider = MojangProvider::new();
        let config = provider.agent.config();
        assert!(config.https_only());
        assert!(config.proxy().is_none());
        assert_eq!(config.max_redirects(), 0);
        assert_eq!(config.max_response_header_size(), MAX_HEADER_BYTES);
        assert_eq!(config.max_idle_connections(), 0);
        assert_eq!(config.max_idle_connections_per_host(), 0);
        assert_eq!(config.timeouts().global, Some(Duration::from_secs(10)));
        assert_eq!(config.timeouts().connect, Some(Duration::from_secs(5)));
        assert_eq!(config.timeouts().recv_body, Some(Duration::from_secs(5)));
        assert!(matches!(
            config.tls_config().provider(),
            TlsProvider::Rustls
        ));
        assert!(matches!(
            config.tls_config().root_certs(),
            RootCerts::WebPki
        ));
        assert!(!config.tls_config().disable_verification());
    }
}
