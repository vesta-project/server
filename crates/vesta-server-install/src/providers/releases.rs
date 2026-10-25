//! Metadata-only choices, using the same catalogues and limits as planning.
use super::*;

#[derive(Clone, Debug, serde::Serialize)]
pub struct SoftwareRelease {
    pub id: String,
    pub build: Option<u32>,
    pub loader_version: Option<String>,
}

pub(crate) async fn list(
    http: &Transport,
    software: Software,
    version: &str,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Vec<SoftwareRelease>> {
    if matches!(software, Software::Forge | Software::NeoForge) {
        if !identifier(version) { return Err(InstallError::InvalidInput); }
        return super::forge::releases(http, software, version, job, reporter).await;
    }
    if !identifier(version) {
        return Err(InstallError::InvalidInput);
    }
    if matches!(software, Software::Fabric | Software::Quilt) {
        return loaders::releases(http, software, version, job, reporter).await;
    }
    let mut revisions = Vec::new();
    let mut builds = match software {
        Software::Paper | Software::Folia => {
            let url = format!(
                "https://fill.papermc.io/v3/projects/{}/versions/{}",
                software.id(),
                version
            );
            let metadata_value: FillVersionEnvelope =
                metadata(http, &url, software, job, reporter, &mut revisions).await?;
            let ids: HashSet<_> = metadata_value.builds.0.iter().copied().collect();
            if metadata_value.version.id != version
                || ids.len() != metadata_value.builds.0.len()
                || ids.contains(&0)
                || ids.len() > job.limits.max_entries
            {
                return Err(InstallError::InvalidMetadata);
            }
            let values: List<FillBuild> = metadata(
                http,
                &(url + "/builds"),
                software,
                job,
                reporter,
                &mut revisions,
            )
            .await?;
            let mut seen = HashSet::new();
            if values.0.len() > job.limits.max_entries
                || values
                    .0
                    .iter()
                    .any(|b| !ids.contains(&b.id) || !seen.insert(b.id))
            {
                return Err(InstallError::InvalidMetadata);
            }
            values
                .0
                .into_iter()
                .filter(|b| b.channel == "STABLE" && b.downloads.server.is_some())
                .map(|b| b.id)
                .collect::<Vec<_>>()
        }
        Software::Purpur => {
            let value: PurpurVersion = metadata(
                http,
                &format!("https://api.purpurmc.org/v2/purpur/{version}"),
                software,
                job,
                reporter,
                &mut revisions,
            )
            .await?;
            if value.project != "purpur" || value.version != version {
                return Err(InstallError::InvalidMetadata);
            }
            check_list(&value.builds.all.0, job.limits)?;
            value
                .builds
                .all
                .0
                .iter()
                .map(|id| numeric_build(id))
                .collect::<Result<Vec<_>>>()?
        }
        Software::Vanilla => return Ok(Vec::new()),
        _ => return Err(InstallError::Unavailable),
    };
    builds.sort_unstable_by(|a, b| b.cmp(a));
    Ok(builds
        .into_iter()
        .map(|build| SoftwareRelease {
            id: build.to_string(),
            build: Some(build),
            loader_version: None,
        })
        .collect())
}
