//! Fixed provider metadata endpoints; credentials never go to download/CDN hosts.
use super::{
    ContentError, ExpectedHash, Provider, ProviderClient, ResolvedSource, Result, http, validate_id,
};
use serde::Deserialize;
use std::{io::Read, time::Instant};

const MAX_METADATA_BYTES: usize = 1_024 * 1_024;

pub struct UreqProviderClient {
    agent: ureq::Agent,
    curseforge_api_key: Option<String>,
    deadline: Option<Instant>,
}
impl UreqProviderClient {
    /// Fixed pack metadata endpoints. No caller URL, redirect or download authority.
    pub(crate) fn pack_document(
        &self,
        provider: Provider,
        project: &str,
        version: Option<&str>,
        versions: bool,
    ) -> Result<serde_json::Value> {
        validate_id(project)?;
        if let Some(id) = version {
            validate_id(id)?;
        }
        match provider {
            Provider::Modrinth => {
                let url = if let Some(id) = version {
                    format!("https://api.modrinth.com/v2/version/{id}")
                } else if versions {
                    format!("https://api.modrinth.com/v2/project/{project}/version")
                } else {
                    format!("https://api.modrinth.com/v2/project/{project}")
                };
                self.metadata(&url, None)
            }
            Provider::Curseforge => {
                let project = numeric_id(project)?;
                let suffix = if let Some(id) = version {
                    format!("/files/{}", numeric_id(id)?)
                } else if versions {
                    "/files?pageSize=50".into()
                } else {
                    String::new()
                };
                let response: serde_json::Value = self.metadata(
                    &format!("https://api.curseforge.com/v1/mods/{project}{suffix}"),
                    self.curseforge_api_key.as_deref(),
                )?;
                response
                    .get("data")
                    .cloned()
                    .ok_or(ContentError::InvalidMetadata)
            }
        }
    }
    /// One optional bounded lookup; successful negative results are cacheable.
    pub fn identify_sha512(&self, hash: &str) -> Result<Option<super::ContentProvider>> {
        ExpectedHash::Sha512(hash.into()).validate()?;
        let client = http::agent_with_timeout(std::time::Duration::from_secs(5));
        let body = serde_json::json!({"hashes":[hash],"algorithm":"sha512"}).to_string();
        let mut response = client
            .post("https://api.modrinth.com/v2/version_files")
            .header("Content-Type", "application/json")
            .header("Accept-Encoding", "identity")
            .header("User-Agent", "vesta-control/content-inventory")
            .send(body.as_bytes())
            .map_err(|_| ContentError::DownloadFailed)?;
        validate_metadata_response(
            response.status().as_u16(),
            response
                .headers()
                .get("content-encoding")
                .map(|v| v.as_bytes()),
        )?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .with_config()
            .limit((MAX_METADATA_BYTES + 1) as u64)
            .reader()
            .take((MAX_METADATA_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| ContentError::DownloadFailed)?;
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(ContentError::InvalidMetadata);
        }
        let versions: std::collections::BTreeMap<String, ModrinthVersion> =
            serde_json::from_slice(&bytes).map_err(|_| ContentError::InvalidMetadata)?;
        let Some(version) = versions.get(hash) else {
            return Ok(None);
        };
        if !version
            .files
            .iter()
            .any(|f| f.hashes.sha512.eq_ignore_ascii_case(hash))
        {
            return Err(ContentError::InvalidMetadata);
        }
        validate_id(&version.id)?;
        validate_id(&version.project_id)?;
        Ok(Some(super::ContentProvider {
            provider: "modrinth".into(),
            project_id: version.project_id.clone(),
            version_id: version.id.clone(),
        }))
    }
    /// API key is trusted server configuration, not a request field.
    pub fn new(curseforge_api_key: Option<String>) -> Self {
        Self {
            agent: http::agent(),
            curseforge_api_key,
            deadline: None,
        }
    }
    /// Reuse canonical metadata parsing within an owned job's remaining budget.
    pub(crate) fn with_deadline(&self, deadline: Instant) -> Result<Self> {
        super::deadline_check(deadline)?;
        Ok(Self {
            agent: http::agent_with_timeout(deadline.saturating_duration_since(Instant::now())),
            curseforge_api_key: self.curseforge_api_key.clone(),
            deadline: Some(deadline),
        })
    }
    fn metadata<T: serde::de::DeserializeOwned>(&self, url: &str, key: Option<&str>) -> Result<T> {
        let agent = if let Some(deadline) = self.deadline {
            super::deadline_check(deadline)?;
            http::agent_with_timeout(deadline.saturating_duration_since(Instant::now()))
        } else {
            self.agent.clone()
        };
        let mut request = agent
            .get(url)
            .header("Accept", "application/json")
            .header("Accept-Encoding", "identity")
            .header("User-Agent", "vesta-control/content-installer");
        if let Some(key) = key {
            request = request.header("x-api-key", key);
        }
        let mut response = request.call().map_err(|_| ContentError::DownloadFailed)?;
        // Metadata redirects are refused entirely; download redirects are separately
        // validated against the corresponding provider's CDN allowlist.
        validate_metadata_response(
            response.status().as_u16(),
            response
                .headers()
                .get("content-encoding")
                .map(|v| v.as_bytes()),
        )?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .with_config()
            .limit((MAX_METADATA_BYTES + 1) as u64)
            .reader()
            .take((MAX_METADATA_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| ContentError::DownloadFailed)?;
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(ContentError::InvalidMetadata);
        }
        if let Some(deadline) = self.deadline {
            super::deadline_check(deadline)?;
        }
        serde_json::from_slice(&bytes).map_err(|_| ContentError::InvalidMetadata)
    }
}
impl ProviderClient for UreqProviderClient {
    fn resolve_with_deadline(
        &self,
        provider: Provider,
        project_id: &str,
        version_id: &str,
        deadline: Instant,
    ) -> Result<ResolvedSource> {
        super::deadline_check(deadline)?;
        let client = Self {
            agent: http::agent_with_timeout(deadline.saturating_duration_since(Instant::now())),
            curseforge_api_key: self.curseforge_api_key.clone(),
            deadline: Some(deadline),
        };
        let source = client.resolve(provider, project_id, version_id)?;
        super::deadline_check(deadline)?;
        Ok(source)
    }
    fn resolve(
        &self,
        provider: Provider,
        project_id: &str,
        version_id: &str,
    ) -> Result<ResolvedSource> {
        validate_id(project_id)?;
        validate_id(version_id)?;
        match provider {
            Provider::Modrinth => {
                let metadata: ModrinthVersion = self.metadata(
                    &format!("https://api.modrinth.com/v2/version/{version_id}"),
                    None,
                )?;
                modrinth_source(metadata, project_id, version_id)
            }
            Provider::Curseforge => {
                // The provider uses numeric IDs; refuse noncanonical numeric spelling.
                let project = numeric_id(project_id)?;
                let version = numeric_id(version_id)?;
                let key = self
                    .curseforge_api_key
                    .as_deref()
                    .ok_or(ContentError::DownloadFailed)?;
                let metadata: CurseforgeResponse = self.metadata(
                    &format!("https://api.curseforge.com/v1/mods/{project}/files/{version}"),
                    Some(key),
                )?;
                curseforge_source(metadata.data, project, version)
            }
        }
    }
}
fn validate_metadata_response(status: u16, encoding: Option<&[u8]>) -> Result<()> {
    if status != 200 || encoding.is_some_and(|value| !value.eq_ignore_ascii_case(b"identity")) {
        return Err(ContentError::DownloadFailed);
    }
    Ok(())
}

fn numeric_id(text: &str) -> Result<u64> {
    let value = text
        .parse::<u64>()
        .map_err(|_| ContentError::InvalidRequest)?;
    if value == 0 || value.to_string() != text {
        return Err(ContentError::InvalidRequest);
    }
    Ok(value)
}
#[derive(Deserialize)]
struct ModrinthVersion {
    id: String,
    project_id: String,
    files: Vec<ModrinthFile>,
}
#[derive(Deserialize)]
struct ModrinthFile {
    filename: String,
    url: String,
    primary: bool,
    hashes: ModrinthHashes,
}
#[derive(Deserialize)]
struct ModrinthHashes {
    sha512: String,
}
fn modrinth_source(version: ModrinthVersion, project: &str, id: &str) -> Result<ResolvedSource> {
    if version.id != id || version.project_id != project {
        return Err(ContentError::InvalidMetadata);
    }
    let mut files = version.files;
    let file = if files.len() == 1 {
        files.remove(0)
    } else {
        let mut primary = files.into_iter().filter(|file| file.primary);
        let selected = primary.next().ok_or(ContentError::InvalidMetadata)?;
        if primary.next().is_some() {
            return Err(ContentError::InvalidMetadata);
        }
        selected
    };
    let source = ResolvedSource {
        provider: Provider::Modrinth,
        filename: file.filename,
        download_url: file.url,
        expected_hash: ExpectedHash::Sha512(file.hashes.sha512),
    };
    super::validate_source(&source)?;
    Ok(source)
}
#[derive(Deserialize)]
struct CurseforgeResponse {
    data: CurseforgeFile,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseforgeFile {
    id: u64,
    mod_id: u64,
    file_name: String,
    download_url: Option<String>,
    hashes: Vec<CurseforgeHash>,
}
#[derive(Deserialize)]
struct CurseforgeHash {
    algo: u8,
    value: String,
}
fn curseforge_source(file: CurseforgeFile, project: u64, id: u64) -> Result<ResolvedSource> {
    if file.id != id || file.mod_id != project {
        return Err(ContentError::InvalidMetadata);
    }
    let mut hashes = file.hashes.into_iter().filter(|hash| hash.algo == 1);
    let hash = hashes.next().ok_or(ContentError::InvalidMetadata)?;
    if hashes.next().is_some() {
        return Err(ContentError::InvalidMetadata);
    }
    let source = ResolvedSource {
        provider: Provider::Curseforge,
        filename: file.file_name,
        download_url: file.download_url.ok_or(ContentError::InvalidMetadata)?,
        expected_hash: ExpectedHash::Sha1(hash.value),
    };
    super::validate_source(&source)?;
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_identity_encoding_is_ascii_case_insensitive() {
        for value in [
            b"identity".as_slice(),
            b"Identity",
            b"IDENTITY",
            b"iDeNtItY",
        ] {
            assert_eq!(validate_metadata_response(200, Some(value)), Ok(()));
        }
        assert_eq!(validate_metadata_response(200, None), Ok(()));
        for value in [b"gzip".as_slice(), b"identity, gzip", b"identity "] {
            assert_eq!(
                validate_metadata_response(200, Some(value)),
                Err(ContentError::DownloadFailed)
            );
        }
        assert_eq!(
            validate_metadata_response(302, Some(b"Identity")),
            Err(ContentError::DownloadFailed)
        );
    }
    #[test]
    fn modrinth_binds_ids_and_selects_unique_primary() {
        let json = |project: &str| {
            format!(
                r#"{{"id":"v1","project_id":"{project}","files":[{{"filename":"sources.jar","url":"https://cdn.modrinth.com/sources","primary":false,"hashes":{{"sha512":"{}"}}}},{{"filename":"mod.jar","url":"https://cdn.modrinth.com/mod","primary":true,"hashes":{{"sha512":"{}"}}}}]}}"#,
                "a".repeat(128),
                "b".repeat(128)
            )
        };
        assert_eq!(
            modrinth_source(serde_json::from_str(&json("p1")).unwrap(), "p1", "v1")
                .unwrap()
                .filename,
            "mod.jar"
        );
        assert_eq!(
            modrinth_source(serde_json::from_str(&json("other")).unwrap(), "p1", "v1").unwrap_err(),
            ContentError::InvalidMetadata
        );
        assert_eq!(
            modrinth_source(serde_json::from_str(&json("p1")).unwrap(), "p1", "other").unwrap_err(),
            ContentError::InvalidMetadata
        );
        let duplicate = json("p1").replace("false", "true");
        assert_eq!(
            modrinth_source(serde_json::from_str(&duplicate).unwrap(), "p1", "v1").unwrap_err(),
            ContentError::InvalidMetadata
        );
    }
    #[test]
    fn curseforge_requires_bound_ids_download_and_sha1() {
        let json = format!(
            r#"{{"id":2,"modId":1,"fileName":"mod.jar","downloadUrl":"https://edge.forgecdn.net/mod.jar","hashes":[{{"algo":1,"value":"{}"}}]}}"#,
            "a".repeat(40)
        );
        let decode = |json: &str| serde_json::from_str::<CurseforgeFile>(json).unwrap();
        assert!(curseforge_source(decode(&json), 1, 2).is_ok());
        assert_eq!(
            curseforge_source(decode(&json), 3, 2).unwrap_err(),
            ContentError::InvalidMetadata
        );
        assert_eq!(
            curseforge_source(decode(&json.replace("\"algo\":1", "\"algo\":2")), 1, 2).unwrap_err(),
            ContentError::InvalidMetadata
        );
        assert_eq!(
            curseforge_source(
                decode(&json.replace("\"https://edge.forgecdn.net/mod.jar\"", "null")),
                1,
                2
            )
            .unwrap_err(),
            ContentError::InvalidMetadata
        );
    }
}
