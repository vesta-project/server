//! Mojang's own launcher metadata (`piston-meta.mojang.com`). Vanilla is
//! resolved through a mirror; Mojang publishes only SHA-1 digests for its
//! server objects, so a host accepts those automatically only after this
//! confirms that every reviewed SHA-1-only object is exactly the `url` and
//! `sha1` Mojang's own version document (itself verified by the SHA-1 in
//! Mojang's manifest) publishes for that Minecraft version.
use super::{List, parse};
use crate::{
    model::*,
    progress::ProgressReporter,
    transfer::{Job, Transport},
};
use serde::Deserialize;
use sha1::{Digest as _, Sha1};

pub(crate) const MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Deserialize)]
struct Manifest {
    versions: List<Entry>,
}
#[derive(Deserialize)]
struct Entry {
    id: String,
    url: String,
    sha1: String,
}
#[derive(Deserialize)]
struct Detail {
    id: String,
    downloads: Downloads,
}
#[derive(Deserialize)]
struct Downloads {
    server: Option<Object>,
    server_mappings: Option<Object>,
}
#[derive(Deserialize)]
struct Object {
    url: String,
    sha1: String,
}

/// `IntegrityConflict` when Mojang does not publish this version or any object
/// differs from Mojang's own data; transport errors when Mojang is unreachable.
pub(crate) async fn verify(
    http: &Transport,
    version: &str,
    objects: &[MojangObject],
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<()> {
    if objects.is_empty() || !identifier(version) {
        return Err(InstallError::InvalidInput);
    }
    let data = http
        .family_metadata(MANIFEST, SourceFamily::MojangMeta, None, job, reporter)
        .await?;
    let manifest: Manifest = parse(&data.data)?;
    let entry = manifest
        .versions
        .0
        .into_iter()
        .find(|e| e.id == version)
        .ok_or(InstallError::IntegrityConflict)?;
    let expected = PublishedDigest::new(DigestAlgorithm::Sha1, entry.sha1)?;
    let data = http
        .family_metadata(&entry.url, SourceFamily::MojangMeta, None, job, reporter)
        .await?;
    if format!("{:x}", Sha1::digest(&data.data)) != expected.hex() {
        return Err(InstallError::IntegrityConflict);
    }
    let detail: Detail = parse(&data.data)?;
    if detail.id != version {
        return Err(InstallError::IntegrityConflict);
    }
    let official: Vec<Object> = [detail.downloads.server, detail.downloads.server_mappings]
        .into_iter()
        .flatten()
        .collect();
    let published = |o: &MojangObject| {
        official
            .iter()
            .any(|x| x.url == o.url && x.sha1.eq_ignore_ascii_case(&o.sha1))
    };
    if !objects.iter().all(published) {
        return Err(InstallError::IntegrityConflict);
    }
    Ok(())
}
