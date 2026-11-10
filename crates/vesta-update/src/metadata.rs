use base64::Engine as _;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const MAX_METADATA: usize = 16384;
const MAX_LIFETIME: i64 = 30 * 24 * 60 * 60 * 1000;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Trust {
    pub schema: u8,
    pub release_key: String,
    pub target: String,
    pub channel: String,
    pub sequence: u64,
    pub metadata_sha256: Option<String>,
    pub last_seen_ms: i64,
}
impl Trust {
    pub fn new(release_key: &str, channel: &str, now: i64) -> Result<Self, String> {
        if !valid_hash(release_key) || !matches!(channel, "stable" | "nightly") || now < 0 {
            return Err("update_trust_invalid".into());
        }
        Ok(Self {
            schema: 1,
            release_key: release_key.to_ascii_lowercase(),
            target: crate::target()?.into(),
            channel: channel.into(),
            sequence: 0,
            metadata_sha256: None,
            last_seen_ms: now,
        })
    }
    pub fn fingerprint(&self) -> String {
        hex::encode(Sha256::digest(
            hex::decode(&self.release_key).unwrap_or_default(),
        ))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub schema: u8,
    pub purpose: String,
    pub channel: String,
    pub sequence: u64,
    pub version: String,
    pub target: String,
    pub manifest_sha256: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    /// Signed compatibility evidence. Legacy releases may still be checked and
    /// staged, but cannot be activated without this attestation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<ActivationCompatibility>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActivationCompatibility {
    pub protocol: u16,
    pub state_schema_min: u32,
    pub state_schema_max: u32,
    pub supervisor_ipc: u8,
}
impl ActivationCompatibility {
    pub fn validates(&self) -> bool {
        self.protocol > 0
            && self.state_schema_min > 0
            && self.state_schema_max >= self.state_schema_min
            && self.supervisor_ipc > 0
    }
}
pub(crate) fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
pub(crate) fn verify(
    trust: &Trust,
    bytes: &[u8],
    signature: &[u8],
    now: i64,
) -> Result<(Metadata, String), String> {
    if trust.schema != 1
        || !valid_hash(&trust.release_key)
        || trust.target != crate::target()?
        || !matches!(trust.channel.as_str(), "stable" | "nightly")
        || trust.last_seen_ms < 0
    {
        return Err("update_trust_invalid".into());
    }
    if bytes.len() > MAX_METADATA || signature.len() > 256 {
        return Err("update_metadata_invalid".into());
    }
    let signature = base64::engine::general_purpose::STANDARD
        .decode(signature.trim_ascii())
        .map_err(|_| "update_signature_invalid")?;
    let key = hex::decode(&trust.release_key).map_err(|_| "update_trust_invalid")?;
    UnparsedPublicKey::new(&ED25519, &key)
        .verify(bytes, &signature)
        .map_err(|_| "update_signature_invalid")?;
    let m: Metadata = serde_json::from_slice(bytes).map_err(|_| "update_metadata_invalid")?;
    if m.schema != 1
        || m.purpose != "vesta-update-release-v1"
        || m.channel != trust.channel
        || m.target != trust.target
        || m.sequence == 0
        || m.sequence > 9_007_199_254_740_991
        || !valid_hash(&m.manifest_sha256)
        || m.activation.as_ref().is_some_and(|a| !a.validates())
        || m.version.is_empty()
        || m.version.len() > 64
        || !m
            .version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
    {
        return Err("update_metadata_invalid".into());
    }
    if now < trust.last_seen_ms {
        return Err("update_clock_rollback".into());
    }
    if m.issued_at_ms < 0
        || m.expires_at_ms <= m.issued_at_ms
        || m.expires_at_ms.saturating_sub(m.issued_at_ms) > MAX_LIFETIME
        || m.issued_at_ms > now.saturating_add(300_000)
        || now >= m.expires_at_ms
    {
        return Err("update_metadata_expired".into());
    }
    let hash = hex::encode(Sha256::digest(bytes));
    if m.sequence < trust.sequence
        || m.sequence == trust.sequence && trust.metadata_sha256.as_deref() != Some(hash.as_str())
    {
        return Err("update_rollback_refused".into());
    }
    Ok((m, hash))
}
