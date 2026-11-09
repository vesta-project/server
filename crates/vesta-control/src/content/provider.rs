//! Fixed provider metadata endpoints; credentials never go to download/CDN hosts.
use super::{
    ContentError, ExpectedHash, Provider, ProviderClient, ResolvedSource, Result, http, validate_id,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io::Read,
    sync::{Arc, Mutex},
    time::Instant,
};

const MAX_METADATA_BYTES: usize = 1_024 * 1_024;
const MAX_JOB_METADATA_BYTES: usize = 4 * 1_024 * 1_024;
#[derive(Default)]
struct MetadataCache {
    bytes: usize,
    entries: BTreeMap<String, Arc<[u8]>>,
    batch_attempts: std::collections::BTreeSet<String>,
    #[cfg(test)]
    requests: [usize; 2],
}
impl MetadataCache {
    fn insert(&mut self, url: &str, bytes: Vec<u8>) {
        if self.entries.contains_key(url)
            || self.entries.len() >= 256
            || self.bytes.saturating_add(bytes.len()) > MAX_JOB_METADATA_BYTES
        {
            return;
        }
        self.bytes += bytes.len();
        self.entries.insert(url.into(), bytes.into());
    }
}
include!(concat!(env!("OUT_DIR"), "/provider_key.rs"));

fn bundled_curseforge_key() -> Option<String> {
    (!BUNDLED_CURSEFORGE_KEY.is_empty()).then(|| {
        BUNDLED_CURSEFORGE_KEY
            .iter()
            .map(|b| (b ^ 0x55) as char)
            .collect()
    })
}

pub struct UreqProviderClient {
    agent: ureq::Agent,
    curseforge_api_key: Option<String>,
    deadline: Option<Instant>,
    // Owned by one bounded operation, never a process-wide stale provider cache.
    metadata_cache: Option<Arc<Mutex<MetadataCache>>>,
}
/// Bulk responses are unordered and may omit files. Bind every returned row
/// before populating the ordinary GET cache; never infer identity by position.
fn bind_curseforge_batch(
    files: bool,
    requested: &BTreeMap<u64, u64>,
    value: &serde_json::Value,
) -> Result<Vec<(String, Vec<u8>)>> {
    let rows = value["data"]
        .as_array()
        .ok_or(ContentError::InvalidMetadata)?;
    if rows.len() > requested.len() {
        return Err(ContentError::InvalidMetadata);
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut result = Vec::new();
    for row in rows {
        let id = row["id"].as_u64().ok_or(ContentError::InvalidMetadata)?;
        let project = requested.get(&id).ok_or(ContentError::InvalidMetadata)?;
        if !seen.insert(id) || (files && row["modId"].as_u64() != Some(*project)) {
            return Err(ContentError::InvalidMetadata);
        }
        let url = if files {
            format!("https://api.curseforge.com/v1/mods/{project}/files/{id}")
        } else {
            format!("https://api.curseforge.com/v1/mods/{id}")
        };
        let bytes = serde_json::to_vec(&serde_json::json!({"data":row}))
            .map_err(|_| ContentError::InvalidMetadata)?;
        result.push((url, bytes));
    }
    Ok(result)
}
impl UreqProviderClient {
    pub(crate) fn resolve_artifact(&self, provider: Provider, project: &str, version: &str, sha512: Option<&str>, deadline: Instant) -> Result<ResolvedSource> {
        let client = self.with_deadline(deadline)?;
        let Some(hash) = sha512 else { return client.resolve(provider, project, version); };
        if provider != Provider::Modrinth { return Err(ContentError::InvalidMetadata); }
        let mut document = client.pack_document(provider, project, Some(version), false)?;
        Self::select_modrinth_artifact(&mut document, hash)?;
        let metadata: ModrinthVersion = serde_json::from_value(document).map_err(|_| ContentError::InvalidMetadata)?;
        modrinth_source(metadata, project, version)
    }
    pub(crate) fn select_modrinth_artifact(document: &mut serde_json::Value, hash: &str) -> Result<()> {
        ExpectedHash::Sha512(hash.into()).validate()?;
        let files = document["files"].as_array().ok_or(ContentError::InvalidMetadata)?;
        let matching: Vec<_> = files.iter().filter(|file| file["hashes"]["sha512"].as_str().is_some_and(|value|value.eq_ignore_ascii_case(hash))).cloned().collect();
        if matching.len() != 1 { return Err(ContentError::InvalidMetadata); }
        document["files"] = serde_json::Value::Array(matching);
        Ok(())
    }
    fn prefetch_once(&self, url: String) -> bool {
        let Some(cache) = &self.metadata_cache else {
            return false;
        };
        let Ok(mut cache) = cache.lock() else {
            return false;
        };
        !cache.entries.contains_key(&url)
            && cache.batch_attempts.len() < 512
            && cache.batch_attempts.insert(url)
    }
    #[cfg(test)]
    fn count_request(&self, post: bool) {
        if let Some(cache) = &self.metadata_cache
            && let Ok(mut cache) = cache.lock()
        {
            cache.requests[usize::from(post)] += 1;
        }
    }
    #[cfg(test)]
    pub(crate) fn metadata_requests(&self) -> [usize; 2] {
        self.metadata_cache
            .as_ref()
            .and_then(|cache| cache.lock().ok().map(|cache| cache.requests))
            .unwrap_or_default()
    }
    fn cached(&self, url: &str) -> Option<Arc<[u8]>> {
        self.metadata_cache
            .as_ref()?
            .lock()
            .ok()?
            .entries
            .get(url)
            .cloned()
    }
    fn read_metadata(&self, mut response: ureq::http::Response<ureq::Body>) -> Result<Vec<u8>> {
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
        Ok(bytes)
    }
    fn prefetch_batch(&self, files: bool, requested: &BTreeMap<u64, u64>) -> Result<()> {
        if requested.is_empty() {
            return Ok(());
        }
        if let Some(deadline) = self.deadline {
            super::deadline_check(deadline)?;
        }
        let route = if files { "mods/files" } else { "mods" };
        let ids: Vec<_> = requested.keys().copied().collect();
        let body = if files {
            serde_json::json!({"fileIds": ids})
        } else {
            serde_json::json!({"modIds": ids, "filterPcOnly": true})
        };
        #[cfg(test)]
        self.count_request(true);
        let response = self
            .agent
            .post(format!("https://api.curseforge.com/v1/{route}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Accept-Encoding", "identity")
            .header("User-Agent", "vesta-control/content-installer")
            .header(
                "x-api-key",
                self.curseforge_api_key
                    .as_deref()
                    .ok_or(ContentError::ProviderNotConfigured)?,
            )
            .config()
            .timeout_global(Some(self.deadline.map_or(
                std::time::Duration::from_secs(20),
                |d| {
                    d.saturating_duration_since(Instant::now())
                        .min(std::time::Duration::from_secs(20))
                },
            )))
            .build()
            .send(serde_json::to_vec(&body).map_err(|_| ContentError::InvalidRequest)?)
            .map_err(|_| ContentError::DownloadFailed)?;
        let bytes = self.read_metadata(response)?;
        let document: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| ContentError::InvalidMetadata)?;
        let documents = bind_curseforge_batch(files, requested, &document)?;
        if let Some(cache) = &self.metadata_cache
            && let Ok(mut cache) = cache.lock()
        {
            for (url, bytes) in documents {
                cache.insert(&url, bytes);
            }
        }
        Ok(())
    }
    /// Fixed pack metadata endpoints. No caller URL, redirect or download authority.
    pub(crate) fn pack_document(
        &self,
        provider: Provider,
        project: &str,
        version: Option<&str>,
        versions: bool,
    ) -> Result<serde_json::Value> {
        self.provider_configuration(provider)?;
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
            metadata_cache: None,
        }
    }
    /// Trusted host configuration. Runtime overrides take precedence over the
    /// application key embedded by the release build; callers supply no key.
    pub fn configured() -> Self {
        Self::new(
            std::env::var("VESTA_CURSEFORGE_API_KEY")
                .ok()
                .or_else(|| std::env::var("CURSEFORGE_API_KEY").ok())
                .or_else(bundled_curseforge_key),
        )
    }
    fn provider_configuration(&self, provider: Provider) -> Result<()> {
        if provider == Provider::Curseforge
            && !self.curseforge_api_key.as_deref().is_some_and(|key| {
                !key.is_empty() && key.len() <= 1024 && key.bytes().all(|b| (33..=126).contains(&b))
            })
        {
            return Err(ContentError::ProviderNotConfigured);
        }
        Ok(())
    }
    /// Reuse canonical metadata parsing within an owned job's remaining budget.
    pub(crate) fn with_deadline(&self, deadline: Instant) -> Result<Self> {
        let deadline = self.deadline.map_or(deadline, |prior| prior.min(deadline));
        super::deadline_check(deadline)?;
        Ok(Self {
            agent: if self.metadata_cache.is_some() {
                self.agent.clone()
            } else {
                http::job_agent(deadline.saturating_duration_since(Instant::now()))
            },
            curseforge_api_key: self.curseforge_api_key.clone(),
            deadline: Some(deadline),
            metadata_cache: Some(self.metadata_cache.clone().unwrap_or_default()),
        })
    }
    fn metadata<T: serde::de::DeserializeOwned>(&self, url: &str, key: Option<&str>) -> Result<T> {
        if let Some(deadline) = self.deadline {
            super::deadline_check(deadline)?;
        }
        let cached = self.cached(url);
        if let Some(bytes) = cached {
            return serde_json::from_slice(&bytes).map_err(|_| ContentError::InvalidMetadata);
        }
        let mut request = self
            .agent
            .get(url)
            .header("Accept", "application/json")
            .header("Accept-Encoding", "identity")
            .header("User-Agent", "vesta-control/content-installer")
            .config()
            .timeout_global(Some(
                self.deadline
                    .map_or(std::time::Duration::from_secs(60), |deadline| {
                        deadline.saturating_duration_since(Instant::now())
                    }),
            ))
            .build();
        if let Some(key) = key {
            request = request.header("x-api-key", key);
        }
        #[cfg(test)]
        self.count_request(false);
        let response = request.call().map_err(|_| ContentError::DownloadFailed)?;
        // Metadata redirects are refused entirely; download redirects are separately
        // validated against the corresponding provider's CDN allowlist.
        let bytes = self.read_metadata(response)?;
        let value = serde_json::from_slice(&bytes).map_err(|_| ContentError::InvalidMetadata)?;
        if let Some(cache) = &self.metadata_cache
            && let Ok(mut cache) = cache.lock()
        {
            cache.insert(url, bytes);
        }
        Ok(value)
    }
}
impl ProviderClient for UreqProviderClient {
    fn archive_directory(&self, provider: Provider, project: &str) -> Result<&'static str> {
        if provider != Provider::Curseforge { return Ok("mods"); }
        let document=self.pack_document(provider,project,None,false)?;
        match document["classId"].as_u64() {
            Some(6) => Ok("mods"), Some(12) => Ok("resourcepacks"),
            Some(6552) => Ok("shaderpacks"), Some(6945) => Ok("datapacks"),
            _ => Err(ContentError::InvalidMetadata),
        }
    }
    fn archive_filename(&self, provider: Provider, project: &str, version: &str) -> Result<String> {
        if provider != Provider::Curseforge { return self.resolve(provider, project, version).map(|s| s.filename); }
        let document = self.pack_document(provider, project, Some(version), false)?;
        let file: CurseforgeFile = serde_json::from_value(document).map_err(|_| ContentError::InvalidMetadata)?;
        if file.id != numeric_id(version)? || file.mod_id != numeric_id(project)? { return Err(ContentError::InvalidMetadata); }
        if file.file_name.contains('/') || file.file_name.contains('\\') { return Err(ContentError::UnsafeFilename); }
        super::provider_storage_name(&file.file_name)
    }
    fn prefetch_curseforge(&self, files: &[(String, String)]) -> Result<()> {
        if files.is_empty() || self.metadata_cache.is_none() {
            return Ok(());
        }
        if files.len() > vesta_protocol::pack::catalogue::MAX_FILES {
            return Err(ContentError::TooLarge);
        }
        self.provider_configuration(Provider::Curseforge)?;
        let mut file_ids = BTreeMap::new();
        let mut projects = BTreeMap::new();
        for (project, version) in files {
            let project = numeric_id(project)?;
            let id = numeric_id(version)?;
            if file_ids
                .insert(id, project)
                .is_some_and(|prior| prior != project)
            {
                return Err(ContentError::InvalidMetadata);
            }
            projects.insert(project, project);
        }
        file_ids.retain(|id, project| {
            self.prefetch_once(format!(
                "https://api.curseforge.com/v1/mods/{project}/files/{id}"
            ))
        });
        projects
            .retain(|id, _| self.prefetch_once(format!("https://api.curseforge.com/v1/mods/{id}")));
        for (is_file, rows, chunk) in [(true, file_ids, 32), (false, projects, 8)] {
            let rows: Vec<_> = rows.into_iter().collect();
            for chunk in rows.chunks(chunk) {
                // Optional optimization: missing rows or a failed bulk endpoint
                // fall back to the same bound, individual resolver below.
                if self
                    .prefetch_batch(is_file, &chunk.iter().copied().collect())
                    .is_err()
                {
                    break;
                }
            }
        }
        Ok(())
    }
    fn resolve_with_deadline(
        &self,
        provider: Provider,
        project_id: &str,
        version_id: &str,
        deadline: Instant,
    ) -> Result<ResolvedSource> {
        super::deadline_check(deadline)?;
        let client = self.with_deadline(deadline)?;
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
        self.provider_configuration(provider)?;
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
        expected_hash: ExpectedHash::Sha512(file.hashes.sha512.to_ascii_lowercase()),
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
    let expected_hash = ExpectedHash::Sha1(hash.value);
    expected_hash.validate()?;
    super::validate_filename(&file.file_name)?;
    // Match the launcher's fallback for files whose API downloadUrl is absent.
    // Only the provider-bound file ID/name choose this fixed CDN path; the digest
    // still comes from authoritative metadata and is verified before use.
    let mut download_url = file
        .download_url
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| {
            let mut url = url::Url::parse("https://edge.forgecdn.net/").expect("fixed CDN URL");
            url.path_segments_mut().expect("fixed CDN base").extend([
                "files",
                &(file.id / 1000).to_string(),
                &format!("{:03}", file.id % 1000),
                &file.file_name,
            ]);
            url.into()
        });
    // The legacy edge alias can return 404 for available archives. Use the
    // provider's current file CDN for that alias, keeping path and digest binding.
    if let Ok(mut url)=url::Url::parse(&download_url) {
        if url.host_str()==Some("edge.forgecdn.net") {
            url.set_host(Some("mediafilez.forgecdn.net")).map_err(|_|ContentError::InvalidMetadata)?;
            download_url=url.into();
        }
    }
    let source = ResolvedSource {
        provider: Provider::Curseforge,
        filename: file.file_name,
        download_url,
        expected_hash,
    };
    super::validate_source(&source)?;
    Ok(source)
}

/// Reuse the exact resolver policy when deciding whether a pack entry can be
/// installed or published. An omitted API URL alone is not an unavailable file.
pub(crate) fn curseforge_document_source(
    document: &serde_json::Value,
    project: &str,
    version: &str,
) -> Result<ResolvedSource> {
    let file =
        serde_json::from_value(document.clone()).map_err(|_| ContentError::InvalidMetadata)?;
    curseforge_source(file, numeric_id(project)?, numeric_id(version)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batch_rows_bind_by_file_and_project_not_response_order() {
        let requested = BTreeMap::from([(2, 1), (3, 9)]);
        let rows = serde_json::json!({"data":[{"id":3,"modId":9},{"id":2,"modId":1}]});
        let bound = bind_curseforge_batch(true, &requested, &rows).unwrap();
        assert_eq!(bound[0].0, "https://api.curseforge.com/v1/mods/9/files/3");
        assert_eq!(bound[1].0, "https://api.curseforge.com/v1/mods/1/files/2");
        for bad in [
            serde_json::json!({"data":[{"id":2,"modId":9}]}),
            serde_json::json!({"data":[{"id":4,"modId":1}]}),
            serde_json::json!({"data":[{"id":2,"modId":1},{"id":2,"modId":1}]}),
            serde_json::json!({"data":[{"modId":1}]}),
        ] {
            assert_eq!(
                bind_curseforge_batch(true, &requested, &bad),
                Err(ContentError::InvalidMetadata)
            );
        }
        let partial = bind_curseforge_batch(
            true,
            &requested,
            &serde_json::json!({"data":[{"id":2,"modId":1}]}),
        )
        .unwrap();
        assert_eq!(partial.len(), 1); // Missing rows remain an ordinary GET lookup.
        assert!(
            bind_curseforge_batch(
                false,
                &BTreeMap::from([(1, 1)]),
                &serde_json::json!({"data":[{"id":9}]})
            )
            .is_err()
        );
    }
    #[test]
    fn failed_prefetch_is_not_retried_by_later_stages_of_the_same_job() {
        let client = UreqProviderClient::new(None);
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        let job = client.with_deadline(deadline).unwrap();
        let url = "https://api.curseforge.com/v1/mods/1/files/2";
        assert!(job.prefetch_once(url.into()));
        assert!(
            !job.with_deadline(deadline)
                .unwrap()
                .prefetch_once(url.into())
        );
        assert!(
            client
                .with_deadline(deadline)
                .unwrap()
                .prefetch_once(url.into())
        );
    }
    #[test]
    fn job_metadata_reuses_exact_documents_with_a_deadline_and_memory_bound() {
        let root = UreqProviderClient::new(Some("fixture".into()));
        assert!(root.metadata_cache.is_none());
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        let job = root.with_deadline(deadline).unwrap();
        let nested = job
            .with_deadline(deadline + std::time::Duration::from_secs(60))
            .unwrap();
        assert_eq!(nested.deadline, Some(deadline));
        assert!(Arc::ptr_eq(
            job.metadata_cache.as_ref().unwrap(),
            nested.metadata_cache.as_ref().unwrap()
        ));
        let url = "https://api.curseforge.com/v1/mods/1/files/2";
        let bytes = serde_json::to_vec(&serde_json::json!({"id":2,"modId":1,"fileName":"mod+1.jar","downloadUrl":null,"hashes":[{"algo":1,"value":"a".repeat(40)}]})).unwrap();
        job.metadata_cache
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .insert(url, bytes);
        let file: CurseforgeFile = nested.metadata(url, Some("fixture")).unwrap();
        assert!(
            curseforge_source(file, 1, 2)
                .unwrap()
                .download_url
                .ends_with("/0/002/mod+1.jar")
        );
        let mut cache = job.metadata_cache.as_ref().unwrap().lock().unwrap();
        let before = cache.bytes;
        cache.insert("too-large", vec![0; MAX_JOB_METADATA_BYTES]);
        assert_eq!(cache.bytes, before);
        assert!(!cache.entries.contains_key("too-large"));
    }
    #[test]
    #[ignore = "requires configured CurseForge application key and live provider access"]
    fn configured_curseforge_key_authenticates_live_metadata() {
        let client = UreqProviderClient::configured();
        let metadata = client
            .pack_document(Provider::Curseforge, "394468", None, false)
            .unwrap();
        assert_eq!(metadata["id"].as_u64(), Some(394468));
    }
    #[test]
    fn missing_curseforge_configuration_is_not_a_bad_request_or_network_failure() {
        for key in [None, Some(String::new()), Some("bad\nkey".into())] {
            let client = UreqProviderClient::new(key);
            assert!(matches!(
                client.resolve(Provider::Curseforge, "1", "2"),
                Err(ContentError::ProviderNotConfigured)
            ));
            assert_eq!(
                client.pack_document(Provider::Curseforge, "1", None, false),
                Err(ContentError::ProviderNotConfigured)
            );
            assert!(client.provider_configuration(Provider::Modrinth).is_ok());
        }
        assert_eq!(
            crate::packs::ReleaseError::from(ContentError::ProviderNotConfigured).code(),
            "content_provider_not_configured"
        );
        for error in [
            ContentError::DownloadFailed,
            ContentError::InvalidMetadata,
            ContentError::HashMismatch,
        ] {
            assert_eq!(
                crate::packs::ReleaseError::from(error).code(),
                "content_provider_unavailable"
            );
        }
    }
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
    fn cobbleverse_unicode_resource_archive_resolves_without_source_refusal() {
        // Real resource-pack dependency from COBBLEVERSE 1.7.42. Its Unicode
        // display name was rejected by the old game-root filename policy.
        let document = serde_json::json!({
            "id": 6060565,
            "modId": 1173260,
            "fileName": "Cobblemon 3D Poké Rods 1.0.zip",
            "downloadUrl": "https://edge.forgecdn.net/files/6060/565/Cobblemon%203D%20Pok%c3%a9%20Rods%201.0.zip",
            "hashes": [{ "algo": 1, "value": "c5a92bf0d82abba72eb3a93f6e8bd40560d1c247" }]
        });
        let source = curseforge_document_source(&document, "1173260", "6060565").unwrap();
        let (storage, url) = super::super::validate_source(&source).unwrap();
        assert_eq!(storage.as_str(), "mods/provider-archive.zip");
        assert_eq!(url.host_str(), Some("mediafilez.forgecdn.net"));
        assert_eq!(source.filename, "Cobblemon 3D Poké Rods 1.0.zip");
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
            .unwrap()
            .download_url,
            "https://mediafilez.forgecdn.net/files/0/002/mod.jar"
        );
        let unavailable = json.replace("\"https://edge.forgecdn.net/mod.jar\"", "null");
        assert_eq!(
            curseforge_source(
                decode(&unavailable.replace("mod.jar", "mielon's-the-sift+26.3.jar")),
                1,
                2
            )
            .unwrap()
            .download_url,
            "https://mediafilez.forgecdn.net/files/0/002/mielon's-the-sift+26.3.jar"
        );
        // A missing URL must not mask invalid identity or paths.
        assert_eq!(
            curseforge_source(decode(&unavailable), 3, 2).unwrap_err(),
            ContentError::InvalidMetadata
        );
        assert_eq!(
            curseforge_source(
                decode(&unavailable.replace("mod.jar", "../secret.jar")),
                1,
                2
            )
            .unwrap_err(),
            ContentError::UnsafeFilename
        );
        assert!(
            curseforge_source(
                decode(&json.replace("mod.jar", "fabric-api+26.3.jar")),
                1,
                2
            )
            .is_ok()
        );
    }
}

#[cfg(test)]
mod artifact_binding_tests {
    use super::*;
    #[test]
    fn pinned_file_selection_does_not_substitute_primary_or_other_project() {
        let file=|name:&str,hash:String,primary:bool|serde_json::json!({"filename":name,"url":format!("https://cdn.modrinth.com/data/project/versions/version/{name}"),"primary":primary,"hashes":{"sha512":hash}});
        let document=serde_json::json!({"id":"version","project_id":"project","files":[file("primary.jar","a".repeat(128),true),file("secondary.jar","b".repeat(128),false)]});
        let mut selected=document.clone();
        UreqProviderClient::select_modrinth_artifact(&mut selected,&"b".repeat(128)).unwrap();
        let source=modrinth_source(serde_json::from_value(selected.clone()).unwrap(),"project","version").unwrap();
        assert_eq!(source.filename,"secondary.jar");
        assert!(matches!(source.expected_hash,ExpectedHash::Sha512(hash) if hash=="b".repeat(128)));
        assert!(modrinth_source(serde_json::from_value(selected.clone()).unwrap(),"another-project","version").is_err());
        assert!(modrinth_source(serde_json::from_value(selected).unwrap(),"project","another-version").is_err());
        assert!(UreqProviderClient::select_modrinth_artifact(&mut document.clone(),&"c".repeat(128)).is_err());
        assert!(UreqProviderClient::select_modrinth_artifact(&mut document.clone(),"invalid").is_err());
        let mut ambiguous=document.clone();ambiguous["files"][0]["hashes"]["sha512"]=("b".repeat(128)).into();
        assert!(UreqProviderClient::select_modrinth_artifact(&mut ambiguous,&"b".repeat(128)).is_err());
    }
}
