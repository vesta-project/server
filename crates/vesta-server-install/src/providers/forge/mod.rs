//! Shared server-only Forge family installation. Profiles describe data, not execution authority.
use super::*;
use crate::paths;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
};

mod catalogue;
mod install;
mod legacy;
mod recipe;
pub(crate) use catalogue::{releases, versions};
pub use install::ProcessedReceipt;
pub(crate) use install::{execute, managed_launch, verify};

const MAX_FILES: usize = 1024;
const MAX_PROCESSORS: usize = 32;
const ARCHIVE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ProcessorPlan {
    pub minecraft: String,
    pub loader_version: String,
    pub coordinate: String,
    pub artifacts: Vec<BundleArtifact>,
    #[serde(serialize_with = "embedded_evidence")]
    pub embedded: BTreeMap<String, Vec<u8>>,
    pub steps: Vec<Step>,
    pub data: BTreeMap<String, String>,
    pub launch: Vec<String>,
    pub mappings: Option<BundleArtifact>,
    pub shim_libraries: BTreeMap<String, String>,
    pub disk_cap: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy: Option<legacy::LegacyLaunch>,
}
fn embedded_evidence<S: serde::Serializer>(
    entries: &BTreeMap<String, Vec<u8>>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    let evidence = entries
        .iter()
        .map(|(path, bytes)| {
            (
                path,
                GeneratedFile {
                    path: path.clone(),
                    size: bytes.len() as u64,
                    sha256: sha256(bytes),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    evidence.serialize(serializer)
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Step {
    pub jar: String,
    pub main: String,
    pub classpath: Vec<String>,
    pub args: Vec<String>,
    pub outputs: BTreeMap<String, String>,
}
#[derive(Deserialize)]
struct Profile {
    spec: u32,
    minecraft: String,
    libraries: List<Library>,
    processors: List<Processor>,
    data: BTreeMap<String, Sided>,
}
#[derive(Deserialize)]
struct Library {
    name: String,
    downloads: Downloads,
}
#[derive(Deserialize)]
struct Downloads {
    artifact: Option<Download>,
}
#[derive(Deserialize)]
struct Download {
    path: String,
    url: String,
    sha1: String,
    size: u64,
}
#[derive(Deserialize)]
struct Sided {
    server: String,
}
#[derive(Deserialize)]
struct Processor {
    jar: String,
    #[serde(default)]
    classpath: Vec<String>,
    args: Vec<String>,
    sides: Option<Vec<String>>,
    #[serde(default)]
    outputs: BTreeMap<String, String>,
}

fn relative(value: &str) -> Result<()> {
    paths::relative(value, |_, _| true)
}
fn maven(value: &str) -> Result<String> {
    if value.len() > 256 {
        return Err(InstallError::InvalidMetadata);
    }
    let (coordinate, ext) = value.split_once('@').unwrap_or((value, "jar"));
    if !matches!(ext, "jar" | "zip" | "txt") {
        return Err(InstallError::InvalidMetadata);
    }
    let parts: Vec<_> = coordinate.split(':').collect();
    if !(3..=4).contains(&parts.len())
        || parts.iter().any(|p| {
            p.is_empty()
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
        })
    {
        return Err(InstallError::InvalidMetadata);
    }
    let classifier = parts.get(3).map(|v| format!("-{v}")).unwrap_or_default();
    let path = format!(
        "{}/{}/{}/{}-{}{}.{}",
        parts[0].replace('.', "/"),
        parts[1],
        parts[2],
        parts[1],
        parts[2],
        classifier,
        ext
    );
    relative(&path)?;
    Ok(path)
}
fn family(url: &str) -> Result<SourceFamily> {
    for family in [
        SourceFamily::ForgeMaven,
        SourceFamily::NeoForgeMaven,
        SourceFamily::MinecraftLibraries,
        SourceFamily::MojangObjects,
    ] {
        if crate::transfer::validate_family_url(url, family, Role::Artifact, false).is_ok() {
            return Ok(family);
        }
    }
    Err(InstallError::SourceRefused)
}
fn zip_entries(bytes: &[u8], cap: u64, legacy: bool) -> Result<BTreeMap<String, Vec<u8>>> {
    if bytes.len() as u64 > ARCHIVE_BYTES {
        return Err(InstallError::LimitExceeded);
    }
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| InstallError::InvalidMetadata)?;
    if zip.len() > 20_000 {
        return Err(InstallError::LimitExceeded);
    }
    let mut names = HashSet::new();
    let mut selected = BTreeMap::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|_| InstallError::InvalidMetadata)?;
        let name = entry.name().to_owned();
        if !names.insert(name.to_ascii_lowercase()) || entry.is_symlink() {
            return Err(InstallError::InvalidMetadata);
        }
        // Server processing never needs the client patch or a second in-memory
        // copy of Maven libraries downloaded through the pinned transport.
        if name == "data/client.lzma" {
            continue;
        }
        if name == "install_profile.json"
            || name.starts_with("data/")
            || name.starts_with("maven/") && name.ends_with("-shim.jar")
            || legacy
                && name.starts_with("maven/net/minecraftforge/forge/")
                && name.ends_with(".jar")
            || legacy && name.starts_with("forge-") && name.ends_with("-universal.jar")
        {
            if entry.is_dir() {
                continue;
            }
            relative(&name)?;
            total = total
                .checked_add(entry.size())
                .ok_or(InstallError::LimitExceeded)?;
            if total > cap {
                return Err(InstallError::LimitExceeded);
            }
            let mut value = Vec::new();
            entry
                .take(cap + 1)
                .read_to_end(&mut value)
                .map_err(|_| InstallError::InvalidMetadata)?;
            if value.len() as u64 > cap {
                return Err(InstallError::LimitExceeded);
            }
            selected.insert(name, value);
        }
    }
    Ok(selected)
}

pub(super) async fn resolve(
    http: &Transport,
    software: Software,
    selection: &Selection,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Resolved> {
    if selection.build.is_some() || selection.experimental {
        return Err(InstallError::InvalidInput);
    }
    let version = selection
        .version
        .as_deref()
        .ok_or(InstallError::InvalidInput)?;
    let legacy = version_order(version).is_some_and(|v| v < vec![1, 17]);
    if version_order(version).is_none_or(|v| v < vec![1, 7])
        || legacy && software != Software::Forge
    {
        return Err(InstallError::Unavailable);
    }
    let entries = catalogue::coordinates(http, software, job, reporter).await?;
    let coordinate = catalogue::select(
        http,
        software,
        version,
        selection.loader_version.as_deref(),
        entries,
        job,
        reporter,
    )
    .await?;
    let loader_version = catalogue::loader_id(software, &coordinate, version)?;
    let base = catalogue::base(software)?;
    let artifact_name = if software == Software::Forge {
        "forge"
    } else {
        "neoforge"
    };
    let installer_url = format!("{base}/{coordinate}/{artifact_name}-{coordinate}-installer.jar");
    let source = family(&installer_url)?;
    let sha = http
        .family_metadata(
            &(installer_url.clone() + ".sha1"),
            source,
            Some(256),
            job,
            reporter,
        )
        .await?;
    let digest = PublishedDigest::new(
        DigestAlgorithm::Sha1,
        String::from_utf8(sha.data)
            .map_err(|_| InstallError::InvalidMetadata)?
            .trim()
            .to_owned(),
    )?;
    let archive = http
        .family_metadata(
            &installer_url,
            source,
            Some(ARCHIVE_BYTES.min(job.limits.metadata_bytes)),
            job,
            reporter,
        )
        .await?;
    use sha1::{Digest as _, Sha1};
    if format!("{:x}", Sha1::digest(&archive.data)) != digest.hex {
        return Err(InstallError::ArtifactChanged);
    }
    let embedded = zip_entries(&archive.data, job.limits.metadata_bytes, legacy)?;
    if legacy {
        return legacy::resolve(
            http,
            legacy::Request {
                version: version.into(),
                coordinate,
                loader_version,
                embedded,
                installer: ArtifactPin {
                    url: installer_url.clone(),
                    digest,
                    size: Some(archive.size),
                },
                metadata: MetadataRevision {
                    url: installer_url,
                    observed_sha256: archive.observed_sha256,
                },
            },
            job,
            reporter,
        )
        .await;
    }
    let profile: Profile = parse(
        embedded
            .get("install_profile.json")
            .ok_or(InstallError::InvalidMetadata)?,
    )?;
    if profile.spec != 1
        || profile.minecraft != version
        || profile.processors.0.len() > MAX_PROCESSORS
        || profile.libraries.0.len() > MAX_FILES
        || profile.data.len() > 128
    {
        return Err(InstallError::InvalidMetadata);
    }
    let vanilla = resolve_single(
        http,
        Software::Vanilla,
        &Selection {
            version: Some(version.into()),
            ..Selection::default()
        },
        job,
        reporter,
    )
    .await?;
    let mut metadata = vanilla.metadata;
    metadata.push(MetadataRevision {
        url: installer_url.clone(),
        observed_sha256: archive.observed_sha256,
    });
    let mut artifacts = vec![
        BundleArtifact {
            destination: "minecraft.jar".into(),
            pin: vanilla.artifact.ok_or(InstallError::InvalidMetadata)?,
            family: SourceFamily::MojangObjects,
        },
        BundleArtifact {
            destination: "installer.jar".into(),
            pin: ArtifactPin {
                url: installer_url,
                digest,
                size: Some(archive.size),
            },
            family: source,
        },
    ];
    let mut seen = HashSet::from(["minecraft.jar".to_string(), "installer.jar".to_string()]);
    for lib in profile.libraries.0 {
        let path = maven(&lib.name)?;
        let download = lib
            .downloads
            .artifact
            .ok_or(InstallError::InvalidMetadata)?;
        let destination = format!("libraries/{path}");
        if path != download.path
            || download.size == 0
            || download.size > job.limits.artifact_bytes
            || !seen.insert(destination.to_ascii_lowercase())
        {
            return Err(InstallError::InvalidMetadata);
        }
        let url = if download.url.is_empty() {
            // An embedded library is still downloaded from its canonical official Maven coordinate.
            format!(
                "{}/{path}",
                if software == Software::Forge {
                    "https://maven.minecraftforge.net"
                } else {
                    "https://maven.neoforged.net/releases"
                }
            )
        } else {
            download.url
        };
        artifacts.push(BundleArtifact {
            destination,
            family: family(&url)?,
            pin: ArtifactPin {
                url,
                digest: PublishedDigest::new(DigestAlgorithm::Sha1, download.sha1)?,
                size: Some(download.size),
            },
        });
    }
    let mut plan = recipe::prepare(
        software,
        version,
        &coordinate,
        loader_version,
        recipe::RecipeInput {
            processors: profile.processors.0,
            data: profile.data,
            legacy: None,
        },
        embedded,
        artifacts,
    )?;
    if plan
        .steps
        .iter()
        .any(|s| s.args.iter().any(|a| a == "DOWNLOAD_MOJMAPS"))
    {
        plan.mappings =
            Some(catalogue::mappings(http, version, job, reporter, &mut metadata).await?);
    }
    let total = plan
        .artifacts
        .iter()
        .chain(plan.mappings.iter())
        .try_fold(0u64, |n, a| {
            n.checked_add(a.pin.size.unwrap_or(0))
                .ok_or(InstallError::LimitExceeded)
        })?;
    if total > job.limits.total_bytes {
        return Err(InstallError::LimitExceeded);
    }
    plan.disk_cap = total
        .checked_mul(3)
        .and_then(|n| n.checked_add(128 * 1024 * 1024))
        .ok_or(InstallError::LimitExceeded)?
        .min(job.limits.total_bytes);
    Ok(Resolved {
        version: version.into(),
        build: None,
        artifact: None,
        bundle: None,
        processed: Some(plan),
        java: vanilla.java,
        warnings: vec![Warning::LegacyChecksum, Warning::FirstRunEffects],
        metadata,
    })
}
