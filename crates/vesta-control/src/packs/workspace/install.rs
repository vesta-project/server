//! Whole-pack review pins verified bytes and file preconditions before staging.
use super::*;
use crate::packs::payload::{Payload, Store as PayloadStore};
use crate::{
    content::{self, ContentProvider, Provider, ProviderClient},
    files::{FileBrowser, FileError, RelativePath},
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
#[path = "archive_policy.rs"]
pub(super) mod archive_policy;
#[path = "install_downloads.rs"]
mod downloads;
#[path = "install_bytes.rs"]
mod stored_bytes;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackInstallProgress {
    pub stage: String,
    pub completed: usize,
    pub total: usize,
}
struct Preparation {
    db: StateDatabase,
    job: String,
}
impl Preparation {
    fn report(&self, stage: &str, completed: usize, total: usize) -> Result<()> {
        let body = serde_json::to_string(&PackInstallProgress {
            stage: stage.into(),
            completed,
            total,
        })
        .map_err(|_| invalid())?;
        self.db.write(|c| {
            c.execute(
                "INSERT OR REPLACE INTO pack_install_progress VALUES(?,?,?,?)",
                params![
                    self.db.server_id(),
                    self.job,
                    body,
                    self.db.clock().millis() + 3_600_000
                ],
            )
            .map(|_| ())
            .map_err(Into::into)
        })?;
        Ok(())
    }
}
impl Drop for Preparation {
    fn drop(&mut self) {
        let _ = self.db.write(|c| {
            c.execute(
                "DELETE FROM pack_install_progress WHERE server_id=? AND job=?",
                params![self.db.server_id(), self.job],
            )
            .map(|_| ())
            .map_err(Into::into)
        });
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum InstallSource {
    Provider {
        provider: String,
        project_id: String,
        version_id: String,
    },
    UploadedArchive {
        upload_id: String,
    },
    Draft {
        revision: String,
    },
    Published {
        revision: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackInstallRequest {
    pub source: InstallSource,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallFile {
    pub path: String,
    pub sha256: String,
    pub expected_sha256: Option<String>,
    pub conflict: Option<String>,
    #[serde(default)]
    pub operation: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SoftwareSwitchTarget {
    pub software: String,
    pub minecraft_version: String,
    pub loader_version: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SoftwareSwitch {
    pub from: SoftwareSwitchTarget,
    pub to: SoftwareSwitchTarget,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallWarning { pub code: String, pub rule: String, pub item: String, pub detail: String }
fn warnings_require_acknowledgement(warnings: &[InstallWarning]) -> bool {
    warnings.iter().any(|warning| {
        !(matches!(warning.code.as_str(), "pack_size_warning" | "version_from_pack_manifest" | "curseforge_server_pack" | "server_pack_managed_file_skipped" | "missing_required_dependency")
            || (warning.code == "archive_limit_warning" && warning.rule.starts_with("legacy_")))
    })
}
fn read_install_warnings<'de, D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Vec<InstallWarning>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire { Structured(InstallWarning), Legacy(String) }
    Ok(Vec::<Wire>::deserialize(deserializer)?.into_iter().map(|warning| match warning { Wire::Structured(value) => value, Wire::Legacy(value) => value.into() }).collect())
}
impl From<String> for InstallWarning {
    fn from(detail: String) -> Self {
        let (code, item) = detail.split_once(':').unwrap_or((&detail, "pack"));
        Self { code: code.into(), rule: code.into(), item: item.into(), detail }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackInstallPlan {
    #[serde(default)]
    pub software_switch: Option<SoftwareSwitch>,
    pub plan_id: String,
    pub confirmation_token: String,
    pub expires_at_ms: i64,
    pub minecraft_version: String,
    pub software: String,
    pub loader_version: Option<String>,
    pub software_switch_required: bool,
    pub valid: bool,
    pub files: Vec<InstallFile>,
    pub skipped: Vec<String>,
    #[serde(deserialize_with = "read_install_warnings")]
    pub warnings: Vec<InstallWarning>,
    pub backup_recommended: bool,
    #[serde(default)]
    pub removed: Vec<(String, String)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallChoice {
    pub path: String,
    pub choice: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackInstallApply {
    #[serde(default)]
    pub acknowledge_warnings: bool,
    #[serde(default)]
    pub acknowledge_software_switch: bool,
    pub plan_id: String,
    pub confirmation_token: String,
    /// Exactly one replace/keep choice per conflicting destination.
    pub choices: Vec<InstallChoice>,
    /// Deprecated compatibility input; apply always links the server and never publishes clients.
    #[serde(default)]
    pub set_as_modpack: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackInstallOutcome {
    #[serde(default)]
    pub software_job_id: Option<String>,
    pub state: String,
    pub pending_ids: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    applied_key: Option<String>,
    #[serde(default, with = "stored_bytes::optional")]
    client_archive: Option<Vec<u8>>,
    #[serde(default)]
    expected_publication: Option<String>,
    #[serde(default)]
    expected_link: Option<ServerModpackLink>,
    manifest: PackManifest,
    source: InstallSource,
    draft_revision: String,
    published_revision: String,
    plan: PackInstallPlan,
    #[serde(with = "stored_bytes")]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bytes: Vec<Vec<u8>>,
    #[serde(default)]
    payloads: Vec<Payload>,
    providers: Vec<Option<ContentProvider>>,
}
fn platform(p: &str) -> Result<Provider> {
    match p {
        "modrinth" => Ok(Provider::Modrinth),
        "curseforge" => Ok(Provider::Curseforge),
        _ => Err(invalid()),
    }
}
fn file_error(error: FileError) -> ReleaseError {
    match error {
        FileError::Conflict(_) | FileError::NotFound => ReleaseError::Conflict,
        other => super::super::PackError::from(other).into(),
    }
}
type Selected = BTreeMap<String, (Payload, Option<ContentProvider>)>;
fn insert_selected(
    selected: &mut Selected,
    used: &mut u64,
    path: String,
    bytes: Payload,
    provider: Option<ContentProvider>,
) -> Result<()> {
    let old = selected.get(&path).map_or(0, |(bytes, _)| bytes.size);
    let total = used
        .checked_sub(old)
        .and_then(|n| n.checked_add(bytes.size))
        .ok_or(super::super::PackError::TooLarge)?;
    selected.insert(path, (bytes, provider));
    *used = total;
    Ok(())
}
/// Executable destinations and protected server control files remain hard safety rules.
pub(crate) fn validate_game_path(path: &str) -> Result<()> {
    vesta_protocol::pack::catalogue::RelativePath::parse(path).map_err(|_| ReleaseError::Refusal { code: "pack_path_invalid", rule: "safe_relative_path", item: path.into() })?;
    let lower = path.to_ascii_lowercase();
    if lower.split('/').any(|part| matches!(part, ".vesta" | "wrapper-config.toml" | "vesta-wrapper")) {
        return Err(ReleaseError::Refusal { code: "pack_path_protected", rule: "protected_server_control_file", item: path.into() });
    }
    let executable = [".jar", ".exe", ".dll", ".bat", ".cmd", ".ps1", ".sh"].iter().any(|suffix| lower.ends_with(suffix));
    if executable && !["mods/", "plugins/", "scripts/", "kubejs/"].iter().any(|prefix| lower.starts_with(prefix)) {
        return Err(ReleaseError::Refusal { code: "pack_path_invalid", rule: "executable_directory", item: path.into() });
    }
    Ok(())
}
fn expected_install_directory(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    ["mods/", "config/", "defaultconfigs/", "kubejs/", "scripts/", "resources/"].iter().any(|p| lower.starts_with(p))
        || lower.contains("/datapacks/")
}
pub(crate) fn validate_install_path(path: &str) -> Result<()> { validate_game_path(path) }

impl PackWorkspace {
    pub fn install_finish(&self, s: &Subject, id: &str) -> Result<()> {
        Ok(self.db.write_with(|t| {
            let c = t.connection();
            self.install_authority(c, s)?;
            c.execute(
                "DELETE FROM pack_install_plans WHERE id=? AND principal=? AND device=?",
                params![id, s.principal.as_str(), s.device.as_str()],
            )?;
            Ok(())
        })?)
    }

    pub(super) fn install_authority(&self, c: &Connection, s: &Subject) -> Result<()> {
        let p = identity::policy_in(c, self.db.server_id(), self.db.clock().millis(), Some(s))?;
        if !p.holds_builtin(identity::BuiltinRole::Owner)
            && !p.holds_builtin(identity::BuiltinRole::Administrator)
        {
            return Err(crate::ControlError::forbidden(identity::DenyReason::AdminReserved).into());
        }
        self.authorize_in(c, s, Capability::ContentInstall)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn install_plan(
        &self,
        s: &Subject,
        mc: &str,
        software: Option<&str>,
        request: &PackInstallRequest,
        files: &FileBrowser,
        releases: &super::super::PackReleases,
        providers: &content::UreqProviderClient,
    ) -> Result<PackInstallPlan> {
        #[cfg(test)]
        let timing = {
            let started = Instant::now();
            move |stage: &str| {
                eprintln!("pack_plan {stage}: {:.3}s", started.elapsed().as_secs_f64())
            }
        };
        self.db
            .read_with(|t| self.install_authority(t.connection(), s))?;
        let progress = Preparation {
            db: self.db.clone(),
            job: uuid::Uuid::new_v4().to_string(),
        };
        progress.report("archive", 0, 0)?;
        let payload_store = PayloadStore::open(&self.db).map_err(super::super::PackError::from)?;
        let _ = payload_store.collect();
        let _cleanup = payload_store.cleanup();
        let _payload_lease = payload_store
            .lease()
            .map_err(super::super::PackError::from)?;
        if self
            .server_link(s)?
            .is_some_and(|link| link.pending.is_some())
        {
            return Err(ReleaseError::Conflict);
        }
        let deadline = Instant::now() + Duration::from_secs(3600);
        let bounded_providers = providers
            .with_deadline(deadline)
            .map_err(ReleaseError::from)?;
        let providers = &bounded_providers;
        let http = content::UreqDownloadHttp::for_job();
        let archive_warnings = std::cell::RefCell::new(Vec::new());
        let parse = |b: &Payload, item: &str| {
            archive_warnings.borrow_mut().extend(archive_policy::inspect(
                payload_store.verify(b).map_err(super::super::PackError::from)?, b.size, item)?);
            super::import::parse_spooled(b, &payload_store, providers, |h| {
                Ok(providers.identify_sha512(h).ok().flatten())
            })
        };
        let download_archive = |p: &str, project: &str, version: &str| -> Result<_> {
            let source = providers
                .resolve_with_deadline(platform(p)?, project, version, deadline)
                .map_err(|e| {
                    ReleaseError::provider_file(e, p, project, version, "metadata", None)
                })?;
            let mut stage = payload_store
                .stage()
                .map_err(super::super::PackError::from)?;
            content::fetch_verified_source_into(&source, &http, deadline, &mut stage).map_err(
                |e| {
                    ReleaseError::provider_file(
                        e,
                        p,
                        project,
                        version,
                        "download",
                        Some(&source.filename),
                    ).with_download_url(&source.download_url)
                },
            )?;
            let bytes = stage.finish().map_err(super::super::PackError::from)?;
            Ok((bytes, content::sanitized_download_url(&source.download_url)))
        };
        let provider_archive = |p: &str, project: &str, version: &str| -> Result<_> {
            let (bytes, item) = download_archive(p, project, version)?;
            let (client, client_blobs) = parse(&bytes, &item)?;
            if p != "curseforge" { return Ok((client, client_blobs)); }
            let document = providers.pack_document(Provider::Curseforge, project, Some(version), false)
                .map_err(|e| ReleaseError::provider_file(e, p, project, version, "metadata", None))?;
            let tags = document["gameVersions"].as_array();
            let has_game = tags.is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some(&client.minecraft_version)));
            let has_loader = tags.is_some_and(|tags| tags.iter().any(|tag| tag.as_str().is_some_and(|tag| tag.eq_ignore_ascii_case(client.loader.id()))));
            if !has_game || !has_loader {
                archive_warnings.borrow_mut().push(InstallWarning { code: "version_from_pack_manifest".into(),
                    rule: "pack_manifest_runtime".into(), item: version.into(), detail: "Version from pack manifest".into() });
            }
            let Some(server_id) = document["serverPackFileId"].as_u64().filter(|id| *id > 0) else {
                return Ok((client, client_blobs));
            };
            let server_id = server_id.to_string();
            if server_id == version { return Err(invalid()); }
            let server_document = providers.pack_document(Provider::Curseforge, project, Some(&server_id), false)
                .map_err(|e| ReleaseError::provider_file(e, p, project, &server_id, "metadata", None))?;
            if server_document["isServerPack"] != true { return Err(invalid()); }
            let (server_bytes, server_item) = download_archive(p, project, &server_id)?;
            archive_warnings.borrow_mut().extend(archive_policy::inspect(
                payload_store.verify(&server_bytes).map_err(super::super::PackError::from)?, server_bytes.size, &server_item)?);
            let (mut server, blobs, skipped) = super::import::parse_server_spooled(&server_bytes, &payload_store, providers)?;
            for path in skipped {
                archive_warnings.borrow_mut().push(InstallWarning { code: "server_pack_managed_file_skipped".into(),
                    rule: "managed_server_configuration_or_runtime".into(), item: path,
                    detail: "Kept server-managed configuration and software; configure these through server settings".into() });
            }
            // Ready server folders have no manifest. The client archive remains the
            // authority for runtime and the catalogue keeps the original client ID.
            server.name = client.name;
            server.display_version = client.display_version;
            server.minecraft_version = client.minecraft_version;
            server.loader = client.loader;
            server.loader_version = client.loader_version;
            archive_warnings.borrow_mut().push(InstallWarning { code: "curseforge_server_pack".into(),
                rule: "provider_server_pack".into(), item: server_id, detail: "Using the provider's server pack; client installs retain the client pack reference".into() });
            Ok((server, blobs))
        };
        let mut linked_source = request.source.clone();
        let (mut manifest, blobs) = match &request.source {
            InstallSource::Provider {
                provider,
                project_id,
                version_id,
            } => provider_archive(provider, project_id, version_id)?,
            InstallSource::UploadedArchive { upload_id } => {
                parse(&self.archive_payload(s, upload_id, &payload_store)?, "imported archive")?
            }
            InstallSource::Draft { revision } => {
                let d = self.read(s, mc)?;
                if d.revision != *revision {
                    return Err(ReleaseError::Conflict);
                };
                let retained: Option<String> = self.db.read(|c| c.query_row(
                    "SELECT warnings_json FROM pack_import_warnings WHERE server_id=? AND revision=?",
                    params![self.db.server_id(), revision], |r| r.get(0)).optional().map_err(Into::into))?;
                if let Some(retained) = retained {
                    archive_warnings.borrow_mut().extend(serde_json::from_str::<Vec<InstallWarning>>(&retained).map_err(|_| invalid())?);
                }
                (d.manifest, Vec::new())
            }
            InstallSource::Published { revision: r } => {
                let current = releases.current()?.ok_or(ReleaseError::Conflict)?;
                if current.revision() != revision(r)? {
                    return Err(ReleaseError::Conflict);
                }
                if let Some(p) = self.view(s, mc, releases)?.provider_pack {
                    linked_source = InstallSource::Provider {
                        provider: p.provider.clone(),
                        project_id: p.project_id.clone(),
                        version_id: p.version_id.clone(),
                    };
                    provider_archive(&p.provider, &p.project_id, &p.version_id)?
                } else if let Some((rev, m)) = self.published(s)? {
                    if rev != current.revision() {
                        return Err(ReleaseError::Conflict);
                    };
                    (m, Vec::new())
                } else {
                    let a = releases
                        .artifacts()
                        .load(current.artifact(), super::super::MAX_ARTIFACT_BYTES)?;
                    parse(
                        &payload_store
                            .put(&mut std::io::Cursor::new(a.bytes()))
                            .map_err(super::super::PackError::from)?,
                        "published archive",
                    )?
                }
            }
        };
        if manifest.minecraft_version.is_empty() {
            manifest.minecraft_version = mc.into();
        }
        validate_manifest(&manifest)?;
        #[cfg(test)]
        timing("parsed");
        let blob_map: BTreeMap<String, Payload> =
            blobs.into_iter().map(|b| (b.sha256.clone(), b)).collect();
        let mut selected: Selected = BTreeMap::new();
        let mut selected_bytes = 0;
        let mut skipped = Vec::new();
        let mut warnings = Vec::new();
        let curseforge: Vec<_> = manifest
            .entries
            .iter()
            .filter_map(|entry| match &entry.source {
                EntrySource::Provider {
                    provider,
                    project_id,
                    version_id,
                } if provider == "curseforge" => Some((project_id.clone(), version_id.clone())),
                _ => None,
            })
            .collect();
        providers
            .prefetch_curseforge(&curseforge)
            .map_err(ReleaseError::from)?;
        // Resolve every server candidate before downloading any mod. A restricted
        // or malformed file must not waste the rest of the review's download budget.
        let mut sources = BTreeMap::new();
        progress.report("metadata", 0, manifest.entries.len())?;
        for e in &manifest.entries {
            if e.environment.server == Side::Unsupported
                || matches!(e.kind, EntryKind::ResourcePack | EntryKind::Shader)
            {
                continue;
            }
            if let EntrySource::Provider {
                provider,
                project_id,
                version_id,
            } = &e.source
            {
                let meta = providers.pack_metadata(provider, project_id, version_id)?;
                if meta.server_requirement == Side::Unsupported
                    || matches!(meta.kind, EntryKind::ResourcePack | EntryKind::Shader)
                {
                    continue;
                }
                let source = providers
                    .resolve_with_deadline(platform(provider)?, project_id, version_id, deadline)
                    .map_err(|error| {
                        ReleaseError::provider_file(
                            error,
                            provider,
                            project_id,
                            version_id,
                            "metadata",
                            Some(&meta.filename),
                        )
                    })?;
                if e.path.rsplit('/').next() != Some(content::provider_storage_name(&source.filename).map_err(ReleaseError::from)?.as_str()) {
                    return Err(ReleaseError::provider_file(
                        content::ContentError::InvalidMetadata,
                        provider,
                        project_id,
                        version_id,
                        "metadata",
                        Some(&source.filename),
                    ));
                }
                sources.insert(e.id.clone(), (meta, source));
            }
        }
        #[cfg(test)]
        timing("metadata");
        #[cfg(test)]
        eprintln!(
            "pack_plan metadata requests [GET, POST]: {:?}",
            providers.metadata_requests()
        );
        // Replanning after a software change reuses already verified files. Metadata
        // is still resolved and its authoritative provider digest checked afresh.
        let cached=self.db.read(|c| {
            let row:Option<(String,String)>=c.query_row("SELECT json_extract(body,'$.providers'),coalesce(json_extract(body,'$.payloads'),'[]') FROM pack_install_plans WHERE principal=? AND device=? AND expires>=? ORDER BY expires DESC LIMIT 1",params![s.principal.as_str(),s.device.as_str(),self.db.clock().millis()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            Ok(row)
        })?;
        let mut cached_files = BTreeMap::new();
        if let Some((providers, payloads)) = cached {
            let providers: Vec<Option<ContentProvider>> =
                serde_json::from_str(&providers).map_err(|_| invalid())?;
            let payloads: Vec<Payload> = serde_json::from_str(&payloads).map_err(|_| invalid())?;
            for (provider, payload) in providers.into_iter().zip(payloads) {
                if let Some(p) = provider {
                    cached_files.insert((p.provider, p.project_id, p.version_id), payload);
                }
            }
        }
        for (index, chunk) in manifest.entries.chunks(2).enumerate() {
            progress.report("download_verify", index * 2, manifest.entries.len())?;
            let candidates: Vec<_> = chunk
                .iter()
                .filter_map(|entry| sources.get(&entry.id).map(|(_, source)| (entry, source)))
                .collect();
            let bodies = downloads::pair_map(&candidates, &|(entry, source)| {
                let EntrySource::Provider {
                    provider,
                    project_id,
                    version_id,
                } = &entry.source
                else {
                    return Err(invalid());
                };
                if let Some(payload) =
                    cached_files.get(&(provider.clone(), project_id.clone(), version_id.clone()))
                    && payload_store
                        .verify(payload)
                        .is_ok_and(|file| content::verify_source_reader(source, file).is_ok())
                {
                    return Ok(payload.clone());
                }
                let mut stage = payload_store
                    .stage()
                    .map_err(super::super::PackError::from)?;
                content::fetch_verified_source_into(source, &http, deadline, &mut stage).map_err(
                    |error| {
                        ReleaseError::provider_file(
                            error,
                            provider,
                            project_id,
                            version_id,
                            "download",
                            Some(&source.filename),
                        ).with_download_url(&source.download_url)
                    },
                )?;
                stage
                    .finish()
                    .map_err(|e| super::super::PackError::from(e).into())
            })?;
            let mut downloaded: BTreeMap<_, _> = candidates
                .into_iter()
                .zip(bodies)
                .map(|((entry, _), bytes)| (entry.id.as_str(), bytes))
                .collect();
            for e in chunk {
                if e.environment.server == Side::Unsupported
                    || matches!(e.kind, EntryKind::ResourcePack | EntryKind::Shader)
                {
                    skipped.push(e.path.clone());
                    continue;
                }
                let mut destination = e.path.clone();
                let (bytes, provider) = match &e.source {
                    EntrySource::Upload { sha256 } => {
                        warnings.push(format!("unknown_server_side:{}", e.path));
                        (
                            match blob_map.get(sha256) {
                                Some(b) => b.clone(),
                                None => payload_store
                                    .put(&mut std::io::Cursor::new(
                                        self.db
                                            .read_with(|t| self.blob_in(t.connection(), sha256))?,
                                    ))
                                    .map_err(super::super::PackError::from)?,
                            },
                            None,
                        )
                    }
                    EntrySource::Manual { .. } => {
                        return Err(super::super::PackError::UnresolvedFile.into());
                    }
                    EntrySource::Provider {
                        provider,
                        project_id,
                        version_id,
                    } => {
                        let Some((meta, source)) = sources.get(&e.id) else {
                            skipped.push(e.path.clone());
                            continue;
                        };
                        if meta.kind == EntryKind::Datapack {
                            destination = format!("datapacks/{}", source.filename);
                        }
                        (
                            downloaded.remove(e.id.as_str()).ok_or_else(invalid)?,
                            Some(ContentProvider {
                                provider: provider.clone(),
                                project_id: project_id.clone(),
                                version_id: version_id.clone(),
                            }),
                        )
                    }
                };
                if e.kind == EntryKind::Mod
                    && FileBrowser::pack_jar_side_reader(
                        payload_store
                            .open_file(&bytes)
                            .map_err(super::super::PackError::from)?,
                    )
                    .as_deref()
                        == Some("client")
                {
                    skipped.push(e.path.clone());
                    continue;
                }
                insert_selected(
                    &mut selected,
                    &mut selected_bytes,
                    destination,
                    bytes,
                    provider,
                )?;
            }
        }
        progress.report("review", sources.len(), sources.len())?;
        for root in ["overrides", "server-overrides"] {
            for f in manifest
                .override_files
                .iter()
                .filter(|f| !f.trashed && f.root == root)
            {
                if let Some(hash) = &f.sha256 {
                    let bytes = match blob_map.get(hash) {
                        Some(b) => b.clone(),
                        None => payload_store
                            .put(&mut std::io::Cursor::new(
                                self.db.read_with(|t| self.blob_in(t.connection(), hash))?,
                            ))
                            .map_err(super::super::PackError::from)?,
                    };
                    insert_selected(
                        &mut selected,
                        &mut selected_bytes,
                        f.path.clone(),
                        bytes,
                        None,
                    )?;
                }
            }
        }
        // Install the pack author's pinned set instead of re-resolving provider
        // dependency edges. Projects can move or split by loader: FancyMenu's
        // Konkrete edge names 410295 while this Fabric pack pins 416797, which
        // supplies the same `konkrete` mod. Project-ID comparisons are not a
        // reliable way to diagnose a prebuilt pack's runtime dependencies.
        #[cfg(test)]
        timing("downloads");
        let world = files.world(s).map_err(file_error)?;
        let mut inventory = files
            .content_inventory(s, software.map(str::to_owned), mc.into(), &world)
            .map_err(file_error)?;
        if inventory.truncated {
            return Err(ReleaseError::Conflict);
        }
        // Interactive inventory limits hashing work. A reviewed install must also
        // recognize known projects beyond that preview budget, including large JARs.
        for item in &mut inventory.items {
            content::deadline_check(deadline)?;
            if item.sha256.is_none() {
                match files.open_stream(s, &RelativePath::parse(&item.path).map_err(file_error)?) {
                    Ok(mut stream) => {
                        let hash = hex::encode(
                            stream
                                .snapshot_digest(0, stream.size())
                                .map_err(file_error)?,
                        );
                        item.provider = files
                            .content_provider(&item.path, &hash)
                            .map_err(file_error)?;
                        item.sha256 = Some(hash);
                    }
                    Err(FileError::NotFound) => {}
                    Err(error) => return Err(file_error(error)),
                }
            }
        }
        let by_path: BTreeMap<_, _> = inventory
            .items
            .iter()
            .map(|item| (item.path.as_str(), item))
            .collect();
        let by_project: BTreeMap<_, _> = inventory
            .items
            .iter()
            .filter_map(|item| {
                item.provider
                    .as_ref()
                    .map(|p| ((p.provider.as_str(), p.project_id.as_str()), item))
            })
            .collect();
        let owned = self.server_link(s)?.map(|l| l.files).unwrap_or_default();
        let source_urls: BTreeMap<_, _> = manifest.entries.iter().filter_map(|e| {
            let EntrySource::Provider { provider, project_id, version_id } = &e.source else { return None; };
            sources.get(&e.id).map(|(_, source)| ((provider.as_str(), project_id.as_str(), version_id.as_str()), content::sanitized_download_url(&source.download_url)))
        }).collect();
        let mut review = Vec::new();
        let mut bytes = Vec::new();
        let mut refs = Vec::new();
        let mut destinations = BTreeSet::new();
        for (mut path, (body, provider)) in selected {
            let lower = path.to_ascii_lowercase();
            if lower.starts_with("resourcepacks/")
                || lower.starts_with("shaderpacks/")
                || matches!(
                    lower.as_str(),
                    "options.txt" | "optionsof.txt" | "optionsshaders.txt" | "servers.dat"
                )
            {
                skipped.push(path);
                continue;
            }

            if let Some(relative) = path.strip_prefix("datapacks/") {
                path = format!("{}/datapacks/{relative}", world.name);
            }
            let mut conflict = None;
            if let Some(p) = &provider
                && let Some(old) = by_project
                    .get(&(p.provider.as_str(), p.project_id.as_str()))
                    .filter(|item| {
                        item.provider
                            .as_ref()
                            .is_some_and(|old| old.version_id != p.version_id)
                    })
            {
                path = old.path.clone();
                conflict = Some("project_version".into());
            }
            validate_install_path(&path)?;
            let item = provider.as_ref().and_then(|p| source_urls.get(&(p.provider.as_str(), p.project_id.as_str(), p.version_id.as_str()))).map(String::as_str).unwrap_or(&path);
            archive_warnings.borrow_mut().extend(archive_policy::payload_warnings(&body, item));
            if !matches!(linked_source, InstallSource::Provider { .. }) && !expected_install_directory(&path) {
                warnings.push(format!("unusual_content_directory:{path}"));
            }
            if !destinations.insert(path.to_lowercase()) {
                return Err(ReleaseError::Conflict);
            }
            let expected = if let Some(hash) = by_path
                .get(path.as_str())
                .and_then(|item| item.sha256.as_ref())
            {
                Some(hash.clone())
            } else {
                match files.open_stream(s, &RelativePath::parse(&path).map_err(file_error)?) {
                    Ok(mut stream) => Some(hex::encode(
                        stream
                            .snapshot_digest(0, stream.size())
                            .map_err(file_error)?,
                    )),
                    Err(FileError::NotFound) => None,
                    Err(e) => return Err(file_error(e)),
                }
            };
            if expected.is_some() && conflict.is_none() {
                conflict = Some(
                    if by_path
                        .get(path.as_str())
                        .is_some_and(|i| i.provider.is_none())
                    {
                        "unidentified_jar"
                    } else {
                        "same_filename"
                    }
                    .into(),
                );
            }
            if expected
                .as_ref()
                .is_some_and(|hash| owned.get(&path) == Some(hash))
            {
                conflict = None;
            }
            let sha256 = body.sha256.clone();
            let operation = if expected.is_none() {
                "added"
            } else if expected.as_deref() == Some(sha256.as_str()) {
                "unchanged"
            } else {
                "updated"
            }
            .into();
            review.push(InstallFile {
                operation,
                path,
                sha256,
                expected_sha256: expected,
                conflict,
            });
            bytes.push(body);
            refs.push(provider);
        }
        let current = self.db.read_with(|t| {
            crate::software::SoftwareHandle::pack_install_target_in(t.connection())
        })?;
        let unknown_loader = manifest.loader == ClientLoader::None
            && manifest.entries.iter().any(|e| e.kind == EntryKind::Mod);
        if unknown_loader {
            manifest.loader = match software {
                Some("fabric") => ClientLoader::Fabric,
                Some("quilt") => ClientLoader::Quilt,
                Some("forge") => ClientLoader::Forge,
                Some("neoforge") => ClientLoader::Neoforge,
                _ => ClientLoader::None,
            };
            manifest.loader_version = current.as_ref().and_then(|i| i.loader_version.clone());
            warnings.push("archive_loader_inferred_from_server_review_required".into());
        }
        let valid = !unknown_loader
            || (manifest.loader != ClientLoader::None && manifest.loader_version.is_some());
        if !valid {
            warnings.push("loader_unknown_choose_software_and_replan".into());
        }
        let software_required = if manifest.loader == ClientLoader::None {
            "vanilla"
        } else {
            manifest.loader.id()
        };
        let actual_family = current
            .as_ref()
            .and_then(|i| i.software)
            .and_then(|family| serde_json::to_value(family).ok())
            .and_then(|value| value.as_str().map(str::to_owned));
        let actual_version = current
            .as_ref()
            .and_then(|i| i.version.as_deref())
            .unwrap_or(mc);
        let loader_switch = current
            .as_ref()
            .is_some_and(|i| i.loader_version != manifest.loader_version);
        for i in &inventory.items {
            if i.provider.is_none() {
                warnings.push(format!("retained_unidentified_content:{}", i.path));
            }
        }
        if selected_bytes > 64 * 1024 * 1024 { warnings.push(format!("pack_size_warning:{selected_bytes}")); }
        let mut plan = PackInstallPlan {
            software_switch: None,
            plan_id: uuid::Uuid::new_v4().to_string(),
            confirmation_token: uuid::Uuid::new_v4().to_string(),
            expires_at_ms: self.db.clock().millis() + 900_000,
            minecraft_version: manifest.minecraft_version.clone(),
            software: software_required.into(),
            loader_version: manifest.loader_version.clone(),
            valid,
            software_switch_required: loader_switch
                || actual_version != manifest.minecraft_version
                || actual_family.as_deref().or(software) != Some(software_required),
            files: review,
            skipped,
            warnings: warnings.into_iter().map(InstallWarning::from).chain(archive_warnings.into_inner()).collect(),
            backup_recommended: true,
            removed: owned
                .into_iter()
                .filter(|(path, _)| !destinations.contains(&path.to_lowercase()))
                .collect(),
        };
        plan.warnings.extend(archive_policy::manifest_warnings(&manifest, selected_bytes, &blob_map));
        if plan.software_switch_required {
            plan.warnings.push("software_legacy_checksum_review".to_string().into());
            plan.software_switch = Some(SoftwareSwitch {
                from: SoftwareSwitchTarget {
                    software: actual_family
                        .as_deref()
                        .or(software)
                        .unwrap_or("unknown")
                        .into(),
                    minecraft_version: actual_version.into(),
                    loader_version: current.as_ref().and_then(|i| i.loader_version.clone()),
                },
                to: SoftwareSwitchTarget {
                    software: plan.software.clone(),
                    minecraft_version: plan.minecraft_version.clone(),
                    loader_version: plan.loader_version.clone(),
                },
            });
        }
        let view = self.view(s, mc, releases)?;
        #[cfg(test)]
        timing("review");
        // Pin the client projection beyond upload/plan expiry. Provider links keep IDs only.
        let client_archive = if matches!(linked_source, InstallSource::Provider { .. }) {
            None
        } else {
            let captured = (|| -> Result<Vec<u8>> {
                let client = self.client_manifest(&manifest)?;
                let metadata = crate::config::PackMetadata::linked(&client, 1, 1, None);
                let mut memory_blobs = BTreeMap::new();
                let mut memory_size = 0u64;
                for (hash, payload) in &blob_map {
                    memory_size = memory_size.saturating_add(payload.size);
                    if memory_size > MAX_BLOBS as u64 {
                        return Err(super::super::PackError::TooLarge.into());
                    }
                    let mut file = payload_store
                        .verify(payload)
                        .map_err(super::super::PackError::from)?;
                    let mut body = Vec::new();
                    std::io::Read::read_to_end(&mut file, &mut body).map_err(|_| invalid())?;
                    memory_blobs.insert(hash.clone(), body);
                }
                let snapshot =
                    self.snapshot(&client, &metadata, providers, &http, &memory_blobs)?;
                let env = client
                    .entries
                    .iter()
                    .map(|e| {
                        (
                            e.path.clone(),
                            (
                                if e.required { "required" } else { "optional" }.into(),
                                "unsupported".into(),
                            ),
                        )
                    })
                    .collect();
                Ok(super::super::build::build_vesta_with_environments(
                    &snapshot,
                    &metadata,
                    super::super::VestaBuildLimits::default(),
                    &env,
                    None,
                )
                .map_err(|_| invalid())?
                .bytes()
                .to_vec())
            })();
            match captured {
                Ok(bytes) => Some(bytes),
                Err(error) => {
                    plan.warnings
                        .push(format!("client_delivery_unavailable:{}", error.code()).into());
                    None
                }
            }
        };
        let mut stored_plan = plan.clone();
        stored_plan.confirmation_token.clear();
        let stored = Stored {
            applied_key: None,
            client_archive,
            expected_publication: if let InstallSource::Published { revision } = &request.source {
                Some(revision.clone())
            } else {
                None
            },
            expected_link: self.server_link(s)?,
            manifest,
            source: linked_source,
            draft_revision: view.draft.revision,
            published_revision: view.published_revision,
            plan: stored_plan,
            payloads: bytes,
            bytes: Vec::new(),
            providers: refs,
        };
        #[cfg(test)]
        timing("client_capture");
        // Encoding verified blobs is CPU work, not part of the write transaction.
        let stored_body = serde_json::to_string(&stored).map_err(|_| invalid())?;
        #[cfg(test)]
        timing("encoded");
        self.db.write_with(|t| {
            let c = t.connection();
            self.install_authority(c, s)?;
            c.execute(
                "DELETE FROM pack_install_plans WHERE expires<? OR (principal=? AND device=?)",
                params![
                    self.db.clock().millis(),
                    s.principal.as_str(),
                    s.device.as_str()
                ],
            )?;
            let count: i64 =
                c.query_row("SELECT count(*) FROM pack_install_plans", [], |r| r.get(0))?;
            if count >= 64 {
                return Err(super::super::PackError::TooLarge.into());
            }
            c.execute(
                "INSERT INTO pack_install_plans VALUES(?,?,?,?,?,?)",
                params![
                    plan.plan_id,
                    s.principal.as_str(),
                    s.device.as_str(),
                    plan.expires_at_ms,
                    digest(plan.confirmation_token.as_bytes()),
                    stored_body
                ],
            )?;
            self.audit.append(
                c,
                &AuditRecord::new(
                    AuditActor::new(
                        s.principal.as_str().into(),
                        s.device.as_str().into(),
                        "pack-install".into(),
                    )?,
                    "pack_install.plan".into(),
                    format!("pack-install:{}", plan.plan_id),
                    "success".into(),
                    None,
                    [],
                )?,
            )?;
            Ok(())
        })?;
        #[cfg(test)]
        timing("stored");
        Ok(plan)
    }
    pub fn install_switch_review(
        &self,
        s: &Subject,
        input: &PackInstallApply,
    ) -> Result<PackInstallPlan> {
        Ok(self.db.read_with(|t| {
            let c = t.connection(); self.install_authority(c, s)?;
            let row: Option<(i64, String, String)> = c.query_row("SELECT expires,token_hash,body FROM pack_install_plans WHERE id=? AND principal=? AND device=?", params![input.plan_id,s.principal.as_str(),s.device.as_str()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let (expires, hash, body) = row.ok_or(ReleaseError::Conflict)?;
            if expires < self.db.clock().millis() || hash != digest(input.confirmation_token.as_bytes()) { return Err(ReleaseError::Conflict); }
            let stored: Stored = serde_json::from_str(&body).map_err(|_| invalid())?;
            if (warnings_require_acknowledgement(&stored.plan.warnings) && !input.acknowledge_warnings) || !stored.plan.valid || !input.acknowledge_software_switch { return Err(ReleaseError::Conflict); }
            let conflicts: BTreeSet<_> = stored.plan.files.iter().filter(|f| f.conflict.is_some()).map(|f| f.path.as_str()).collect();
            let choices: BTreeSet<_> = input.choices.iter().map(|c| c.path.as_str()).collect();
            if choices.len() != input.choices.len() || choices != conflicts || input.choices.iter().any(|c| c.choice != "keep" && c.choice != "replace") { return Err(invalid()); }
            Ok(stored.plan)
        })?)
    }
    pub fn install_apply(
        &self,
        s: &Subject,
        mc: &str,
        software: Option<&str>,
        key: &str,
        input: &PackInstallApply,
        files: &FileBrowser,
    ) -> Result<PackInstallOutcome> {
        self.db
            .read_with(|t| self.install_authority(t.connection(), s))?;
        let payload_store = PayloadStore::open(&self.db).map_err(super::super::PackError::from)?;
        let _ = payload_store.collect();
        let _cleanup = payload_store.cleanup();
        let _payload_lease = payload_store
            .lease()
            .map_err(super::super::PackError::from)?;
        let _guard = files.pack_admission().map_err(file_error)?;
        self.mutate(s,key,input,|c|{
            self.install_authority(c,s)?;
            let row:Option<(i64,String,String)>=c.query_row("SELECT expires,token_hash,body FROM pack_install_plans WHERE id=? AND principal=? AND device=?",params![input.plan_id,s.principal.as_str(),s.device.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let (expires,hash,body)=row.ok_or(ReleaseError::Conflict)?;
            if expires<self.db.clock().millis()||hash!=digest(input.confirmation_token.as_bytes()){return Err(ReleaseError::Conflict)}
            let mut stored:Stored=serde_json::from_str(&body).map_err(|_|invalid())?;
            if stored.payloads.is_empty() {
                for bytes in std::mem::take(&mut stored.bytes) {
                    stored.payloads.push(payload_store.put(&mut std::io::Cursor::new(bytes)).map_err(super::super::PackError::from)?);
                }
            }
            if stored.payloads.len()!=stored.plan.files.len() || stored.providers.len()!=stored.plan.files.len() {return Err(invalid());}
            if stored.payloads.iter().zip(&stored.plan.files).any(|(p,f)|p.sha256!=f.sha256) {return Err(ReleaseError::Conflict);}
            if !stored.plan.valid{return Err(ReleaseError::Conflict)}
            if warnings_require_acknowledgement(&stored.plan.warnings) && !input.acknowledge_warnings {return Err(invalid());}
            if serde_json::to_value(self.server_link_in(c)?).map_err(|_|invalid())? != serde_json::to_value(&stored.expected_link).map_err(|_|invalid())? {return Err(ReleaseError::Conflict)}
            if stored.expected_publication.is_some() || matches!(stored.source,InstallSource::Published{..}) {
                let published:i64=c.query_row("SELECT coalesce(max(revision),0) FROM pack_releases WHERE server_id=?",[self.db.server_id()],|r|r.get(0))?;
                if published as u64!=revision(&stored.published_revision)?{return Err(ReleaseError::Conflict)}

            }

            if let Some(current)=crate::software::SoftwareHandle::pack_install_target_in(c)? {
                let family=current.software.and_then(|f|serde_json::to_value(f).ok()).and_then(|v|v.as_str().map(str::to_owned));
                if current.version.as_deref()!=Some(stored.plan.minecraft_version.as_str())||current.loader_version!=stored.plan.loader_version||family.as_deref()!=Some(stored.plan.software.as_str()){return Err(ReleaseError::Conflict)}
            }
            if mc!=stored.plan.minecraft_version||software!=Some(stored.plan.software.as_str()){return Err(ReleaseError::Conflict)}
            if stored.applied_key.as_deref().is_some_and(|k|k!=key){return Err(ReleaseError::Conflict)}
            let conflicts:BTreeSet<_>=stored.plan.files.iter().filter(|f|f.conflict.is_some()).map(|f|f.path.clone()).collect();
            let choices:BTreeMap<_,_>=input.choices.iter().map(|c|(c.path.clone(),c.choice.clone())).collect();
            if choices.len()!=input.choices.len()||conflicts!=choices.keys().cloned().collect()||choices.values().any(|v|v!="replace"&&v!="keep"){return Err(invalid())}
            let link_files = stored.plan.files.iter().filter(|f| choices.get(&f.path).is_none_or(|v|v!="keep")).map(|f|(f.path.clone(), f.sha256.clone())).collect();
            let selected:Vec<_>=stored.plan.files.clone().into_iter().zip(stored.payloads).zip(stored.providers).filter_map(|((f,b),p)|{if choices.get(&f.path).is_some_and(|v|v=="keep"){None}else{Some((f.path,f.expected_sha256,b,p))}}).collect();
            let mut pending_ids=files.stage_pack_payloads_in(c,s,key,&selected).map_err(file_error)?;
            pending_ids.extend(files.stage_pack_removals_in(c,s,&format!("{key}-remove"),&stored.plan.removed).map_err(file_error)?);
            if let Some(archive) = &stored.client_archive {
                c.execute("INSERT OR REPLACE INTO server_modpack_delivery VALUES(?,?,?)", params![self.db.server_id(), serde_json::to_string(&stored.source).map_err(|_|invalid())?, archive])?;
            }
            let installed = self.server_link_in(c)?.and_then(|link| link.installed);
            // Retain only the confirmed and incoming client projections, independently
            // of expiring uploads/plans and of the editable custom workspace.
            let prior_source=installed.as_ref().map(|v|serde_json::to_string(&v.source)).transpose().map_err(|_|invalid())?.unwrap_or_default();
            c.execute("DELETE FROM server_modpack_delivery WHERE server_id=? AND source NOT IN (?,?)",params![self.db.server_id(),prior_source,serde_json::to_string(&stored.source).map_err(|_|invalid())?])?;
            let pending = Some(ServerModpackVersion { source: stored.source.clone(), display_version: stored.manifest.display_version.clone() });
            let link=ServerModpackLink{source:stored.source,display_version:stored.manifest.display_version,files:link_files,state:"staged_until_restart".into(),pending_ids:pending_ids.clone(),installed,pending};
            c.execute("INSERT OR REPLACE INTO server_modpack_link VALUES(?,?)",params![self.db.server_id(),serde_json::to_string(&link).map_err(|_|invalid())?])?;
            c.execute("DELETE FROM server_modpack_files",[])?;
            for (path,hash) in &link.files {c.execute("INSERT INTO server_modpack_files VALUES(?,?)",params![path,hash])?;}

            // The durable mutation receipt owns replay after this transaction.
            // Staged files and optional client delivery already own the bytes.
            // Do not re-encode and retain another whole pack for thirty days,
            // occupying both the writer and one of the eight review slots.
            c.execute("DELETE FROM pack_install_plans WHERE id=?",[&input.plan_id])?;

            Ok(PackInstallOutcome{software_job_id:None,state:"staged_until_restart".into(),pending_ids})
        })
    }
}

#[cfg(test)]
mod triage_live {
    use super::*;
    #[test]
    #[ignore = "live CurseForge RLCraft review; requires configured application key"]
    fn rlcraft_4612979_review() {
        let f = super::super::tests::Fixture::new();
        let game = f.temp.path().join("game");
        std::fs::create_dir_all(&game).unwrap();
        let files = FileBrowser::open(f.db.clone(), &game, &[]).unwrap();
        let releases = super::super::super::PackReleases::open(&f.db).unwrap();
        let plan = f.workspace.install_plan(&f.subject, "1.21.1", Some("fabric"),
            &PackInstallRequest { source: InstallSource::Provider { provider: "curseforge".into(), project_id: "285109".into(), version_id: "4612979".into() } },
            &files, &releases, &content::UreqProviderClient::configured()).unwrap_or_else(|error| {
                if let ReleaseError::Refusal { code, rule, item } = &error { panic!("{code}: {rule}: {item}"); }
                panic!("{error:?}")
            });
        assert!(plan.valid);
        assert_eq!(plan.minecraft_version, "1.12.2");
        assert_eq!(plan.software, "forge");
        assert_eq!(plan.loader_version.as_deref(), Some("14.23.5.2860"));
        assert!(plan.warnings.iter().any(|w| w.code == "version_from_pack_manifest"));
        assert!(plan.warnings.iter().any(|w| w.code == "curseforge_server_pack" && w.item == "4612990"));
        assert!(plan.files.iter().any(|f| f.path.starts_with("mods/")));
        eprintln!("RLCRAFT plan: valid={}, runtime={}/{}/{:?}, files={}", plan.valid, plan.minecraft_version, plan.software, plan.loader_version, plan.files.len());
    }
    #[test]
    #[ignore = "real CurseForge metadata and pack download; configured application key required"]
    fn cobbleverse_1_7_42_review() {
        let f = super::super::tests::Fixture::new();
        let game = f.temp.path().join("game");
        std::fs::create_dir_all(&game).unwrap();
        let files = FileBrowser::open(f.db.clone(), &game, &[]).unwrap();
        let releases = super::super::super::PackReleases::open(&f.db).unwrap();
        let result = f.workspace.install_plan(&f.subject, "1.21.1", Some("fabric"),
            &PackInstallRequest { source: InstallSource::Provider { provider: "curseforge".into(), project_id: "1210677".into(), version_id: "8488030".into() } },
            &files, &releases, &content::UreqProviderClient::configured());
        match &result {
            Ok(plan) => eprintln!("COBBLEVERSE review: valid={}, files={}, warnings={:?}", plan.valid, plan.files.len(), plan.warnings),
            Err(ReleaseError::Refusal {code, rule, item}) => eprintln!("COBBLEVERSE refusal: {code}, {rule}, {item}"),
            Err(ReleaseError::ProviderFile {failure, details}) => eprintln!("COBBLEVERSE provider refusal: {}, stage={}, reason={}, file={:?}", failure.code(), details.stage, details.reason, details.filename),
            Err(error) => eprintln!("COBBLEVERSE refusal: {}", error.code()),
        }
        result.unwrap();
    }
}

#[cfg(test)]
mod safety_review_tests {
    use super::*;
    #[test]
    fn unusual_data_is_reviewable_but_executable_and_escape_rules_are_hard() {
        assert!(validate_game_path("licenses/notice.pdf").is_ok());
        assert!(validate_game_path("mods/mod.jar").is_ok());
        for (path, expected) in [("../outside.jar", "safe_relative_path"), ("config/run.exe", "executable_directory"), (".vesta/identity", "protected_server_control_file")] {
            match validate_game_path(path).unwrap_err() {
                ReleaseError::Refusal { rule, item, .. } => { assert_eq!(rule, expected); assert_eq!(item, path); },
                other => panic!("missing issue: {other:?}"),
            }
        }
        let warning: InstallWarning = "unusual_content_directory:licenses/notice.pdf".to_string().into();
        assert_eq!(warning.item, "licenses/notice.pdf");
        let json = serde_json::to_value(warning).unwrap();
        assert_eq!(json["code"], "unusual_content_directory");
        assert_eq!(json["rule"], "unusual_content_directory");
    }
}
