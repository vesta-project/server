//! Compact internal plan storage, with read compatibility for existing JSON arrays.
//! These adapters are not used by public request/response DTOs.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeSeq};

enum Blob {
    Encoded(String),
    Legacy(Vec<u8>),
}
impl<'de> Deserialize<'de> for Blob {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Bytes;
        impl<'de> de::Visitor<'de> for Bytes {
            type Value = Blob;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("base64 or legacy plan bytes")
            }
            fn visit_str<E: de::Error>(self, text: &str) -> Result<Blob, E> {
                if text.len() > crate::packs::MAX_ARTIFACT_BYTES.div_ceil(3) * 4 {
                    return Err(E::custom("plan byte limit"));
                }
                Ok(Blob::Encoded(text.to_owned()))
            }
            fn visit_string<E: de::Error>(self, text: String) -> Result<Blob, E> {
                if text.len() > crate::packs::MAX_ARTIFACT_BYTES.div_ceil(3) * 4 {
                    return Err(E::custom("plan byte limit"));
                }
                Ok(Blob::Encoded(text))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Blob, A::Error> {
                // Stream old numeric arrays directly; an untagged enum would
                // first allocate one generic serde value for every byte.
                let mut bytes = Vec::new();
                while let Some(byte) = seq.next_element::<u8>()? {
                    if bytes.len() >= crate::packs::MAX_ARTIFACT_BYTES {
                        return Err(de::Error::custom("plan byte limit"));
                    }
                    bytes.push(byte);
                }
                Ok(Blob::Legacy(bytes))
            }
        }
        deserializer.deserialize_any(Bytes)
    }
}
impl Blob {
    fn decode<E: de::Error>(self, limit: usize) -> Result<Vec<u8>, E> {
        match self {
            Self::Encoded(text) => {
                if text.len() > limit.div_ceil(3) * 4 {
                    return Err(E::custom("plan byte limit"));
                }
                let bytes = STANDARD
                    .decode(text)
                    .map_err(|_| E::custom("invalid plan encoding"))?;
                if bytes.len() > limit {
                    return Err(E::custom("plan byte limit"));
                }
                Ok(bytes)
            }
            Self::Legacy(bytes) if bytes.len() <= limit => Ok(bytes),
            _ => Err(E::custom("plan byte limit")),
        }
    }
}
struct Encoded<'a>(&'a [u8]);
impl Serialize for Encoded<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(self.0))
    }
}
pub fn serialize<S: Serializer>(blobs: &[Vec<u8>], serializer: S) -> Result<S::Ok, S::Error> {
    let mut seq = serializer.serialize_seq(Some(blobs.len()))?;
    for blob in blobs {
        seq.serialize_element(&Encoded(blob))?;
    }
    seq.end()
}
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Vec<u8>>, D::Error> {
    struct Blobs;
    impl<'de> de::Visitor<'de> for Blobs {
        type Value = Vec<Vec<u8>>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded plan blobs")
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut result = Vec::new();
            let mut remaining = super::super::MAX_BLOBS;
            while let Some(blob) = seq.next_element::<Blob>()? {
                if result.len() >= crate::packs::MAX_VESTA_ENTRIES {
                    return Err(de::Error::custom("plan file limit"));
                }
                let bytes =
                    blob.decode::<A::Error>(remaining.min(crate::packs::MAX_PINNED_FILE_BYTES))?;
                remaining -= bytes.len();
                result.push(bytes);
            }
            Ok(result)
        }
    }
    deserializer.deserialize_seq(Blobs)
}
pub mod optional {
    use super::*;
    pub fn serialize<S: Serializer>(
        blob: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match blob {
            Some(bytes) => serializer.serialize_some(&Encoded(bytes)),
            None => serializer.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<Blob>::deserialize(deserializer)?
            .map(|blob| blob.decode(crate::packs::MAX_ARTIFACT_BYTES))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Fixture {
        #[serde(with = "super")]
        bytes: Vec<Vec<u8>>,
        #[serde(default, with = "optional")]
        archive: Option<Vec<u8>>,
    }
    #[test]
    fn old_numeric_plans_and_new_compact_plans_restore_identical_bytes() {
        let old = r#"{"bytes":[[0,255,1],[],[104,105]],"archive":[0,255]}"#;
        let expected = Fixture {
            bytes: vec![vec![0, 255, 1], vec![], b"hi".to_vec()],
            archive: Some(vec![0, 255]),
        };
        assert_eq!(serde_json::from_str::<Fixture>(old).unwrap(), expected);
        let compact = serde_json::to_value(&expected).unwrap();
        assert_eq!(compact["bytes"][0], "AP8B");
        assert_eq!(compact["archive"], "AP8=");
        assert_eq!(
            serde_json::from_value::<Fixture>(compact).unwrap(),
            expected
        );
        assert!(serde_json::from_str::<Fixture>(r#"{"bytes":["!bad"]}"#).is_err());
        assert!(serde_json::from_str::<Fixture>(r#"{"bytes":[[256]]}"#).is_err());
    }
    #[test]
    fn binary_json_size_is_reduced_without_compression_work() {
        let sample = Fixture {
            bytes: vec![vec![255; 1024 * 1024]],
            archive: None,
        };
        let compact = serde_json::to_string(&sample).unwrap();
        let legacy =
            serde_json::to_string(&serde_json::json!({"bytes": sample.bytes,"archive":null}))
                .unwrap();
        assert!(compact.len() * 2 < legacy.len());
        assert_eq!(serde_json::from_str::<Fixture>(&compact).unwrap(), sample);
    }
}
