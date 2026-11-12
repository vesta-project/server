//! Bounded player-facing join information. Descriptive only: never a provider URL,
//! automatic connection instruction, server identity, or authorization grant.
use serde::{Deserialize, Serialize};
use std::{fmt, net::IpAddr};

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "WireJoin")]
pub struct JoinDescriptor {
    // Legacy Java ServerInfo.required is intentionally not mapped. This descriptor
    // only describes where a player may join; there is no backend-configured advisory
    // requirement in the current model. Inventing a default would convey no useful
    // information and could be mistaken for client enforcement or access authority.
    // Pack compatibility is instead described by the release's minimum_revision;
    // neither that signal nor join information grants authority. A future meaningful
    // advisory requires an explicit backend configuration, not a client-supplied flag.
    address: String,
    port: u16,
    minecraft_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireJoin {
    address: String,
    port: u16,
    minecraft_version: String,
    description: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidJoinDescriptor;
impl fmt::Display for InvalidJoinDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid join descriptor")
    }
}
impl std::error::Error for InvalidJoinDescriptor {}

impl JoinDescriptor {
    pub fn new(
        address: &str,
        port: u16,
        minecraft_version: &str,
        description: Option<&str>,
    ) -> Result<Self, InvalidJoinDescriptor> {
        let dns = address.len() <= 253
            && address.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label.as_bytes()[0].is_ascii_alphanumeric()
                    && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
        if port == 0
            || address.is_empty()
            || !(dns || address.parse::<IpAddr>().is_ok())
            || !valid_text(minecraft_version, 256)
            || description.is_some_and(|text| !valid_text(text, 4096))
        {
            return Err(InvalidJoinDescriptor);
        }
        Ok(Self {
            address: address.to_owned(),
            port,
            minecraft_version: minecraft_version.to_owned(),
            description: description.map(str::to_owned),
        })
    }
    pub fn address(&self) -> &str {
        &self.address
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn minecraft_version(&self) -> &str {
        &self.minecraft_version
    }
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
}
fn valid_text(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text.trim() == text
        && !text.chars().any(char::is_control)
}
impl TryFrom<WireJoin> for JoinDescriptor {
    type Error = InvalidJoinDescriptor;
    fn try_from(wire: WireJoin) -> Result<Self, Self::Error> {
        Self::new(
            &wire.address,
            wire.port,
            &wire.minecraft_version,
            wire.description.as_deref(),
        )
    }
}
impl fmt::Debug for JoinDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JoinDescriptor[<redacted>]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_join_round_trip_and_invalid_addresses() {
        for address in ["play.example.org", "localhost", "127.0.0.1", "::1"] {
            let join =
                JoinDescriptor::new(address, 25565, "1.21.1", Some("Player server")).unwrap();
            let encoded = serde_json::to_vec(&join).unwrap();
            assert_eq!(
                serde_json::from_slice::<JoinDescriptor>(&encoded).unwrap(),
                join
            );
        }
        for address in [
            "",
            "https://example.org",
            "user@example.org",
            "example.org:25565",
            " example.org",
            "example.org.",
            "-bad.org",
            "bad_.org",
            "a/b",
            "a\\b",
        ] {
            assert!(JoinDescriptor::new(address, 25565, "1.21.1", None).is_err());
        }
        assert!(JoinDescriptor::new("example.org", 0, "1.21.1", None).is_err());
        assert!(JoinDescriptor::new("example.org", 1, " 1.21.1", None).is_err());
        assert!(JoinDescriptor::new("example.org", 1, "1.21.1", Some("secret\n")).is_err());
    }
    #[test]
    fn decoding_rejects_unknown_duplicate_and_ambiguous_fields() {
        for input in [
            r#"{"address":"example.org","port":1,"minecraft_version":"1","url":"https://evil.org"}"#,
            r#"{"address":"example.org","port":1,"minecraft_version":"1","required":true}"#,
            r#"{"address":"example.org","address":"evil.org","port":1,"minecraft_version":"1"}"#,
            r#"{"address":"example.org:1","port":1,"minecraft_version":"1"}"#,
            r#"{"address":"example.org","port":0,"minecraft_version":"1"}"#,
        ] {
            assert!(serde_json::from_str::<JoinDescriptor>(input).is_err());
        }
    }
}
