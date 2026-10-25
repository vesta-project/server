use super::*;

pub(super) fn base(software: Software) -> Result<&'static str> {
    match software {
        Software::Forge => Ok("https://maven.minecraftforge.net/net/minecraftforge/forge"),
        Software::NeoForge => Ok("https://maven.neoforged.net/releases/net/neoforged/neoforge"),
        _ => Err(InstallError::Unavailable),
    }
}
pub(super) async fn coordinates(
    http: &Transport,
    software: Software,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Vec<String>> {
    let url = format!("{}/maven-metadata.xml", base(software)?);
    let value = http
        .family_metadata(
            &url,
            family(&(base(software)?.to_owned() + "/x/x.jar"))?,
            None,
            job,
            reporter,
        )
        .await?;
    let text = std::str::from_utf8(&value.data).map_err(|_| InstallError::InvalidMetadata)?;
    // A deliberately closed Maven metadata subset: no XML expansion or external entities.
    if text.contains("<!") || text.contains('&') {
        return Err(InstallError::InvalidMetadata);
    }
    let body = text
        .split_once("<versions>")
        .and_then(|(_, v)| v.split_once("</versions>"))
        .ok_or(InstallError::InvalidMetadata)?
        .0;
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let mut tail = body;
    while let Some((_, rest)) = tail.split_once("<version>") {
        let (id, rest) = rest
            .split_once("</version>")
            .ok_or(InstallError::InvalidMetadata)?;
        if !loader_identifier(id)
            || !seen.insert(id.to_owned())
            || result.len() >= job.limits.max_entries
        {
            return Err(InstallError::InvalidMetadata);
        }
        result.push(id.to_owned());
        tail = rest;
    }
    if result.is_empty() {
        return Err(InstallError::Unavailable);
    }
    Ok(result)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    game_versions: List<Game>,
    #[serde(default)]
    version_groups: Vec<Group>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Game {
    id: String,
    loaders: List<Loader>,
    version_group: Option<String>,
}
#[derive(Deserialize)]
struct Group {
    id: String,
    loaders: List<Loader>,
}
#[derive(Deserialize)]
struct Loader {
    id: String,
    stable: bool,
}
async fn neo_games(
    http: &Transport,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<BTreeMap<String, Vec<String>>> {
    let data = http
        .family_metadata(
            "https://launcher-meta.modrinth.com/neo/v0/manifest.json",
            SourceFamily::ModrinthNeoForge,
            None,
            job,
            reporter,
        )
        .await?;
    let value: Manifest = parse(&data.data)?;
    if value.game_versions.0.len() > job.limits.max_entries
        || value.version_groups.len() > job.limits.max_entries
    {
        return Err(InstallError::LimitExceeded);
    }
    let mut groups = BTreeMap::new();
    for group in value.version_groups {
        if groups.insert(group.id, group.loaders.0).is_some() {
            return Err(InstallError::InvalidMetadata);
        }
    }
    let mut result = BTreeMap::new();
    for game in value.game_versions.0 {
        if !identifier(&game.id) {
            return Err(InstallError::InvalidMetadata);
        }
        let mut loaders = game.loaders.0;
        if let Some(group) = game.version_group {
            loaders.extend(
                groups
                    .get(&group)
                    .ok_or(InstallError::InvalidMetadata)?
                    .iter()
                    .map(|l| Loader {
                        id: l.id.clone(),
                        stable: l.stable,
                    }),
            );
        }
        let mut ids = HashSet::new();
        let values = loaders
            .into_iter()
            .filter(|l| l.stable && !l.id.contains('-'))
            .map(|l| {
                if !loader_identifier(&l.id) || !ids.insert(l.id.clone()) {
                    Err(InstallError::InvalidMetadata)
                } else {
                    Ok(l.id)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        if result.insert(game.id, values).is_some() {
            return Err(InstallError::InvalidMetadata);
        }
    }
    Ok(result)
}
fn newer(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |s: &str| {
        s.split(['.', '-'])
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parts(b).cmp(&parts(a)).then_with(|| b.cmp(a))
}
pub(crate) async fn versions(
    http: &Transport,
    software: Software,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Vec<String>> {
    let mut values = if software == Software::NeoForge {
        neo_games(http, job, reporter)
            .await?
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, _)| k)
            .collect::<Vec<_>>()
    } else {
        coordinates(http, software, job, reporter)
            .await?
            .iter()
            .filter_map(|v| v.split_once('-').map(|(mc, _)| mc.to_owned()))
            .collect()
    };
    values.retain(|v| {
        version_order(v).is_some_and(|v| {
            v >= if software == Software::Forge {
                vec![1, 7]
            } else {
                vec![1, 17]
            }
        })
    });
    values.sort_by(|a, b| newer(a, b));
    values.dedup();
    Ok(values)
}
pub(super) fn loader_id(software: Software, coordinate: &str, version: &str) -> Result<String> {
    if software == Software::Forge {
        coordinate
            .strip_prefix(&(version.to_owned() + "-"))
            .map(str::to_owned)
            .ok_or(InstallError::InvalidMetadata)
    } else {
        Ok(coordinate.to_owned())
    }
}
pub(crate) async fn releases(
    http: &Transport,
    software: Software,
    version: &str,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Vec<super::super::releases::SoftwareRelease>> {
    let coordinates = coordinates(http, software, job, reporter).await?;
    release_choices(http, software, version, coordinates, job, reporter).await
}
async fn release_choices(
    http: &Transport,
    software: Software,
    version: &str,
    coordinates: Vec<String>,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Vec<super::super::releases::SoftwareRelease>> {
    let allowed = if software == Software::NeoForge {
        Some(
            neo_games(http, job, reporter)
                .await?
                .remove(version)
                .ok_or(InstallError::Unavailable)?,
        )
    } else {
        None
    };
    let mut result = coordinates
        .into_iter()
        .filter(|id| {
            allowed
                .as_ref()
                .map(|v| v.contains(id))
                .unwrap_or_else(|| id.starts_with(&(version.to_owned() + "-")))
        })
        .map(|id| loader_id(software, &id, version))
        .collect::<Result<Vec<_>>>()?;
    result.retain(|id| {
        let numeric = if software == Software::Forge {
            match id.split_once('-') {
                Some((loader, suffix)) if suffix == version => loader,
                Some(_) => return false,
                None => id.as_str(),
            }
        } else {
            id.as_str()
        };
        numeric
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    });
    result.sort_by(|a, b| newer(a, b));
    Ok(result
        .into_iter()
        .map(|id| super::super::releases::SoftwareRelease {
            id: id.clone(),
            build: None,
            loader_version: Some(id),
        })
        .collect())
}
pub(super) async fn select(
    http: &Transport,
    software: Software,
    version: &str,
    wanted: Option<&str>,
    coordinates: Vec<String>,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<String> {
    let options =
        release_choices(http, software, version, coordinates.clone(), job, reporter).await?;
    let selected = wanted
        .or_else(|| options.first().map(|v| v.id.as_str()))
        .ok_or(InstallError::Unavailable)?;
    if !options.iter().any(|v| v.id == selected) {
        return Err(InstallError::Unavailable);
    }
    coordinates
        .into_iter()
        .find(|c| loader_id(software, c, version).is_ok_and(|id| id == selected))
        .ok_or(InstallError::Unavailable)
}
pub(super) async fn mappings(
    http: &Transport,
    version: &str,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
    revisions: &mut Vec<MetadataRevision>,
) -> Result<BundleArtifact> {
    let c = catalogue(http, Software::Vanilla, job, reporter, revisions).await?;
    let entry = c
        .vanilla
        .ok_or(InstallError::InvalidMetadata)?
        .versions
        .0
        .into_iter()
        .find(|v| v.id == version)
        .ok_or(InstallError::Unavailable)?;
    let bytes = http
        .receive(
            Source {
                url: &entry.url,
                software: Software::Vanilla,
            },
            Role::Metadata,
            None,
            job,
            reporter,
            None,
        )
        .await?;
    use sha1::{Digest as _, Sha1};
    if format!("{:x}", Sha1::digest(&bytes.data)) != entry.sha1 {
        return Err(InstallError::ArtifactChanged);
    }
    #[derive(Deserialize)]
    struct Detail {
        downloads: BTreeMap<String, VanillaArtifact>,
    }
    let mut detail: Detail = parse(&bytes.data)?;
    let value = detail
        .downloads
        .remove("server_mappings")
        .ok_or(InstallError::Unavailable)?;
    Ok(BundleArtifact {
        destination: "server-mappings.txt".into(),
        family: family(&value.url)?,
        pin: ArtifactPin {
            url: value.url,
            digest: PublishedDigest::new(DigestAlgorithm::Sha1, value.sha1)?,
            size: Some(value.size),
        },
    })
}
