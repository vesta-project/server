//! Byte-bounded draft-v1 server feature discovery, without transports or action dispatch.
//!
//! Unknown valid feature IDs and reasons remain inert descriptive data. Neither a
//! supported feature nor a consistent server identity is an authorization check.

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeSeq, SerializeStruct};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::marker::PhantomData;

pub const PROTOCOL: &str = "vesta.server-capabilities";
pub const VERSION: u16 = 1;
pub const MAX_BYTES: usize = 16_384;

const MAX_VERSIONS: usize = 8;
const MAX_FEATURES: usize = 64;
const MAX_ID_CHARS: usize = 64;
const INVALID_JSON: &str = "invalid server feature protocol message";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCode {
    InvalidMessage,
    MessageTooLarge,
    UnsupportedProtocol,
    UnexpectedResponse,
}

impl ErrorCode {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::InvalidMessage => "invalid_message",
            Self::MessageTooLarge => "message_too_large",
            Self::UnsupportedProtocol => "unsupported_protocol",
            Self::UnexpectedResponse => "unexpected_response",
        }
    }

    fn from_wire(bytes: &[u8]) -> Result<Self, ProtocolError> {
        match bytes {
            b"invalid_message" => Ok(Self::InvalidMessage),
            b"message_too_large" => Ok(Self::MessageTooLarge),
            b"unsupported_protocol" => Ok(Self::UnsupportedProtocol),
            b"unexpected_response" => Ok(Self::UnexpectedResponse),
            _ => Err(invalid()),
        }
    }
}

/// A fixed input-free error, with no underlying parser or input-bearing cause.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolError {
    code: ErrorCode,
}

impl ProtocolError {
    pub const fn code(&self) -> ErrorCode {
        self.code
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Server feature protocol failed: {}", self.code.wire())
    }
}

impl std::error::Error for ProtocolError {}

fn invalid() -> ProtocolError {
    ProtocolError {
        code: ErrorCode::InvalidMessage,
    }
}

fn unsupported() -> ProtocolError {
    ProtocolError {
        code: ErrorCode::UnsupportedProtocol,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Support {
    Supported,
    Unsupported,
}

impl Support {
    fn wire(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
        }
    }

    fn from_wire(bytes: &[u8]) -> Result<Self, ProtocolError> {
        match bytes {
            b"supported" => Ok(Self::Supported),
            b"unsupported" => Ok(Self::Unsupported),
            _ => Err(invalid()),
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct Feature {
    pub id: String,
    pub support: Support,
    pub reason: Option<String>,
}

impl Feature {
    pub fn new(
        id: String,
        support: Support,
        reason: Option<String>,
    ) -> Result<Self, ProtocolError> {
        let feature = Self {
            id,
            support,
            reason,
        };
        validate_feature(&feature)?;
        Ok(feature)
    }
}

impl fmt::Debug for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Feature")
            .field("support", &self.support)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct Manifest {
    pub version: u16,
    pub server_id: String,
    pub features: Vec<Feature>,
}

impl Manifest {
    pub fn new(
        version: u16,
        server_id: String,
        features: Vec<Feature>,
    ) -> Result<Self, ProtocolError> {
        let mut manifest = Self {
            version,
            server_id,
            features,
        };
        validate_manifest(&manifest)?;
        manifest.features.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(manifest)
    }

    /// Exact lookup of a Java-known descriptive ID, not permission to dispatch.
    ///
    /// Public fields can be mutated or constructed without `new`; invalid
    /// manifests fail closed. Unknown IDs remain round-trippable descriptive
    /// data, but cannot enable this known-feature query.
    pub fn supports(&self, id: &str) -> bool {
        validate_manifest(self).is_ok()
            && matches!(
                id,
                "game.start"
                    | "game.stop"
                    | "game.force_kill"
                    | "tasks.replay"
                    | "game.readiness"
                    | "supervisor.continuity"
            )
            && self
                .features
                .iter()
                .any(|feature| feature.id == id && feature.support == Support::Supported)
    }
}

impl fmt::Debug for Manifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Manifest")
            .field("version", &self.version)
            .field("features", &self.features.len())
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryRequest {
    pub versions: Vec<u16>,
}

impl DiscoveryRequest {
    pub fn new(versions: Vec<u16>) -> Result<Self, ProtocolError> {
        let mut request = Self { versions };
        validate_request(&request)?;
        request.versions.sort_by(|a, b| b.cmp(a));
        Ok(request)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Failure {
    pub error: ErrorCode,
}

/// Highest mutually implemented discovery version; independent of task-event versions.
pub fn negotiate(request: &DiscoveryRequest) -> Result<u16, ProtocolError> {
    validate_request(request)?;
    if request.versions.contains(&VERSION) {
        Ok(VERSION)
    } else {
        Err(unsupported())
    }
}

/// Consistency only: obtain `expected_server_id` from an authenticated context.
pub fn check_response(
    request: &DiscoveryRequest,
    expected_server_id: &str,
    manifest: &Manifest,
) -> Result<(), ProtocolError> {
    validate_request(request)?;
    require_server_id(expected_server_id)?;
    validate_manifest(manifest)?;
    if !request.versions.contains(&manifest.version) || expected_server_id != manifest.server_id {
        return Err(ProtocolError {
            code: ErrorCode::UnexpectedResponse,
        });
    }
    Ok(())
}

fn validate_request(request: &DiscoveryRequest) -> Result<(), ProtocolError> {
    if request.versions.is_empty() || request.versions.len() > MAX_VERSIONS {
        return Err(invalid());
    }
    for (index, version) in request.versions.iter().enumerate() {
        if *version == 0 || request.versions[..index].contains(version) {
            return Err(invalid());
        }
    }
    Ok(())
}

fn validate_feature(feature: &Feature) -> Result<(), ProtocolError> {
    if !valid_id(&feature.id) {
        return Err(invalid());
    }
    match (feature.support, feature.reason.as_deref()) {
        (Support::Supported, None) => Ok(()),
        (Support::Unsupported, Some(reason)) if valid_reason(reason) => Ok(()),
        _ => Err(invalid()),
    }
}

fn validate_manifest_version(version: u16) -> Result<(), ProtocolError> {
    if version == 0 {
        Err(invalid())
    } else if version != VERSION {
        Err(unsupported())
    } else {
        Ok(())
    }
}

fn validate_manifest(manifest: &Manifest) -> Result<(), ProtocolError> {
    // The Java constructor checks the selected version before identity/list data.
    validate_manifest_version(manifest.version)?;
    require_server_id(&manifest.server_id)?;
    if manifest.features.len() > MAX_FEATURES {
        return Err(invalid());
    }
    for (index, feature) in manifest.features.iter().enumerate() {
        validate_feature(feature)?;
        if manifest.features[..index]
            .iter()
            .any(|previous| previous.id == feature.id)
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_ID_CHARS || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    let mut separator = false;
    for &byte in &bytes[1..] {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            separator = false;
        } else if matches!(byte, b'.' | b'_' | b'-') && !separator {
            separator = true;
        } else {
            return false;
        }
    }
    !separator
}

fn valid_reason(reason: &str) -> bool {
    let bytes = reason.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_ID_CHARS
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

fn require_server_id(id: &str) -> Result<(), ProtocolError> {
    let bytes = id.as_bytes();
    if bytes.len() != 36 {
        return Err(invalid());
    }
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if *byte != b'-' {
                return Err(invalid());
            }
        } else if !byte.is_ascii_digit() && !matches!(byte, b'a'..=b'f') {
            return Err(invalid());
        }
    }
    Ok(())
}

// Strings are privately retained as WTF-8 until the Java-equivalent validation
// point. In particular, an unsupported manifest version precedes UUID validation
// even when server_id contains an escaped lone UTF-16 surrogate. Valid public
// strings are ASCII, so no WTF-8 escapes can reach the public data model.
struct JsonString(Vec<u8>);

impl JsonString {
    fn into_string(self) -> Result<String, ProtocolError> {
        String::from_utf8(self.0).map_err(|_| invalid())
    }
}

impl<'de> Deserialize<'de> for JsonString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StringVisitor;

        impl Visitor<'_> for StringVisitor {
            type Value = JsonString;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded JSON string")
            }

            fn visit_bytes<E: de::Error>(self, value: &[u8]) -> Result<Self::Value, E> {
                // UTF-8/WTF-8 leading bytes count Java UTF-16 code units:
                // non-BMP scalars occupy two units, lone surrogates occupy one.
                let units: usize = value
                    .iter()
                    .map(|byte| match byte {
                        0x00..=0x7f | 0xc0..=0xef => 1,
                        0xf0..=0xf4 => 2,
                        _ => 0,
                    })
                    .sum();
                if units > MAX_ID_CHARS {
                    return Err(E::custom(INVALID_JSON));
                }
                Ok(JsonString(value.to_vec()))
            }
        }

        // serde_json 1.0.149's byte-string visitor accepts lone surrogates, but
        // also relaxes raw control-character checking; read() restores the latter.
        deserializer.deserialize_bytes(StringVisitor)
    }
}

struct WireVersion(u16);

impl<'de> Deserialize<'de> for WireVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct VersionVisitor;

        impl Visitor<'_> for VersionVisitor {
            type Value = WireVersion;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an integer JSON version from 1 through 65535")
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                if value == 0 || value > u64::from(u16::MAX) {
                    return Err(E::custom(INVALID_JSON));
                }
                Ok(WireVersion(value as u16))
            }
        }

        // Deliberately no signed/float/string/map visitors. In particular, 1.0
        // and 1e0 must not be coerced into version 1. JSON's number parser also
        // rejects leading zeroes and plus signs before a value can be accepted.
        deserializer.deserialize_any(VersionVisitor)
    }
}

struct BoundedList<T, const MAX: usize>(Vec<T>);

struct RejectExtra;

impl<'de> DeserializeSeed<'de> for RejectExtra {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, _: D) -> Result<(), D::Error> {
        // Do not descend into or materialize an item beyond the schema's cap.
        Err(de::Error::custom(INVALID_JSON))
    }
}

impl<'de, T: Deserialize<'de>, const MAX: usize> Deserialize<'de> for BoundedList<T, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ListVisitor<T, const MAX: usize>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>, const MAX: usize> Visitor<'de> for ListVisitor<T, MAX> {
            type Value = BoundedList<T, MAX>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded JSON array")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while values.len() < MAX {
                    match seq.next_element()? {
                        Some(value) => values.push(value),
                        None => return Ok(BoundedList(values)),
                    }
                }
                let _ = seq.next_element_seed(RejectExtra)?;
                Ok(BoundedList(values))
            }
        }

        deserializer.deserialize_seq(ListVisitor::<T, MAX>(PhantomData))
    }
}

// Typed visitors retain duplicate members, reject unknown members immediately,
// and require every member (including nullable feature.reason). No Value tree,
// ignored values, unbounded recursive visitor, or attacker-sized size_hint is used.
macro_rules! strict_object {
    ($name:ident { $($field:ident : $ty:ty),+ $(,)? }) => {
        struct $name {
            $($field: $ty),+
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct ObjectVisitor;

                impl<'de> Visitor<'de> for ObjectVisitor {
                    type Value = $name;

                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("a strict server feature protocol object")
                    }

                    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                        $(let mut $field: Option<$ty> = None;)+
                        while let Some(key) = map.next_key::<String>()? {
                            match key.as_str() {
                                $(stringify!($field) => {
                                    if $field.is_some() {
                                        return Err(de::Error::custom(INVALID_JSON));
                                    }
                                    $field = Some(map.next_value()?);
                                },)+
                                _ => return Err(de::Error::custom(INVALID_JSON)),
                            }
                        }
                        Ok($name {
                            $($field: $field.ok_or_else(|| <A::Error as de::Error>::custom(INVALID_JSON))?),+
                        })
                    }
                }

                deserializer.deserialize_map(ObjectVisitor)
            }
        }
    };
}

strict_object!(WireRequest {
    protocol: JsonString,
    versions: BoundedList<WireVersion, MAX_VERSIONS>,
});

strict_object!(WireManifest {
    protocol: JsonString,
    version: WireVersion,
    server_id: JsonString,
    features: BoundedList<WireFeature, MAX_FEATURES>,
});

strict_object!(WireFailure {
    protocol: JsonString,
    error: JsonString,
});

strict_object!(FeatureFields {
    id: JsonString,
    support: JsonString,
    reason: Option<JsonString>,
});

struct WireFeature(Feature);

impl<'de> Deserialize<'de> for WireFeature {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let fields = FeatureFields::deserialize(deserializer)?;
        // Feature semantics are checked while reading the feature, before root
        // protocol/version/identity validation, just as in the Java codec.
        let feature = (|| {
            Feature::new(
                fields.id.into_string()?,
                Support::from_wire(&fields.support.0)?,
                fields.reason.map(JsonString::into_string).transpose()?,
            )
        })()
        .map_err(|_| <D::Error as de::Error>::custom(INVALID_JSON))?;
        Ok(WireFeature(feature))
    }
}

fn read<'de, T: Deserialize<'de>>(bytes: &'de [u8]) -> Result<T, ProtocolError> {
    // Byte size wins over malformed UTF-8 and all JSON/schema failures.
    if bytes.len() > MAX_BYTES {
        return Err(ProtocolError {
            code: ErrorCode::MessageTooLarge,
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    if text.starts_with('\u{feff}') {
        return Err(invalid());
    }
    reject_raw_string_controls(bytes)?;
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    // Keep serde_json's depth guard enabled. More importantly, schema visitors
    // only descend through root -> features array -> feature object; unexpected
    // containers are rejected without recursively consuming their contents.
    let value = T::deserialize(&mut deserializer).map_err(|_| invalid())?;
    deserializer.end().map_err(|_| invalid())?;
    Ok(value)
}

fn reject_raw_string_controls(bytes: &[u8]) -> Result<(), ProtocolError> {
    let mut in_string = false;
    let mut escaped = false;
    for &byte in bytes {
        if !in_string {
            if byte == b'"' {
                in_string = true;
            }
        } else if escaped {
            escaped = false;
        } else {
            match byte {
                b'"' => in_string = false,
                b'\\' => escaped = true,
                0x00..=0x1f => return Err(invalid()),
                _ => {}
            }
        }
    }
    Ok(())
}

fn require_protocol(protocol: &JsonString) -> Result<(), ProtocolError> {
    if protocol.0 == PROTOCOL.as_bytes() {
        Ok(())
    } else {
        Err(invalid())
    }
}

pub fn decode_request(bytes: &[u8]) -> Result<DiscoveryRequest, ProtocolError> {
    let wire: WireRequest = read(bytes)?;
    require_protocol(&wire.protocol)?;
    DiscoveryRequest::new(
        wire.versions
            .0
            .into_iter()
            .map(|version| version.0)
            .collect(),
    )
}

pub fn decode_manifest(bytes: &[u8]) -> Result<Manifest, ProtocolError> {
    let wire: WireManifest = read(bytes)?;
    require_protocol(&wire.protocol)?;
    // Do not convert a potentially lone-surrogate UUID before the selected
    // version check: Java reports unsupported_protocol first at this point.
    validate_manifest_version(wire.version.0)?;
    Manifest::new(
        wire.version.0,
        wire.server_id.into_string()?,
        wire.features
            .0
            .into_iter()
            .map(|feature| feature.0)
            .collect(),
    )
}

pub fn decode_failure(bytes: &[u8]) -> Result<Failure, ProtocolError> {
    let wire: WireFailure = read(bytes)?;
    require_protocol(&wire.protocol)?;
    Ok(Failure {
        error: ErrorCode::from_wire(&wire.error.0)?,
    })
}

struct RequestOut<'a>(&'a [u16]);

impl Serialize for RequestOut<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut object = serializer.serialize_struct("DiscoveryRequest", 2)?;
        object.serialize_field("protocol", PROTOCOL)?;
        object.serialize_field("versions", self.0)?;
        object.end()
    }
}

struct FeatureOut<'a>(&'a Feature);

impl Serialize for FeatureOut<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut object = serializer.serialize_struct("Feature", 3)?;
        object.serialize_field("id", &self.0.id)?;
        object.serialize_field("support", self.0.support.wire())?;
        object.serialize_field("reason", &self.0.reason)?;
        object.end()
    }
}

struct FeaturesOut<'a>(&'a [&'a Feature]);

impl Serialize for FeaturesOut<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut array = serializer.serialize_seq(Some(self.0.len()))?;
        for feature in self.0 {
            array.serialize_element(&FeatureOut(feature))?;
        }
        array.end()
    }
}

struct ManifestOut<'a> {
    version: u16,
    server_id: &'a str,
    features: FeaturesOut<'a>,
}

impl Serialize for ManifestOut<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut object = serializer.serialize_struct("Manifest", 4)?;
        object.serialize_field("protocol", PROTOCOL)?;
        object.serialize_field("version", &self.version)?;
        object.serialize_field("server_id", self.server_id)?;
        object.serialize_field("features", &self.features)?;
        object.end()
    }
}

struct FailureOut(ErrorCode);

impl Serialize for FailureOut {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut object = serializer.serialize_struct("Failure", 2)?;
        object.serialize_field("protocol", PROTOCOL)?;
        object.serialize_field("error", self.0.wire())?;
        object.end()
    }
}

fn write(value: &impl Serialize) -> Result<Vec<u8>, ProtocolError> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid())?;
    if bytes.len() > MAX_BYTES {
        return Err(ProtocolError {
            code: ErrorCode::MessageTooLarge,
        });
    }
    Ok(bytes)
}

pub fn encode_request(request: &DiscoveryRequest) -> Result<Vec<u8>, ProtocolError> {
    validate_request(request)?;
    let mut versions = request.versions.clone();
    versions.sort_by(|a, b| b.cmp(a));
    write(&RequestOut(&versions))
}

pub fn encode_manifest(manifest: &Manifest) -> Result<Vec<u8>, ProtocolError> {
    validate_manifest(manifest)?;
    let mut features: Vec<&Feature> = manifest.features.iter().collect();
    features.sort_by(|a, b| a.id.cmp(&b.id));
    write(&ManifestOut {
        version: manifest.version,
        server_id: &manifest.server_id,
        features: FeaturesOut(&features),
    })
}

pub fn encode_failure(failure: &Failure) -> Result<Vec<u8>, ProtocolError> {
    // A non-null Rust ErrorCode can only contain one of the four fixed codes.
    write(&FailureOut(failure.error))
}
