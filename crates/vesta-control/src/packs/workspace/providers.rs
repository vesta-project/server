//! Provider compatibility stays authoritative on the agent. Dependency *selection*
//! uses the launcher's shared instance resolver; publication checks completeness.
use super::*;
use crate::content::{Provider, ProviderClient, UreqProviderClient};
use serde_json::Value;
#[derive(Clone, Debug)]
pub struct ProviderDependency {
    pub project_id: String,
    pub version_id: Option<String>,
    pub incompatible: bool,
}

#[derive(Clone, Debug)]
pub struct ProviderMetadata {
    pub client_required: bool,
    pub server_requirement: Side,
    pub minecraft_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub kind: EntryKind,
    pub client_supported: bool,
    pub downloadable: bool,
    pub filename: String,
    pub dependencies: Vec<ProviderDependency>,
    pub published: String,
}
pub trait WorkspaceProvider: ProviderClient {
    fn pack_metadata_for_artifact(
        &self,
        provider: &str,
        project: &str,
        version: &str,
        sha512: Option<&str>,
    ) -> Result<ProviderMetadata> {
        if sha512.is_some() {
            return Err(invalid());
        }
        self.pack_metadata(provider, project, version)
    }
    fn pack_metadata(
        &self,
        provider: &str,
        project: &str,
        version: &str,
    ) -> Result<ProviderMetadata>;
    fn newer(
        &self,
        provider: &str,
        project: &str,
        version: &str,
        manifest: &PackManifest,
    ) -> Result<Option<String>>;
}
fn platform(provider: &str) -> Result<Provider> {
    match provider {
        "modrinth" => Ok(Provider::Modrinth),
        "curseforge" => Ok(Provider::Curseforge),
        _ => Err(invalid()),
    }
}
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
fn document(
    client: &UreqProviderClient,
    provider: Provider,
    project: &str,
    version: Option<&str>,
    versions: bool,
) -> Result<Value> {
    client
        .pack_document(provider, project, version, versions)
        .map_err(|error| {
            ReleaseError::provider_file(
                error,
                if provider == Provider::Curseforge {
                    "curseforge"
                } else {
                    "modrinth"
                },
                project,
                version.unwrap_or_default(),
                "metadata",
                None,
            )
        })
}
impl WorkspaceProvider for UreqProviderClient {
    fn pack_metadata_for_artifact(
        &self,
        provider: &str,
        project: &str,
        version: &str,
        sha512: Option<&str>,
    ) -> Result<ProviderMetadata> {
        let Some(hash) = sha512 else {
            return self.pack_metadata(provider, project, version);
        };
        if provider != "modrinth" {
            return Err(invalid());
        }
        let mut v = document(self, Provider::Modrinth, project, Some(version), false)?;
        UreqProviderClient::select_modrinth_artifact(&mut v, hash).map_err(|error| {
            ReleaseError::provider_file(error, provider, project, version, "artifact_binding", None)
        })?;
        let p = document(self, Provider::Modrinth, project, None, false)?;
        decode_metadata(Provider::Modrinth, project, version, &p, &v).map_err(|_| {
            ReleaseError::provider_file(
                crate::content::ContentError::InvalidMetadata,
                provider,
                project,
                version,
                "metadata",
                v["files"][0]["filename"].as_str(),
            )
        })
    }
    fn pack_metadata(
        &self,
        provider: &str,
        project: &str,
        version: &str,
    ) -> Result<ProviderMetadata> {
        let platform = platform(provider)?;
        let v = document(self, platform, project, Some(version), false)?;
        let p = document(self, platform, project, None, false)?;
        decode_metadata(platform, project, version, &p, &v).map_err(|_| {
            ReleaseError::provider_file(
                crate::content::ContentError::InvalidMetadata,
                provider,
                project,
                version,
                "metadata",
                None,
            )
        })
    }
    fn newer(
        &self,
        provider: &str,
        project: &str,
        version: &str,
        m: &PackManifest,
    ) -> Result<Option<String>> {
        let platform = platform(provider)?;
        let current = self.pack_metadata(provider, project, version)?;
        let p = document(self, platform, project, None, false)?;
        let versions = document(self, platform, project, None, true)?;
        let rows = versions.as_array().ok_or_else(invalid)?;
        let mut candidates = Vec::new();
        for v in rows.iter().take(256) {
            let id = if platform == Provider::Modrinth {
                v["id"].as_str().unwrap_or_default().to_string()
            } else {
                v["id"].as_u64().unwrap_or_default().to_string()
            };
            if id == version {
                continue;
            }
            if let Ok(metadata) = decode_metadata(platform, project, &id, &p, v)
                && metadata.published > current.published
                && compatible(&metadata, m)
                && metadata.downloadable
            {
                candidates.push((metadata.published, id));
            }
        }
        candidates.sort();
        Ok(candidates.pop().map(|(_, id)| id))
    }
}
pub(super) fn compatible(meta: &ProviderMetadata, m: &PackManifest) -> bool {
    meta.client_supported
        && meta.minecraft_versions.contains(&m.minecraft_version)
        && match meta.kind {
            EntryKind::Mod => {
                m.loader != ClientLoader::None
                    && (meta.loaders.iter().any(|l| l == m.loader.id())
                        || (m.loader == ClientLoader::Quilt
                            && meta.loaders.iter().any(|l| l == "fabric")))
            }
            EntryKind::ResourcePack | EntryKind::Shader | EntryKind::Datapack => true,
            EntryKind::ConfigOverride => false,
        }
}
fn decode_metadata(
    provider: Provider,
    project: &str,
    version: &str,
    p: &Value,
    v: &Value,
) -> Result<ProviderMetadata> {
    if provider == Provider::Modrinth {
        if v["id"] != version || v["project_id"] != project || p["id"] != project {
            return Err(invalid());
        }
        let loaders = strings(&v["loaders"]);
        let kind = if loaders.iter().any(|s| s == "datapack") {
            EntryKind::Datapack
        } else {
            match p["project_type"].as_str() {
                Some("mod") => EntryKind::Mod,
                Some("resourcepack") => EntryKind::ResourcePack,
                Some("shader") => EntryKind::Shader,
                Some("datapack") => EntryKind::Datapack,
                _ => return Err(invalid()),
            }
        };
        let files = v["files"].as_array().ok_or_else(invalid)?;
        let selected = if files.len() == 1 {
            &files[0]
        } else {
            let mut primary = files.iter().filter(|f| f["primary"] == true);
            let selected = primary.next().ok_or_else(invalid)?;
            if primary.next().is_some() {
                return Err(invalid());
            }
            selected
        };
        let mut deps = Vec::new();
        for d in v["dependencies"].as_array().into_iter().flatten() {
            if !matches!(
                d["dependency_type"].as_str(),
                Some("required" | "incompatible")
            ) {
                continue;
            }
            // A version-only edge is checked by pinned version even without project ID.
            let project = d["project_id"].as_str().unwrap_or_default().to_string();
            let version = d["version_id"].as_str().map(str::to_string);
            if project.is_empty() && version.is_none() {
                return Err(invalid());
            }
            deps.push(ProviderDependency {
                project_id: project,
                version_id: version,
                incompatible: d["dependency_type"] == "incompatible",
            });
        }
        Ok(ProviderMetadata {
            client_required: p["client_side"] == "required",
            server_requirement: match p["server_side"].as_str() {
                Some("required") => Side::Required,
                Some("optional") => Side::Optional,
                Some("unsupported") => Side::Unsupported,
                _ => Side::Optional,
            },
            minecraft_versions: strings(&v["game_versions"]),
            loaders,
            kind,
            client_supported: p["client_side"] != "unsupported",
            downloadable: selected["url"].as_str().is_some_and(|s| !s.is_empty()),
            filename: selected["filename"].as_str().ok_or_else(invalid)?.into(),
            dependencies: deps,
            published: v["date_published"].as_str().ok_or_else(invalid)?.into(),
        })
    } else {
        if v["id"].as_u64().map(|n| n.to_string()).as_deref() != Some(version)
            || v["modId"].as_u64().map(|n| n.to_string()).as_deref() != Some(project)
            || p["id"].as_u64().map(|n| n.to_string()).as_deref() != Some(project)
        {
            return Err(invalid());
        }
        let kind = match p["classId"].as_u64() {
            Some(6) => EntryKind::Mod,
            Some(12) => EntryKind::ResourcePack,
            Some(6552) => EntryKind::Shader,
            Some(6945) => EntryKind::Datapack,
            _ => return Err(invalid()),
        };
        let versions = strings(&v["gameVersions"]);
        let loaders = versions
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .filter(|s| matches!(s.as_str(), "fabric" | "quilt" | "forge" | "neoforge"))
            .collect();
        let deps = v["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| matches!(d["relationType"].as_u64(), Some(3 | 5)))
            .map(|d| {
                Ok(ProviderDependency {
                    project_id: d["modId"].as_u64().ok_or_else(invalid)?.to_string(),
                    version_id: None,
                    incompatible: d["relationType"] == 5,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ProviderMetadata {
            client_required: false,
            server_requirement: Side::Optional,
            minecraft_versions: versions,
            loaders,
            kind,
            client_supported: true,
            downloadable: crate::content::curseforge_document_source(v, project, version).is_ok(),
            filename: v["fileName"].as_str().ok_or_else(invalid)?.into(),
            dependencies: deps,
            published: v["fileDate"].as_str().ok_or_else(invalid)?.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "small real CurseForge metadata check; explicit repo dotenv runtime configuration required"]
    fn audit_pinned_curseforge_server_associations() {
        let client = UreqProviderClient::configured();
        for (project, version, server) in [
            ("285109", "4612979", 4612990u64),
            ("925200", "9431424", 9431427u64),
        ] {
            let file = client
                .pack_document(Provider::Curseforge, project, Some(version), false)
                .unwrap();
            assert_eq!(file["serverPackFileId"], server);
            assert_eq!(file["modId"].as_u64().unwrap().to_string(), project);
            let target = client
                .pack_document(
                    Provider::Curseforge,
                    project,
                    Some(&server.to_string()),
                    false,
                )
                .unwrap();
            assert_eq!(target["id"], server);
            assert_eq!(target["modId"].as_u64().unwrap().to_string(), project);
            assert_eq!(target["isServerPack"], true);
            let resolved = client
                .resolve(Provider::Curseforge, project, &server.to_string())
                .unwrap();
            eprintln!(
                "AUDIT_PROVIDER project={project} client={version} client_name={} server={server} server_name={} server_bytes={} hash={:?}",
                file["fileName"], target["fileName"], target["fileLength"], resolved.expected_hash
            );
        }
    }
    #[test]
    fn curseforge_publication_accepts_verified_cdn_fallback_but_not_missing_hashes() {
        let project = serde_json::json!({"id":1691925,"classId":6,"allowModDistribution":false});
        let mut file = serde_json::json!({"id":9022781,"modId":1691925,"fileName":"mielon's-the-sift+26.3.jar","downloadUrl":null,
            "hashes":[{"algo":1,"value":"a".repeat(40)}],"gameVersions":["26.3","Fabric"],"fileDate":"2026-10-29T00:00:00Z"});
        assert!(
            decode_metadata(Provider::Curseforge, "1691925", "9022781", &project, &file)
                .unwrap()
                .downloadable
        );
        file["hashes"] = serde_json::json!([]);
        assert!(
            !decode_metadata(Provider::Curseforge, "1691925", "9022781", &project, &file)
                .unwrap()
                .downloadable
        );
    }
}
