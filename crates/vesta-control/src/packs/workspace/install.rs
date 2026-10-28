//! Whole-pack review pins verified bytes and file preconditions before staging.
use super::*;
use crate::{
    content::{self, ContentProvider, Provider, ProviderClient},
    files::{FileBrowser, FileError, RelativePath},
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
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
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackInstallPlan {
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
    pub plan_id: String,
    pub confirmation_token: String,
    /// Exactly one replace/keep choice per conflicting destination.
    pub choices: Vec<InstallChoice>,
    pub set_as_modpack: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackInstallOutcome {
    pub state: String,
    pub pending_ids: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    applied_key: Option<String>,
    manifest: PackManifest,
    source: InstallSource,
    draft_revision: String,
    published_revision: String,
    blobs: Vec<Vec<u8>>,
    plan: PackInstallPlan,
    bytes: Vec<Vec<u8>>,
    providers: Vec<Option<ContentProvider>>,
}
fn platform(p: &str) -> Result<Provider> {
    match p {
        "modrinth" => Ok(Provider::Modrinth),
        "curseforge" => Ok(Provider::Curseforge),
        _ => Err(invalid()),
    }
}
fn file_error(_: FileError) -> ReleaseError {
    ReleaseError::Conflict
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

    fn install_authority(&self, c: &Connection, s: &Subject) -> Result<()> {
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
        self.db
            .read_with(|t| self.install_authority(t.connection(), s))?;
        let deadline = Instant::now() + Duration::from_secs(120);
        let parse = |b: &[u8]| {
            super::import::parse_archive(b, providers, |h| {
                Ok(providers.identify_sha512(h).ok().flatten())
            })
        };
        let provider_archive = |p: &str, project: &str, version: &str| -> Result<_> {
            let source = providers
                .resolve_with_deadline(platform(p)?, project, version, deadline)
                .map_err(|_| invalid())?;
            let bytes = content::fetch_verified_source(
                &source,
                &content::UreqDownloadHttp::default(),
                deadline,
            )
            .map_err(|_| invalid())?;
            parse(&bytes)
        };
        let (mut manifest, blobs) = match &request.source {
            InstallSource::Provider {
                provider,
                project_id,
                version_id,
            } => provider_archive(provider, project_id, version_id)?,
            InstallSource::UploadedArchive { upload_id } => {
                parse(&self.archive_upload(s, upload_id)?)?
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
                    parse(a.bytes())?
                }
            }
        };
        if manifest.minecraft_version.is_empty() {
            manifest.minecraft_version = mc.into();
        }
        validate_manifest(&manifest)?;
        let retained_blobs = blobs.clone();
        let blob_map: BTreeMap<String, Vec<u8>> =
            blobs.into_iter().map(|b| (digest(&b), b)).collect();
        let mut selected: BTreeMap<String, (Vec<u8>, Option<ContentProvider>)> = BTreeMap::new();
        let mut skipped = Vec::new();
        let mut warnings = Vec::new();
        for e in &manifest.entries {
            if e.environment.server == Side::Unsupported
                || matches!(e.kind, EntryKind::ResourcePack | EntryKind::Shader)
            {
                skipped.push(e.path.clone());
                continue;
            }
            let (bytes, provider) = match &e.source {
                EntrySource::Upload { sha256 } => {
                    warnings.push(format!("unknown_server_side:{}", e.path));
                    (
                        match blob_map.get(sha256) {
                            Some(b) => b.clone(),
                            None => self
                                .db
                                .read_with(|t| self.blob_in(t.connection(), sha256))?,
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
                    let meta = providers.pack_metadata(provider, project_id, version_id)?;
                    if meta.server_requirement == Side::Unsupported {
                        skipped.push(e.path.clone());
                        continue;
                    }
                    if meta.server_requirement == Side::Optional {
                        warnings.push(format!("unknown_or_optional_server_side:{}", e.path));
                    }
                    let source = providers
                        .resolve_with_deadline(
                            platform(provider)?,
                            project_id,
                            version_id,
                            deadline,
                        )
                        .map_err(|_| invalid())?;
                    if e.path.rsplit('/').next() != Some(source.filename.as_str()) {
                        return Err(ReleaseError::Conflict);
                    }
                    (
                        content::fetch_verified_source(
                            &source,
                            &content::UreqDownloadHttp::default(),
                            deadline,
                        )
                        .map_err(|_| invalid())?,
                        Some(ContentProvider {
                            provider: provider.clone(),
                            project_id: project_id.clone(),
                            version_id: version_id.clone(),
                        }),
                    )
                }
            };
            if e.kind == EntryKind::Mod
                && FileBrowser::pack_jar_side(&bytes).as_deref() == Some("client")
            {
                skipped.push(e.path.clone());
                continue;
            }
            selected.insert(e.path.clone(), (bytes, provider));
        }
        for root in ["overrides", "server-overrides"] {
            for f in manifest
                .override_files
                .iter()
                .filter(|f| !f.trashed && f.root == root)
            {
                if let Some(hash) = &f.sha256 {
                    let bytes = match blob_map.get(hash) {
                        Some(b) => b.clone(),
                        None => self.db.read_with(|t| self.blob_in(t.connection(), hash))?,
                    };
                    selected.insert(f.path.clone(), (bytes, None));
                }
            }
        }
        if selected.len() > 128 || selected.values().map(|v| v.0.len()).sum::<usize>() > MAX_BLOBS {
            return Err(super::super::PackError::TooLarge.into());
        }
        let world = files.world(s).map_err(file_error)?;
        let inventory = files
            .content_inventory(s, software.map(str::to_owned), mc.into(), &world)
            .map_err(file_error)?;
        if inventory.truncated {
            return Err(ReleaseError::Conflict);
        }
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
                && let Some(old) = inventory.items.iter().find(|i| {
                    i.provider.as_ref().is_some_and(|v| {
                        v.provider == p.provider
                            && v.project_id == p.project_id
                            && v.version_id != p.version_id
                    })
                })
            {
                path = old.path.clone();
                conflict = Some("project_version".into());
            }
            validate_install_path(&path)?;
            if !destinations.insert(path.to_lowercase()) {
                return Err(ReleaseError::Conflict);
            }
            let expected =
                match files.open_stream(s, &RelativePath::parse(&path).map_err(file_error)?) {
                    Ok(mut stream) => Some(hex::encode(
                        stream
                            .snapshot_digest(0, stream.size())
                            .map_err(file_error)?,
                    )),
                    Err(FileError::NotFound) => None,
                    Err(e) => return Err(file_error(e)),
                };
            if expected.is_some() && conflict.is_none() {
                conflict = Some(
                    if inventory
                        .items
                        .iter()
                        .any(|i| i.path == path && i.provider.is_none())
                    {
                        "unidentified_jar"
                    } else {
                        "same_filename"
                    }
                    .into(),
                );
            }
            review.push(InstallFile {
                path,
                sha256: digest(&body),
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
        let loader_switch = current.is_some_and(|i| i.loader_version != manifest.loader_version);
        for i in &inventory.items {
            if i.provider.is_none() {
                warnings.push(format!("retained_unidentified_content:{}", i.path));
            }
        }
        let plan = PackInstallPlan {
            plan_id: uuid::Uuid::new_v4().to_string(),
            confirmation_token: uuid::Uuid::new_v4().to_string(),
            expires_at_ms: self.db.clock().millis() + 900_000,
            minecraft_version: manifest.minecraft_version.clone(),
            software: software_required.into(),
            loader_version: manifest.loader_version.clone(),
            valid,
            software_switch_required: loader_switch
                || mc != manifest.minecraft_version
                || software != Some(software_required),
            files: review,
            skipped,
            warnings,
            backup_recommended: true,
        };
        let mut stored_plan = plan.clone();
        stored_plan.confirmation_token.clear();
        let view = self.view(s, mc, releases)?;
        let stored = Stored {
            applied_key: None,
            manifest,
            source: request.source.clone(),
            draft_revision: view.draft.revision,
            published_revision: view.published_revision,
            blobs: retained_blobs,
            plan: stored_plan,
            bytes,
            providers: refs,
        };
        self.db.write_with(|t| {
            let c = t.connection();
            self.install_authority(c, s)?;
            c.execute(
                "DELETE FROM pack_install_plans WHERE expires<?",
                [self.db.clock().millis()],
            )?;
            let count: i64 =
                c.query_row("SELECT count(*) FROM pack_install_plans", [], |r| r.get(0))?;
            if count >= 8 {
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
                    serde_json::to_string(&stored).map_err(|_| invalid())?
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
        Ok(plan)
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
        let _guard = files.pack_admission().map_err(file_error)?;
        self.mutate(s,key,input,|c|{
            self.install_authority(c,s)?;
            let row:Option<(i64,String,String)>=c.query_row("SELECT expires,token_hash,body FROM pack_install_plans WHERE id=? AND principal=? AND device=?",params![input.plan_id,s.principal.as_str(),s.device.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let (expires,hash,body)=row.ok_or(ReleaseError::Conflict)?;
            if expires<self.db.clock().millis()||hash!=digest(input.confirmation_token.as_bytes()){return Err(ReleaseError::Conflict)}
            let mut stored:Stored=serde_json::from_str(&body).map_err(|_|invalid())?;
            if !stored.plan.valid{return Err(ReleaseError::Conflict)}
            if input.set_as_modpack || matches!(stored.source,InstallSource::Published{..}) {
                let published:i64=c.query_row("SELECT coalesce(max(revision),0) FROM pack_releases WHERE server_id=?",[self.db.server_id()],|r|r.get(0))?;
                if published as u64!=revision(&stored.published_revision)?{return Err(ReleaseError::Conflict)}
                if input.set_as_modpack && self.read_in(c,mc)?.revision!=stored.draft_revision{return Err(ReleaseError::Conflict)}
            }

            if let Some(current)=crate::software::SoftwareHandle::pack_install_target_in(c)? {
                let family=current.software.and_then(|f|serde_json::to_value(f).ok()).and_then(|v|v.as_str().map(str::to_owned));
                if current.version.as_deref()!=Some(stored.plan.minecraft_version.as_str())||current.loader_version!=stored.plan.loader_version||family.as_deref()!=Some(stored.plan.software.as_str()){return Err(ReleaseError::Conflict)}
            }
            if mc!=stored.plan.minecraft_version||software!=Some(stored.plan.software.as_str()){return Err(ReleaseError::Conflict)}
            if stored.applied_key.as_deref().is_some_and(|k|k!=key){return Err(ReleaseError::Conflict)}
            stored.applied_key=Some(key.into());
            c.execute("UPDATE pack_install_plans SET body=? WHERE id=?",params![serde_json::to_string(&stored).map_err(|_|invalid())?,input.plan_id])?;
            let conflicts:BTreeSet<_>=stored.plan.files.iter().filter(|f|f.conflict.is_some()).map(|f|f.path.clone()).collect();
            let choices:BTreeMap<_,_>=input.choices.iter().map(|c|(c.path.clone(),c.choice.clone())).collect();
            if choices.len()!=input.choices.len()||conflicts!=choices.keys().cloned().collect()||choices.values().any(|v|v!="replace"&&v!="keep"){return Err(invalid())}
            let selected:Vec<_>=stored.plan.files.into_iter().zip(stored.bytes).zip(stored.providers).filter_map(|((f,b),p)|{if choices.get(&f.path).is_some_and(|v|v=="keep"){None}else{Some((f.path,f.expected_sha256,b,p))}}).collect();
            c.execute("UPDATE pack_install_plans SET expires=? WHERE id=?",params![self.db.clock().millis()+2_592_000_000i64,input.plan_id])?;
            let pending_ids=files.stage_pack_in(c,s,key,&selected).map_err(file_error)?;
            Ok(PackInstallOutcome{state:"staged_until_restart".into(),pending_ids})
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum Assignment {
    Provider(ProviderPackPublish),
    Custom(WorkspacePublish),
    Current(String),
}
impl PackWorkspace {
    /// Assignment is a resumable second step; stage receipt is retained if publication refuses.
    pub fn install_assign(
        &self,
        s: &Subject,
        mc: &str,
        input: &PackInstallApply,
        releases: &super::super::PackReleases,
        providers: &content::UreqProviderClient,
        base: &crate::config::PackMetadata,
    ) -> Result<String> {
        let key = format!("install-assign-{}", input.plan_id);
        let assignment: Assignment = if let Some(a) = self.replay(s, &key, input)? {
            a
        } else {
            let stored:Stored=self.db.read_with(|t|{let c=t.connection();self.install_authority(c,s)?;let raw:String=c.query_row("SELECT body FROM pack_install_plans WHERE id=? AND principal=? AND device=? AND token_hash=?",params![input.plan_id,s.principal.as_str(),s.device.as_str(),digest(input.confirmation_token.as_bytes())],|r|r.get(0))?;serde_json::from_str(&raw).map_err(|_|invalid())})?;
            let prepared = match &stored.source {
                InstallSource::Published { revision } => Assignment::Current(revision.clone()),
                InstallSource::Provider {
                    provider,
                    project_id,
                    version_id,
                } => Assignment::Provider(ProviderPackPublish {
                    expected_revision: stored.published_revision,
                    draft_revision: stored.draft_revision,
                    provider: provider.clone(),
                    project_id: project_id.clone(),
                    version_id: version_id.clone(),
                    required_update: false,
                    acknowledge_lower_version: false,
                }),
                _ => {
                    let draft: PackDraft =
                        self.mutate(s, &format!("install-draft-{}", input.plan_id), input, |c| {
                            self.install_authority(c, s)?;
                            for b in &stored.blobs {
                                self.store_blob(c, b)?;
                            }
                            self.write_in(c, s, mc, &stored.draft_revision, stored.manifest.clone())
                        })?;
                    let preview = self.preview(
                        s,
                        mc,
                        &PreviewRequest {
                            draft_revision: draft.revision.clone(),
                            resolved_entries: Vec::new(),
                        },
                        releases,
                        providers,
                        Vec::new(),
                    )?;
                    if !preview.valid {
                        return Err(ReleaseError::Conflict);
                    }
                    Assignment::Custom(WorkspacePublish {
                        expected_revision: stored.published_revision,
                        draft_revision: draft.revision,
                        resolved_entries: Vec::new(),
                        review_sha256: preview.review_sha256,
                        required_update: false,
                        acknowledge_lower_version: false,
                    })
                }
            };
            self.mutate(s, &key, input, |_| Ok(prepared))?
        };
        let publish_key = format!("install-publish-{}", input.plan_id);
        match assignment {
            Assignment::Current(r) => Ok(r),
            Assignment::Provider(p) => self
                .publish_provider_pack(s, mc, &publish_key, &p, releases)
                .map(|o| o.hosted_revision.to_string()),
            Assignment::Custom(p) => self
                .publish(
                    s,
                    mc,
                    &publish_key,
                    &p,
                    releases,
                    providers,
                    &content::UreqDownloadHttp::default(),
                    base,
                    Vec::new(),
                )
                .map(|o| o.hosted_revision.to_string()),
        }
    }
}
