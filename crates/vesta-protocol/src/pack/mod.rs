//! Data-only Vesta pack index projection. This is not an untrusted-input decoder,
//! archive builder, provider resolver, signature verifier, or authorization boundary.
//! Paths, hashes and URLs are descriptive inputs here, never executable/install policy.

pub mod catalogue;
pub mod join;
pub mod model;
pub mod sources;
pub mod sync;

use model::{
    BuilderFile, BuilderManifest, PackDependencies, PackEnvironment, PackHashes, VestaFileSource,
    VestaIndexFile, VestaIndexV1,
};
use std::{error::Error, fmt};

pub const INDEX_NAME: &str = "vesta.index.json";
pub const FORMAT: &str = "vesta";
pub const FORMAT_VERSION: i32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackProjectionError;

impl fmt::Display for PackProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Pack index projection failed: invalid_manifest")
    }
}

impl Error for PackProjectionError {}

/// Project the current Java builder's index fields without touching the filesystem.
/// Hashes/sizes/references are copied, not verified. This confers no authority to fetch
/// a URL or publish a pack. ZIP and protected-path behavior require a separate host adapter.
pub fn project_index(manifest: &BuilderManifest) -> Result<VestaIndexV1, PackProjectionError> {
    let mut entries: Vec<&BuilderFile> = manifest
        .mods
        .iter()
        .flatten()
        .chain(manifest.overrides.iter().flatten())
        .collect();
    if entries.iter().any(|entry| entry.path.is_none()) {
        return Err(PackProjectionError);
    }
    // Java String.compareTo uses UTF-16 code units, not Rust's scalar ordering.
    entries.sort_by(|left, right| {
        left.path
            .as_deref()
            .unwrap_or_default()
            .encode_utf16()
            .cmp(right.path.as_deref().unwrap_or_default().encode_utf16())
    });
    let mut dependencies = PackDependencies {
        minecraft: manifest.minecraft_version.clone(),
        ..PackDependencies::default()
    };
    if let Some(loader) = &manifest.modloader {
        let version = loader.version.clone().unwrap_or_default();
        // Compiled ASCII identifiers are locale-independent; unknown types stay inert.
        match loader
            .r#type
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("fabric") => dependencies.fabric_loader = Some(version),
            Some("quilt") => dependencies.quilt_loader = Some(version),
            Some("forge") => dependencies.forge = Some(version),
            Some("neoforge") => dependencies.neoforge = Some(version),
            _ => {}
        }
    }
    let files = entries
        .into_iter()
        .map(|entry| {
            let source = match &entry.source {
                Some(source)
                    if source.r#type.as_deref().is_some_and(|kind| {
                        kind.eq_ignore_ascii_case("modrinth")
                            || kind.eq_ignore_ascii_case("curseforge")
                    }) =>
                {
                    let kind = source.r#type.clone().unwrap_or_default();
                    VestaFileSource {
                        platform: source.platform.clone().or_else(|| Some(kind.clone())),
                        r#type: kind,
                        project_id: source.project_id.clone(),
                        version_id: source.version_id.clone(),
                        url: source.url.clone(),
                    }
                }
                _ => VestaFileSource {
                    r#type: "embedded".into(),
                    platform: None,
                    project_id: None,
                    version_id: None,
                    url: None,
                },
            };
            VestaIndexFile {
                path: entry.path.clone().unwrap_or_default(),
                size: entry.size,
                hashes: PackHashes {
                    sha256: entry.sha256.clone(),
                    sha1: entry.sha1.clone(),
                },
                env: PackEnvironment {
                    client: "required".into(),
                    server: "required".into(),
                },
                source,
            }
        })
        .collect();
    Ok(VestaIndexV1 {
        summary: None,
        icon: None,
        join: None, // Only the trusted backend build may attach join configuration.
        format_version: FORMAT_VERSION,
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        revision: manifest.revision,
        minimum_revision: manifest.minimum_revision,
        minecraft_version: manifest.minecraft_version.clone(),
        modloader_type: manifest
            .modloader
            .as_ref()
            .and_then(|loader| loader.r#type.clone()),
        modloader_version: manifest
            .modloader
            .as_ref()
            .and_then(|loader| loader.version.clone()),
        dependencies,
        files,
    })
}
