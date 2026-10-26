//! Data-only types for projecting the Java pack-builder input into a Vesta index.
//!
//! `BuilderManifest` is deliberately only the input subset consumed by the index
//! projection; it is not a complete server manifest and does not represent
//! authorized installation data. Filesystem access, hashing, archive creation,
//! publication, and transport behavior belong outside this module.

use serde::{Deserialize, Serialize};

/// The pack-builder input fields used to construct a Vesta index.
///
/// Optional fields are omitted when serializing, matching Gson's default null
/// handling. `revision` and `minimum_revision` correspond to Java `long` fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuilderManifest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub revision: i64,
    pub minimum_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minecraft_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modloader: Option<BuilderModloader>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mods: Option<Vec<BuilderFile>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<Vec<BuilderFile>>,
}

/// Modloader input copied into the index and used for dependency projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuilderModloader {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A manifest file input to the pack builder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuilderFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<BuilderFileSource>,
}

/// Source metadata as represented by the Java `FileSource` input model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuilderFileSource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Vesta pack index schema version 1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VestaIndexV1 {
    /// Optional pack presentation; icon is an indexed embedded relative path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Optional backend-authored player information, not installation authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join: Option<super::join::JoinDescriptor>,
    pub format_version: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub revision: i64,
    pub minimum_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minecraft_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modloader_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modloader_version: Option<String>,
    pub dependencies: PackDependencies,
    pub files: Vec<VestaIndexFile>,
}

/// Dependency keys emitted by the Java pack builder.
///
/// The Java projection emits `minecraft` followed by at most one recognized
/// loader dependency. Field order mirrors that insertion order; absent values
/// are omitted, leaving an empty JSON object when no dependencies are present.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PackDependencies {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minecraft: Option<String>,
    #[serde(rename = "fabric-loader", skip_serializing_if = "Option::is_none")]
    pub fabric_loader: Option<String>,
    #[serde(rename = "quilt-loader", skip_serializing_if = "Option::is_none")]
    pub quilt_loader: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub neoforge: Option<String>,
}

/// One projected index file. Its source and environment are separate wire types.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VestaIndexFile {
    pub path: String,
    pub size: i64,
    pub hashes: PackHashes,
    pub env: PackEnvironment,
    pub source: VestaFileSource,
}

/// Hash fields emitted by the Java pack builder, in its SHA-256 then SHA-1 order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PackHashes {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
}

/// Client/server environment requirements in a Vesta index file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackEnvironment {
    pub client: String,
    pub server: String,
}

/// Projected source metadata in a Vesta index file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VestaFileSource {
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(rename = "project_id", skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(rename = "version_id", skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}
