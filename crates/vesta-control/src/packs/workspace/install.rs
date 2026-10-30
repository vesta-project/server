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
    pub warnings: Vec<String>,
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
    if selected.len() + usize::from(!selected.contains_key(&path))
        > vesta_protocol::pack::catalogue::MAX_FILES
    {
        return Err(super::super::PackError::TooLarge.into());
    }
    selected.insert(path, (bytes, provider));
    *used = total;
    Ok(())
}
pub(crate) fn validate_install_path(path: &str) -> Result<()> {
    vesta_protocol::pack::catalogue::RelativePath::parse(path).map_err(|_| invalid())?;
    let lower = path.to_ascii_lowercase();
    let allowed = [
        "mods/",
        "config/",
        "defaultconfigs/",
        "kubejs/",
        "scripts/",
        "resources/",
    ]
    .iter()
    .any(|p| lower.starts_with(p))
        || (lower.contains("/datapacks/")
            && lower.ends_with(".zip")
            && lower.matches('/').count() == 2);
    if !allowed {
        return Err(invalid());
    }
    Ok(())
}
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
        let parse = |b: &Payload| {
            super::import::parse_spooled(b, &payload_store, providers, |h| {
                Ok(providers.identify_sha512(h).ok().flatten())
            })
        };
        let provider_archive = |p: &str, project: &str, version: &str| -> Result<_> {
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
                    )
                },
            )?;
            let bytes = stage.finish().map_err(super::super::PackError::from)?;
            parse(&bytes)
        };
        let mut linked_source = request.source.clone();
        let (mut manifest, blobs) = match &request.source {
            InstallSource::Provider {
                provider,
                project_id,
                version_id,
            } => provider_archive(provider, project_id, version_id)?,
            InstallSource::UploadedArchive { upload_id } => {
                parse(&self.archive_payload(s, upload_id, &payload_store)?)?
            }
            InstallSource::Draft { revision } => {
                let d = self.read(s, mc)?;
                if d.revision != *revision {
                    return Err(ReleaseError::Conflict);
                };
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
                        )
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
        let selected_projects: BTreeSet<_> = selected
            .values()
            .filter_map(|(_, p)| {
                p.as_ref()
                    .map(|p| (p.provider.as_str(), p.project_id.as_str()))
            })
            .collect();
        for (path, (_, provider)) in &selected {
            let Some(provider) = provider else {
                continue;
            };
            if let Some((meta,_))=manifest.entries.iter().find(|e|matches!(&e.source,EntrySource::Provider{provider:p,project_id,version_id} if p==&provider.provider&&project_id==&provider.project_id&&version_id==&provider.version_id)).and_then(|e|sources.get(&e.id)) {
                for dependency in &meta.dependencies {
                    if !dependency.incompatible && !dependency.project_id.is_empty()
                        && !selected_projects.contains(&(provider.provider.as_str(),dependency.project_id.as_str()))
                        && !manifest.entries.iter().any(|e|matches!(&e.source,EntrySource::Provider{provider:p,project_id,..} if p==&provider.provider && project_id==&dependency.project_id)) {
                        warnings.push(format!("missing_required_dependency:{path} ({}/{})",provider.provider,dependency.project_id));
                    }
                }
            }
        }
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
            warnings,
            backup_recommended: true,
            removed: owned
                .into_iter()
                .filter(|(path, _)| !destinations.contains(&path.to_lowercase()))
                .collect(),
        };
        if plan.software_switch_required {
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
                        .push(format!("client_delivery_unavailable:{}", error.code()));
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
            if (!stored.plan.warnings.is_empty() && !input.acknowledge_warnings) || !stored.plan.valid || !input.acknowledge_software_switch { return Err(ReleaseError::Conflict); }
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
            if !stored.plan.warnings.is_empty() && !input.acknowledge_warnings {return Err(invalid());}
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
