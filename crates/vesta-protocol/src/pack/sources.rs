//! Backend-owned source descriptions bound to a portable path and actual SHA-256.
//! This registry performs no provider lookup, download, persistence or authorization.
use super::catalogue::{
    self, BoundedFiles, CatalogueError, FileSource, MAX_FILES, MAX_TOTAL_TEXT_BYTES, Platform,
    RelativePath, SCHEMA_VERSION, Sha256, SourceKind,
};
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};
use std::{collections::BTreeMap, fmt};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnownSource {
    path: RelativePath,
    sha256: Sha256,
    source: FileSource,
}
impl KnownSource {
    pub fn new(path: &str, sha256: &str, source: FileSource) -> Result<Self, CatalogueError> {
        if source.kind() != SourceKind::Platform {
            return Err(CatalogueError::InvalidSource);
        }
        Ok(Self {
            path: RelativePath::parse(path)?,
            sha256: Sha256::parse(sha256)?,
            source,
        })
    }
    pub fn path(&self) -> &RelativePath {
        &self.path
    }
}

/// No permissive missing/malformed-sidecar fallback. An empty registry must be
/// explicitly selected by the trusted backend. Changed known files cannot silently
/// fall back to embedding bytes under a stale provider description.
#[derive(Default)]
pub struct SourceRegistry(BTreeMap<RelativePath, KnownSource>);
impl SourceRegistry {
    pub fn new(entries: Vec<KnownSource>) -> Result<Self, CatalogueError> {
        if entries.len() > MAX_FILES {
            return Err(CatalogueError::TooManyFiles);
        }
        catalogue::validate_namespace(entries.iter().map(KnownSource::path), false)?;
        let mut budget = 0;
        for entry in &entries {
            budget += entry.path.as_str().len()
                + 64
                + entry.source.project_id().map_or(0, str::len)
                + entry.source.version_id().map_or(0, str::len)
                + entry.source.url().map_or(0, str::len);
            if budget > MAX_TOTAL_TEXT_BYTES {
                return Err(CatalogueError::BudgetExceeded);
            }
        }
        Ok(Self(
            entries
                .into_iter()
                .map(|entry| (entry.path.clone(), entry))
                .collect(),
        ))
    }

    /// Exact path and content identity, never a case-folded permission or URL grant.
    ///
    /// Case folding is a *fallback*, tried only after the exact key misses. Folding
    /// first — or folding the exact-key branch too — would report `PathCollision` for
    /// an exact match whose content disagrees, and a caller that treated that as
    /// "unrecognized" would embed bytes under a filename the registry already claims.
    pub fn source_for(
        &self,
        path: &RelativePath,
        hash: &Sha256,
    ) -> Result<Option<&FileSource>, CatalogueError> {
        if let Some(entry) = self.0.get(path) {
            return if &entry.sha256 == hash {
                Ok(Some(&entry.source))
            } else {
                Err(CatalogueError::InvalidHash)
            };
        }
        if self
            .0
            .keys()
            .any(|known| known.as_str().eq_ignore_ascii_case(path.as_str()))
        {
            // The registry pins this namespace's content; a different spelling is a
            // collision to refuse, never a licence to embed unverified bytes.
            return Err(CatalogueError::PathCollision);
        }
        Ok(None)
    }

    /// Separate strict Rust schema; not an automatic Java sidecar migration.
    pub fn decode(bytes: &[u8]) -> Result<Self, CatalogueError> {
        let Object(wire): Object<WireRegistry> = catalogue::decode_json(bytes)?;
        if wire.schema_version != SCHEMA_VERSION {
            return Err(CatalogueError::UnsupportedSchema);
        }
        let entries = wire
            .files
            .0
            .into_iter()
            .map(|Object(entry)| {
                KnownSource::new(
                    &entry.path,
                    &entry.sha256,
                    FileSource::platform(
                        entry.platform,
                        &entry.project_id,
                        &entry.version_id,
                        entry.url.as_deref(),
                    )?,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(entries)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
impl fmt::Debug for SourceRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceRegistry")
            .field("entries", &self.len())
            .finish_non_exhaustive()
    }
}

struct Object<T>(T);
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
            type Value = Object<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a source registry object")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                T::deserialize(MapAccessDeserializer::new(map)).map(Object)
            }
        }
        deserializer.deserialize_map(ObjectVisitor(std::marker::PhantomData))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireRegistry {
    schema_version: u16,
    files: BoundedFiles<Object<WireSource>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireSource {
    path: String,
    sha256: String,
    platform: Platform,
    project_id: String,
    version_id: String,
    url: Option<String>,
}
