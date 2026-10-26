use super::{
    model::*,
    transfer::{Job, Role, Source, Transport},
};
use crate::progress::ProgressReporter;
use serde::{
    de::{MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    marker::PhantomData,
};

mod buildtools;
pub(crate) mod forge;
pub(super) mod loaders;
pub(crate) mod releases;

const MAX_ENTRIES: usize = 10_000;

// Hard limits are enforced by visitors, before collecting an arbitrary catalogue.
pub(crate) struct List<T>(pub(crate) Vec<T>);
impl<T> Default for List<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for List<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = List<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("bounded list")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(v) = a.next_element()? {
                    if out.len() == MAX_ENTRIES {
                        return Err(serde::de::Error::custom("list limit"));
                    }
                    out.push(v);
                }
                Ok(List(out))
            }
        }
        d.deserialize_seq(V(PhantomData))
    }
}
struct Map<T>(BTreeMap<String, T>);
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Map<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Map<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("bounded map")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut out = BTreeMap::new();
                while let Some(k) = a.next_key::<String>()? {
                    if out.len() == MAX_ENTRIES || !identifier(&k) || out.contains_key(&k) {
                        return Err(serde::de::Error::custom("map limit"));
                    }
                    out.insert(k, a.next_value()?);
                }
                Ok(Map(out))
            }
        }
        d.deserialize_map(V(PhantomData))
    }
}
fn parse<T: for<'de> Deserialize<'de>>(data: &[u8]) -> Result<T> {
    std::str::from_utf8(data).map_err(|_| InstallError::InvalidMetadata)?;
    serde_json::from_slice(data).map_err(|_| InstallError::InvalidMetadata)
}
fn check_list(values: &[String], limits: Limits) -> Result<()> {
    if values.is_empty() || values.len() > limits.max_entries {
        return Err(InstallError::InvalidMetadata);
    }
    let mut seen = HashSet::new();
    if values.iter().any(|v| !identifier(v) || !seen.insert(v)) {
        return Err(InstallError::InvalidMetadata);
    }
    Ok(())
}
fn major(n: u32, evidence: &str) -> Result<JavaRequirement> {
    if !(8..=99).contains(&n) {
        return Err(InstallError::RuntimeUnknown);
    }
    Ok(JavaRequirement {
        major: n,
        evidence: evidence.into(),
    })
}

#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub version: Option<String>,
    /// Exact catalogue loader identifier; omitted selects the first stable loader.
    pub loader_version: Option<String>,
    pub build: Option<u32>,
    /// Explicit opt-in; never silently substitute experimental builds for stable ones.
    pub experimental: bool,
}

#[derive(Deserialize)]
struct VanillaManifest {
    latest: Latest,
    versions: List<VanillaEntry>,
}
#[derive(Deserialize)]
struct Latest {
    release: String,
}
#[derive(Deserialize)]
struct VanillaEntry {
    id: String,
    url: String,
    sha1: String,
    #[serde(rename = "type")]
    kind: String,
}
#[derive(Deserialize)]
struct VanillaDetail {
    id: String,
    downloads: VanillaDownloads,
    #[serde(rename = "javaVersion")]
    java: Option<JavaVersion>,
}
#[derive(Deserialize)]
struct VanillaDownloads {
    server: Option<VanillaArtifact>,
}
#[derive(Deserialize)]
struct VanillaArtifact {
    url: String,
    sha1: String,
    size: u64,
}
#[derive(Deserialize)]
struct JavaVersion {
    #[serde(rename = "majorVersion")]
    major: u32,
}
#[derive(Deserialize)]
struct FillProject {
    project: Project,
    versions: Map<List<String>>,
}
#[derive(Deserialize)]
struct Project {
    id: String,
}
#[derive(Deserialize)]
struct FillVersionEnvelope {
    version: FillVersion,
    builds: List<u32>,
}
#[derive(Deserialize)]
struct FillVersion {
    id: String,
    java: Option<FillJava>,
    support: Option<Support>,
}
#[derive(Deserialize)]
struct FillJava {
    version: FillJavaVersion,
}
#[derive(Deserialize)]
struct FillJavaVersion {
    minimum: u32,
}
#[derive(Deserialize)]
struct Support {
    status: String,
}
#[derive(Deserialize)]
struct FillBuild {
    id: u32,
    channel: String,
    downloads: FillDownloads,
}
#[derive(Deserialize)]
struct FillDownloads {
    #[serde(rename = "server:default")]
    server: Option<FillArtifact>,
}
#[derive(Deserialize)]
struct FillArtifact {
    name: String,
    url: String,
    size: u64,
    checksums: Checksums,
}
#[derive(Deserialize)]
struct Checksums {
    sha256: String,
}
#[derive(Deserialize)]
struct PurpurProject {
    project: String,
    versions: List<String>,
}
#[derive(Deserialize)]
struct PurpurVersion {
    project: String,
    version: String,
    builds: PurpurBuildIds,
}
#[derive(Deserialize)]
struct PurpurBuildIds {
    all: List<String>,
}
#[derive(Deserialize)]
struct PurpurBuild {
    project: String,
    version: String,
    build: String,
    result: String,
    md5: Option<String>,
}

pub(crate) struct Resolved {
    pub processed: Option<forge::ProcessorPlan>,
    pub version: String,
    pub build: Option<u32>,
    pub artifact: Option<ArtifactPin>,
    pub bundle: Option<DownloadBundle>,
    pub java: JavaRequirement,
    pub warnings: Vec<Warning>,
    pub metadata: Vec<MetadataRevision>,
}
struct Catalogue {
    versions: Vec<String>,
    vanilla: Option<VanillaManifest>,
    latest: Option<String>,
}

async fn metadata<T: for<'de> Deserialize<'de>>(
    http: &Transport,
    url: &str,
    software: Software,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
    revisions: &mut Vec<MetadataRevision>,
) -> Result<T> {
    let data = http
        .receive(
            Source { url, software },
            Role::Metadata,
            None,
            job,
            reporter,
            None,
        )
        .await?;
    revisions.push(MetadataRevision {
        url: url.into(),
        observed_sha256: data.observed_sha256,
    });
    parse(&data.data)
}
async fn catalogue(
    http: &Transport,
    software: Software,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
    revisions: &mut Vec<MetadataRevision>,
) -> Result<Catalogue> {
    let (versions, vanilla, latest) = match software {
        Software::Vanilla => {
            let m: VanillaManifest = metadata(
                http,
                "https://launcher-meta.modrinth.com/minecraft/v0/manifest.json",
                software,
                job,
                reporter,
                revisions,
            )
            .await?;
            // Historical labels containing spaces are not selectable IDs. Validate raw
            // identity/counts before filtering; they must not poison modern releases.
            let mut seen = HashSet::new();
            if m.versions.0.len() > job.limits.max_entries
                || m.versions.0.iter().any(|e| {
                    e.id.is_empty()
                        || e.id.len() > 64
                        || e.id.chars().any(char::is_control)
                        || !seen.insert(&e.id)
                })
            {
                return Err(InstallError::InvalidMetadata);
            }
            let versions = m
                .versions
                .0
                .iter()
                .filter(|v| identifier(&v.id))
                .map(|v| v.id.clone())
                .collect();
            let latest = Some(m.latest.release.clone());
            (versions, Some(m), latest)
        }
        Software::Paper | Software::Folia => {
            let url = format!("https://fill.papermc.io/v3/projects/{}", software.id());
            let m: FillProject = metadata(http, &url, software, job, reporter, revisions).await?;
            if m.project.id != software.id() {
                return Err(InstallError::InvalidMetadata);
            }
            let mut versions = Vec::new();
            for list in m.versions.0.into_values() {
                if versions
                    .len()
                    .checked_add(list.0.len())
                    .is_none_or(|n| n > job.limits.max_entries)
                {
                    return Err(InstallError::InvalidMetadata);
                }
                versions.extend(list.0);
            }
            (versions, None, None)
        }
        Software::Purpur => {
            let m: PurpurProject = metadata(
                http,
                "https://api.purpurmc.org/v2/purpur",
                software,
                job,
                reporter,
                revisions,
            )
            .await?;
            if m.project != "purpur" {
                return Err(InstallError::InvalidMetadata);
            }
            (m.versions.0, None, None)
        }
        _ => return Err(InstallError::Unavailable),
    };
    check_list(&versions, job.limits)?;
    Ok(Catalogue {
        versions,
        vanilla,
        latest,
    })
}

pub(crate) async fn versions(
    http: &Transport,
    software: Software,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Vec<String>> {
    if matches!(software, Software::Bukkit | Software::Spigot) {
        return buildtools::versions(http, job, reporter).await;
    }
    if matches!(software, Software::Forge | Software::NeoForge) {
        return forge::versions(http, software, job, reporter).await;
    }
    if matches!(software, Software::Fabric | Software::Quilt) {
        return loaders::versions(http, software, job, reporter).await;
    }
    Ok(catalogue(http, software, job, reporter, &mut Vec::new())
        .await?
        .versions)
}
pub(crate) fn version_order(s: &str) -> Option<Vec<u32>> {
    s.split('.').map(|p| p.parse::<u32>().ok()).collect()
}
fn select_version(c: &Catalogue, selection: &Selection) -> Result<String> {
    let value = if let Some(v) = &selection.version {
        v.clone()
    } else if let Some(v) = &c.latest {
        v.clone()
    } else {
        c.versions
            .iter()
            .filter_map(|v| version_order(v).map(|key| (key, v)))
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, v)| v.clone())
            .ok_or(InstallError::Unavailable)?
    };
    if !identifier(&value) {
        return Err(InstallError::InvalidInput);
    }
    if !c.versions.contains(&value) {
        return Err(InstallError::Unavailable);
    }
    Ok(value)
}

async fn resolve_single(
    http: &Transport,
    software: Software,
    selection: &Selection,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Resolved> {
    if !software.automatic() {
        return Err(InstallError::Unavailable);
    }
    if selection.version.as_ref().is_some_and(|v| !identifier(v)) || selection.build == Some(0) {
        return Err(InstallError::InvalidInput);
    }
    if selection
        .loader_version
        .as_ref()
        .is_some_and(|v| !loader_identifier(v))
    {
        return Err(InstallError::InvalidInput);
    }
    if selection.loader_version.is_some() {
        return Err(InstallError::InvalidInput);
    }
    let mut revisions = Vec::new();
    let c = catalogue(http, software, job, reporter, &mut revisions).await?;
    let version = select_version(&c, selection)?;
    let mut warnings = vec![Warning::FirstRunEffects];
    let (artifact, java, build) = match software {
        Software::Vanilla => {
            if selection.build.is_some() {
                return Err(InstallError::InvalidInput);
            }
            let m = c.vanilla.ok_or(InstallError::InvalidMetadata)?;
            let entry = m
                .versions
                .0
                .into_iter()
                .find(|e| e.id == version)
                .ok_or(InstallError::InvalidMetadata)?;
            if entry.kind != "release" {
                if !selection.experimental {
                    return Err(InstallError::Unavailable);
                }
                warnings.push(Warning::ExperimentalBuild);
            }
            let expected = PublishedDigest::new(DigestAlgorithm::Sha1, entry.sha1)?;
            let data = http
                .receive(
                    Source {
                        url: &entry.url,
                        software,
                    },
                    Role::Metadata,
                    None,
                    job,
                    reporter,
                    None,
                )
                .await?;
            use sha1::{Digest as _, Sha1};
            if format!("{:x}", Sha1::digest(&data.data)) != expected.hex {
                return Err(InstallError::ArtifactChanged);
            }
            let detail: VanillaDetail = parse(&data.data)?;
            revisions.push(MetadataRevision {
                url: entry.url,
                observed_sha256: data.observed_sha256,
            });
            if detail.id != version {
                return Err(InstallError::InvalidMetadata);
            }
            let a = detail.downloads.server.ok_or(InstallError::Unavailable)?;
            let java = major(
                detail.java.ok_or(InstallError::RuntimeUnknown)?.major,
                "modrinth_mirrored_java_version",
            )?;
            (
                ArtifactPin {
                    url: a.url,
                    digest: PublishedDigest::new(DigestAlgorithm::Sha1, a.sha1)?,
                    size: Some(a.size),
                },
                java,
                None,
            )
        }
        Software::Paper | Software::Folia => {
            let url = format!(
                "https://fill.papermc.io/v3/projects/{}/versions/{}",
                software.id(),
                version
            );
            let v: FillVersionEnvelope =
                metadata(http, &url, software, job, reporter, &mut revisions).await?;
            if v.version.id != version || v.builds.0.len() > job.limits.max_entries {
                return Err(InstallError::InvalidMetadata);
            }
            let java = major(
                v.version
                    .java
                    .ok_or(InstallError::RuntimeUnknown)?
                    .version
                    .minimum,
                "fill_declared_java_minimum",
            )?;
            if v.version
                .support
                .as_ref()
                .is_none_or(|s| s.status != "SUPPORTED")
            {
                warnings.push(Warning::UnsupportedUpstream);
            }
            let ids: HashSet<_> = v.builds.0.iter().copied().collect();
            if ids.len() != v.builds.0.len() || ids.contains(&0) {
                return Err(InstallError::InvalidMetadata);
            }
            let builds: List<FillBuild> = metadata(
                http,
                &(url + "/builds"),
                software,
                job,
                reporter,
                &mut revisions,
            )
            .await?;
            if builds.0.len() > job.limits.max_entries {
                return Err(InstallError::InvalidMetadata);
            }
            let mut seen = HashSet::new();
            if builds
                .0
                .iter()
                .any(|b| b.id == 0 || !seen.insert(b.id) || !ids.contains(&b.id))
            {
                return Err(InstallError::InvalidMetadata);
            }
            let chosen = builds
                .0
                .into_iter()
                .filter(|b| selection.build.is_none_or(|id| b.id == id))
                .filter(|b| {
                    b.downloads.server.is_some()
                        && (b.channel == "STABLE" || selection.experimental)
                })
                .max_by_key(|b| (b.channel == "STABLE", b.id))
                .ok_or(InstallError::Unavailable)?;
            if chosen.channel != "STABLE" {
                warnings.push(Warning::ExperimentalBuild);
            }
            if software == Software::Folia {
                warnings.push(Warning::RegionThreading);
            }
            let a = chosen.downloads.server.ok_or(InstallError::Unavailable)?;
            if !identifier(&a.name) || !a.name.ends_with(".jar") {
                return Err(InstallError::InvalidMetadata);
            }
            let digest = PublishedDigest::new(DigestAlgorithm::Sha256, a.checksums.sha256)?;
            let u = super::transfer::validate_url(&a.url, software, Role::Artifact)?;
            if u.path() != format!("/v1/objects/{}/{}", digest.hex, a.name) {
                return Err(InstallError::SourceRefused);
            }
            (
                ArtifactPin {
                    url: a.url,
                    digest,
                    size: Some(a.size),
                },
                java,
                Some(chosen.id),
            )
        }
        Software::Purpur => {
            let url = format!("https://api.purpurmc.org/v2/purpur/{}", version);
            let v: PurpurVersion =
                metadata(http, &url, software, job, reporter, &mut revisions).await?;
            if v.project != "purpur" || v.version != version {
                return Err(InstallError::InvalidMetadata);
            }
            if v.builds.all.0.is_empty() {
                return Err(InstallError::Unavailable);
            }
            check_list(&v.builds.all.0, job.limits)?;
            let java = purpur_java(&version)?;
            let mut ids = v
                .builds
                .all
                .0
                .iter()
                .map(|s| numeric_build(s))
                .collect::<Result<Vec<_>>>()?;
            if let Some(id) = selection.build {
                if !ids.contains(&id) {
                    return Err(InstallError::Unavailable);
                }
                ids = vec![id];
            } else {
                ids.sort_unstable_by(|a, b| b.cmp(a));
                ids.truncate(5); // A default choice never walks an unbounded build history.
            }
            let mut chosen = None;
            for id in ids {
                let b: PurpurBuild = metadata(
                    http,
                    &format!("{}/{}", url, id),
                    software,
                    job,
                    reporter,
                    &mut revisions,
                )
                .await?;
                if b.project != "purpur" || b.version != version || numeric_build(&b.build)? != id {
                    return Err(InstallError::InvalidMetadata);
                }
                if b.result == "SUCCESS" {
                    chosen = Some((id, b));
                    break;
                }
            }
            let (id, b) = chosen.ok_or(InstallError::Unavailable)?;
            (
                ArtifactPin {
                    url: format!("{}/{}/download", url, id),
                    digest: PublishedDigest::new(
                        DigestAlgorithm::Md5,
                        b.md5.ok_or(InstallError::InvalidMetadata)?,
                    )?,
                    size: None,
                },
                java,
                Some(id),
            )
        }
        _ => return Err(InstallError::Unavailable),
    };
    super::transfer::validate_url(&artifact.url, software, Role::Artifact)?;
    if software == Software::Vanilla {
        let u = url::Url::parse(&artifact.url).map_err(|_| InstallError::SourceRefused)?;
        if u.path() != format!("/v1/objects/{}/server.jar", artifact.digest.hex) {
            return Err(InstallError::SourceRefused);
        }
    }
    if artifact
        .size
        .is_some_and(|n| n == 0 || n > job.limits.artifact_bytes)
    {
        return Err(InstallError::LimitExceeded);
    }
    if artifact.digest.algorithm.legacy() {
        warnings.push(Warning::LegacyChecksum);
    }
    if http.uses_proxy() {
        warnings.push(Warning::ProxyResolution);
    }
    Ok(Resolved {
        processed: None,
        version,
        build,
        artifact: Some(artifact),
        bundle: None,
        java,
        warnings,
        metadata: revisions,
    })
}
fn numeric_build(s: &str) -> Result<u32> {
    if s.is_empty() || s.len() > 10 || s.starts_with('0') || !s.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(InstallError::InvalidMetadata);
    }
    s.parse().map_err(|_| InstallError::InvalidMetadata)
}
pub(crate) fn purpur_java(version: &str) -> Result<JavaRequirement> {
    let parts = version_order(version).ok_or(InstallError::RuntimeUnknown)?;
    let n = if parts >= vec![1, 20] && parts <= vec![1, 21, 11] {
        21
    } else if parts >= vec![26, 1] && parts <= vec![26, 3] {
        25
    } else {
        return Err(InstallError::RuntimeUnknown);
    };
    major(n, "purpur_paper_guide_matrix_2026_10_05")
}

pub(crate) async fn resolve(
    http: &Transport,
    software: Software,
    selection: &Selection,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Resolved> {
    if selection.version.as_ref().is_some_and(|v| !identifier(v))
        || selection
            .loader_version
            .as_ref()
            .is_some_and(|v| !loader_identifier(v))
        || selection.build == Some(0)
    {
        return Err(InstallError::InvalidInput);
    }
    if matches!(software, Software::Forge | Software::NeoForge) {
        return forge::resolve(http, software, selection, job, reporter).await;
    }
    if matches!(software, Software::Fabric | Software::Quilt) {
        loaders::resolve(http, software, selection, job, reporter).await
    } else {
        resolve_single(http, software, selection, job, reporter).await
    }
}
