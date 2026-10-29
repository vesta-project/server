//! Read-only discovery. Candidates must still pass whole-pack install review.
use super::*;
use crate::content::{Provider, UreqProviderClient};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerModpackUpdate {
    pub installed: Option<ServerModpackVersion>,
    pub pending: Option<ServerModpackVersion>,
    pub available: Option<InstallSource>,
}

impl PackWorkspace {
    pub fn server_modpack_update(
        &self,
        subject: &Subject,
        minecraft: &str,
        software: Option<&str>,
        providers: &UreqProviderClient,
    ) -> Result<ServerModpackUpdate> {
        let link = self.server_link(subject)?.ok_or(ReleaseError::Conflict)?;
        let mut result = ServerModpackUpdate {
            installed: link.installed,
            pending: link.pending,
            available: None,
        };
        // No candidate can be applied while an earlier update awaits restart.
        if result.pending.is_some() {
            return Ok(result);
        }
        let Some(ServerModpackVersion {
            source:
                InstallSource::Provider {
                    provider,
                    project_id,
                    version_id,
                },
            ..
        }) = &result.installed
        else {
            return Ok(result);
        };
        let platform = match provider.as_str() {
            "modrinth" => Provider::Modrinth,
            "curseforge" => Provider::Curseforge,
            _ => return Err(invalid()),
        };
        let providers = providers
            .with_deadline(std::time::Instant::now() + std::time::Duration::from_secs(20))
            .map_err(|_| super::super::PackError::Unavailable)?;
        let current = providers
            .pack_document(platform, project_id, Some(version_id), false)
            .map_err(|_| super::super::PackError::Unavailable)?;
        let versions = providers
            .pack_document(platform, project_id, None, true)
            .map_err(|_| super::super::PackError::Unavailable)?;
        if let Some(version_id) = newer(
            platform, project_id, version_id, minecraft, software, &current, &versions,
        )? {
            result.available = Some(InstallSource::Provider {
                provider: provider.clone(),
                project_id: project_id.clone(),
                version_id,
            });
        }
        // Refuse stale results if installation changed during metadata requests.
        let now = self.server_link(subject)?.ok_or(ReleaseError::Conflict)?;
        if serde_json::to_value((&now.installed, &now.pending)).map_err(|_| invalid())?
            != serde_json::to_value((&result.installed, &result.pending)).map_err(|_| invalid())?
        {
            return Err(ReleaseError::Conflict);
        }
        Ok(result)
    }
}

fn identity(provider: Provider, v: &Value) -> Option<(String, String, &str)> {
    if provider == Provider::Modrinth {
        Some((
            v["project_id"].as_str()?.into(),
            v["id"].as_str()?.into(),
            v["date_published"].as_str()?,
        ))
    } else {
        Some((
            v["modId"].as_u64()?.to_string(),
            v["id"].as_u64()?.to_string(),
            v["fileDate"].as_str()?,
        ))
    }
}
fn newer(
    provider: Provider,
    project: &str,
    installed: &str,
    minecraft: &str,
    software: Option<&str>,
    current: &Value,
    versions: &Value,
) -> Result<Option<String>> {
    let (bound_project, bound_id, published) = identity(provider, current).ok_or_else(invalid)?;
    if bound_project != project || bound_id != installed || published.is_empty() {
        return Err(invalid());
    }
    let rows = versions.as_array().ok_or_else(invalid)?;
    let mut candidates = Vec::new();
    for v in rows.iter().take(256) {
        let Some((p, id, date)) = identity(provider, v) else {
            continue;
        };
        if p != project || id == installed || date <= published {
            continue;
        }
        let (games, loaders, downloadable) = if provider == Provider::Modrinth {
            (
                &v["game_versions"],
                &v["loaders"],
                v["files"].as_array().is_some_and(|files| {
                    files
                        .iter()
                        .any(|f| f["url"].as_str().is_some_and(|s| !s.is_empty()))
                }),
            )
        } else {
            (
                &v["gameVersions"],
                &v["gameVersions"],
                v["downloadUrl"].as_str().is_some_and(|s| !s.is_empty())
                    && v["isAvailable"] != false,
            )
        };
        let contains = |values: &Value, target: &str| {
            values.as_array().is_some_and(|a| {
                a.iter()
                    .any(|s| s.as_str().is_some_and(|s| s.eq_ignore_ascii_case(target)))
            })
        };
        // Exact loader compatibility; a Fabric pack is not silently a Quilt pack.
        if downloadable
            && contains(games, minecraft)
            && software.is_some_and(|s| contains(loaders, s))
        {
            candidates.push((date, id));
        }
    }
    candidates.sort();
    Ok(candidates.pop().map(|(_, id)| id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn discovery_pins_project_and_filters_game_loader_download_and_age() {
        for provider in [Provider::Modrinth, Provider::Curseforge] {
            let version = |project: &str,
                           id: u64,
                           date: &str,
                           game: &str,
                           loader: &str,
                           available: bool| {
                if provider == Provider::Modrinth {
                    json!({"project_id":project,"id":id.to_string(),"date_published":date,"game_versions":[game],"loaders":[loader],"files":if available {json!([{"url":"https://cdn.modrinth.com/test"}])} else {json!([])}})
                } else {
                    json!({"modId":project.parse::<u64>().unwrap(),"id":id,"fileDate":date,"gameVersions":[game,loader],"downloadUrl":if available {json!("https://edge.forgecdn.net/test")} else {Value::Null},"isAvailable":available})
                }
            };
            let current = version("1", 10, "2026-01-01T00:00:00Z", "1.21.1", "Fabric", true);
            let rows = json!([
                version("1", 11, "2026-02-01T00:00:00Z", "1.21.1", "Fabric", true),
                version("2", 12, "2026-03-01T00:00:00Z", "1.21.1", "Fabric", true),
                version("1", 13, "2026-03-01T00:00:00Z", "1.22", "Fabric", true),
                version("1", 14, "2026-03-01T00:00:00Z", "1.21.1", "Forge", true),
                version("1", 15, "2026-03-01T00:00:00Z", "1.21.1", "Fabric", false),
                version("1", 16, "2025-01-01T00:00:00Z", "1.21.1", "Fabric", true)
            ]);
            assert_eq!(
                newer(
                    provider,
                    "1",
                    "10",
                    "1.21.1",
                    Some("fabric"),
                    &current,
                    &rows
                )
                .unwrap(),
                Some("11".into())
            );
            assert!(
                newer(
                    provider,
                    "2",
                    "10",
                    "1.21.1",
                    Some("fabric"),
                    &current,
                    &rows
                )
                .is_err()
            );
            assert!(
                newer(
                    provider,
                    "1",
                    "10",
                    "1.21.1",
                    Some("quilt"),
                    &current,
                    &rows
                )
                .unwrap()
                .is_none()
            );
        }
    }
}
