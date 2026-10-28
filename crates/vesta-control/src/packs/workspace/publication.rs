use super::super::{
    PackMutation, PackOutcome, PackReleases, PreparedPack, PreparedSnapshot, UpdatePolicy,
};
use super::*;
use crate::{
    config::PackMetadata,
    content::{self, DownloadHttp, Provider},
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use vesta_protocol::pack::catalogue::{
    FileSource, Manifest, ManifestMetadata, PackContent, PackFile, Platform, RelativePath,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewRequest {
    pub draft_revision: String,
    #[serde(default)]
    pub resolved_entries: Vec<PackEntry>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePublish {
    pub expected_revision: String,
    pub draft_revision: String,
    #[serde(default)]
    pub resolved_entries: Vec<PackEntry>,
    pub review_sha256: String,
    pub required_update: bool,
    #[serde(default)]
    pub acknowledge_lower_version: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct PackIssue {
    pub code: String,
    pub entry_id: Option<String>,
    pub severity: String,
}
#[derive(Clone, Debug, Serialize, Default)]
pub struct PackDiff {
    pub added: Vec<PackEntry>,
    pub removed: Vec<PackEntry>,
    pub changed: Vec<EntryDiff>,
    pub metadata_changed: bool,
    pub custom_overrides: Vec<String>,
    pub symbolic_links: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct EntryDiff {
    pub before: PackEntry,
    pub after: PackEntry,
    pub version_changed: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct PackPreview {
    pub draft_revision: String,
    pub published_revision: String,
    pub manifest: PackManifest,
    pub issues: Vec<PackIssue>,
    pub diff: PackDiff,
    pub suggestions: Vec<PackSuggestion>,
    pub review_sha256: String,
    pub valid: bool,
    pub safety_snapshot_recommended: bool,
}
fn issue(issues: &mut Vec<PackIssue>, code: &str, entry: Option<&str>, warning: bool) {
    issues.push(PackIssue {
        code: code.into(),
        entry_id: entry.map(str::to_string),
        severity: if warning { "warning" } else { "error" }.into(),
    });
}
impl PackWorkspace {
    #[allow(clippy::too_many_arguments)]
    pub fn preview(
        &self,
        subject: &Subject,
        mc: &str,
        input: &PreviewRequest,
        releases: &PackReleases,
        providers: &dyn WorkspaceProvider,
        suggestions: Vec<PackSuggestion>,
    ) -> Result<PackPreview> {
        let draft = self.read(subject, mc)?;
        if draft.revision != input.draft_revision {
            return Err(ReleaseError::Conflict);
        }
        let mut manifest = draft.manifest;
        if input.resolved_entries.len() > MAX_ENTRIES {
            return Err(invalid());
        }
        // Resolver additions may not replace a reviewed root or select a second version.
        for dependency in &input.resolved_entries {
            if let Some(original) = manifest.entries.iter_mut().find(|e| e.id == dependency.id) {
                let mut promoted = original.clone();
                promoted.required = true;
                promoted.environment.client = Side::Required;
                if !original.required && promoted == *dependency {
                    *original = promoted;
                    continue;
                }
                return Err(ReleaseError::Conflict);
            }
            if manifest
                .entries
                .iter()
                .any(|e| e.id == dependency.id || e.path.eq_ignore_ascii_case(&dependency.path))
            {
                return Err(ReleaseError::Conflict);
            }
            manifest.entries.push(dependency.clone());
        }
        validate_manifest(&manifest)?;
        self.client_manifest(&manifest)?;
        let release = releases.current()?;
        let published_revision = release.as_ref().map_or(0, |r| r.revision());
        let published = self
            .published(subject)?
            .filter(|(r, _)| *r == published_revision);
        let mut issues = Vec::new();
        if let Some(release) = &release
            && !super::super::releases::version_not_lower(
                &manifest.display_version,
                release.display_version(),
            )
        {
            issue(&mut issues, "display_version_lower", None, true);
        }
        if manifest.minecraft_version != mc {
            issue(&mut issues, "minecraft_version_drift", None, false);
        }
        let published = if published.is_none() {
            if let Some(release) = &release {
                let loaded = releases
                    .artifacts()
                    .load(release.artifact(), super::super::MAX_ARTIFACT_BYTES)?;
                match super::import::parse_archive(loaded.bytes(), providers, |_| Ok(None)) {
                    Ok((manifest, _)) => Some((release.revision(), manifest)),
                    Err(_) => {
                        issue(&mut issues, "published_manifest_unavailable", None, false);
                        None
                    }
                }
            } else {
                None
            }
        } else {
            published
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        for entry in &manifest.entries {
            if entry.environment.client == Side::Unsupported {
                continue;
            }
            match &entry.source {
                EntrySource::Manual { .. } => issue(
                    &mut issues,
                    "manual_download_required",
                    Some(&entry.id),
                    false,
                ),
                EntrySource::Upload { sha256 } => {
                    let exists = self
                        .db
                        .read_with(|s| self.blob_in(s.connection(), sha256))?;
                    if exists.len() > super::super::MAX_PINNED_FILE_BYTES {
                        return Err(invalid());
                    }
                    if entry.kind != EntryKind::ConfigOverride {
                        issue(
                            &mut issues,
                            "uploaded_compatibility_unverified",
                            Some(&entry.id),
                            true,
                        );
                    }
                }
                EntrySource::Provider {
                    provider,
                    project_id,
                    version_id,
                } => {
                    if Instant::now() > deadline {
                        issue(
                            &mut issues,
                            "provider_budget_exhausted",
                            Some(&entry.id),
                            false,
                        );
                        break;
                    }
                    match providers.pack_metadata(provider, project_id, version_id) {
                        Err(_) => {
                            issue(&mut issues, "provider_unavailable", Some(&entry.id), false)
                        }
                        Ok(mut meta) => {
                            if entry.kind == EntryKind::Shader {
                                let engine = match (provider.as_str(), &manifest.loader) {
                                    ("modrinth", ClientLoader::Forge) => Some("oculus"),
                                    ("curseforge", ClientLoader::Forge) => Some("581495"),
                                    (
                                        "modrinth",
                                        ClientLoader::Fabric
                                        | ClientLoader::Quilt
                                        | ClientLoader::Neoforge,
                                    ) => Some("iris"),
                                    (
                                        "curseforge",
                                        ClientLoader::Fabric
                                        | ClientLoader::Quilt
                                        | ClientLoader::Neoforge,
                                    ) => Some("445996"),
                                    _ => None,
                                };
                                if let Some(project) = engine {
                                    meta.dependencies.push(ProviderDependency {
                                        project_id: project.into(),
                                        version_id: None,
                                        incompatible: false,
                                    });
                                } else {
                                    issue(
                                        &mut issues,
                                        "shader_engine_manual",
                                        Some(&entry.id),
                                        true,
                                    );
                                }
                            }
                            if !super::providers::compatible(&meta, &manifest)
                                || meta.kind != entry.kind
                            {
                                issue(&mut issues, "entry_incompatible", Some(&entry.id), false);
                            }
                            if !meta.downloadable {
                                issue(
                                    &mut issues,
                                    "manual_download_required",
                                    Some(&entry.id),
                                    false,
                                );
                            }
                            if entry.path.rsplit('/').next() != Some(meta.filename.as_str()) {
                                issue(
                                    &mut issues,
                                    "provider_filename_mismatch",
                                    Some(&entry.id),
                                    false,
                                );
                            }
                            for dep in meta.dependencies {
                                let found=manifest.entries.iter().find(|other| other.environment.client!=Side::Unsupported&&matches!(&other.source,EntrySource::Provider{provider:p,project_id:pr,version_id:v} if p==provider&&(dep.project_id.is_empty()||pr==&dep.project_id)&&dep.version_id.as_ref().is_none_or(|id|id==v)));
                                if dep.incompatible && found.is_some() {
                                    issue(
                                        &mut issues,
                                        "dependency_incompatible",
                                        Some(&entry.id),
                                        false,
                                    );
                                }
                                if !dep.incompatible
                                    && found.is_none_or(|e| entry.required && !e.required)
                                {
                                    issue(
                                        &mut issues,
                                        "required_dependency_missing",
                                        Some(&entry.id),
                                        false,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        let diff = diff(published.as_ref().map(|(_, m)| m), &manifest);
        let mut preview = PackPreview {
            draft_revision: input.draft_revision.clone(),
            published_revision: published_revision.to_string(),
            manifest,
            valid: !issues.iter().any(|i| i.severity == "error"),
            issues,
            diff,
            suggestions,
            review_sha256: String::new(),
            safety_snapshot_recommended: true,
        };
        preview.review_sha256 = digest(&serde_json::to_vec(&preview).map_err(|_| invalid())?);
        Ok(preview)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        &self,
        subject: &Subject,
        mc: &str,
        key: &str,
        input: &WorkspacePublish,
        releases: &PackReleases,
        providers: &dyn WorkspaceProvider,
        http: &dyn DownloadHttp,
        base: &PackMetadata,
        suggestions: Vec<PackSuggestion>,
    ) -> Result<PackOutcome> {
        // Exact outcome replay precedes draft/metadata reads, so an intervening edit
        // does not turn a previously committed publication into an unknown outcome.
        let expected = revision(&input.expected_revision)?;
        let input_sha = digest(&serde_json::to_vec(input).map_err(|_| invalid())?);
        let request = PackMutation::workspace_publish(
            key.into(),
            expected,
            &input_sha,
            revision(&input.draft_revision)?,
            String::new(),
            input.acknowledge_lower_version,
            None,
            input.required_update,
        )?;
        if let Some(outcome) = releases.remote_outcome(subject, &request)? {
            return Ok(outcome);
        }
        let target = self
            .db
            .read_with(|s| crate::software::SoftwareHandle::pack_target_in(s.connection()))?;
        if target
            .as_ref()
            .and_then(|(_, v)| v.as_deref())
            .is_some_and(|v| v != mc)
        {
            return Err(ReleaseError::Conflict);
        }
        let plan = releases.plan(
            subject,
            expected,
            if input.required_update {
                UpdatePolicy::Required
            } else {
                UpdatePolicy::Optional
            },
        )?;
        let preview = self.preview(
            subject,
            mc,
            &PreviewRequest {
                draft_revision: input.draft_revision.clone(),
                resolved_entries: input.resolved_entries.clone(),
            },
            releases,
            providers,
            suggestions,
        )?;
        if preview.review_sha256 != input.review_sha256
            || preview.published_revision != input.expected_revision
        {
            return Err(ReleaseError::Conflict);
        }
        if !preview.valid {
            return Err(super::super::PackError::UnresolvedFile.into());
        }
        let request = PackMutation::workspace_publish(
            key.into(),
            expected,
            &input_sha,
            revision(&input.draft_revision)?,
            serde_json::to_string(&preview.manifest).map_err(|_| invalid())?,
            input.acknowledge_lower_version,
            target.map(|(r, _)| r),
            input.required_update,
        )?;
        let metadata = PackMetadata::workspace(
            base,
            &preview.manifest,
            plan.revision(),
            plan.minimum_revision(),
        );
        let client_manifest = self.client_manifest(&preview.manifest)?;
        let snapshot = self.snapshot(&client_manifest, &metadata, providers, http)?;
        let icon_path = snapshot
            .bodies
            .keys()
            .find(|p| matches!(p.as_str(), "pack-icon.png" | "pack-icon.jpg"))
            .map(|p| p.as_str());
        let mut env: BTreeMap<String, (String, String)> = client_manifest
            .entries
            .iter()
            .map(|e| {
                (
                    e.path.clone(),
                    (
                        if e.required { "required" } else { "optional" }.into(),
                        match e.environment.server {
                            Side::Required => "required",
                            Side::Optional => "optional",
                            Side::Unsupported => "unsupported",
                        }
                        .into(),
                    ),
                )
            })
            .collect();
        if let Some(path) = icon_path {
            env.insert(path.into(), ("required".into(), "unsupported".into()));
        }
        let built = super::super::build::build_vesta_with_environments(
            &snapshot,
            &metadata,
            super::super::VestaBuildLimits::default(),
            &env,
            Some((&preview.manifest.summary, icon_path)),
        )
        .map_err(|e| match e {
            super::super::VestaBuildError::TooLarge => super::super::PackError::TooLarge,
            super::super::VestaBuildError::InvalidReference => {
                super::super::PackError::UnresolvedFile
            }
            _ => super::super::PackError::Unavailable,
        })?;
        let prepared = PreparedPack {
            manifest: snapshot.manifest,
            content: super::super::configured::PreparedContent::Scanned(built),
            missing_directories: Vec::new(),
        };
        releases.publish_remote(subject, &plan, &prepared, &request)
    }
    fn snapshot(
        &self,
        m: &PackManifest,
        metadata: &PackMetadata,
        providers: &dyn WorkspaceProvider,
        http: &dyn DownloadHttp,
    ) -> Result<PreparedSnapshot> {
        let mut files = Vec::new();
        let mut bodies = BTreeMap::new();
        let mut total = 0usize;
        let deadline = Instant::now() + Duration::from_secs(120);
        for entry in &m.entries {
            if entry.environment.client == Side::Unsupported {
                continue;
            }
            let (bytes, source) = match &entry.source {
                EntrySource::Upload { sha256 } => (
                    self.db
                        .read_with(|s| self.blob_in(s.connection(), sha256))?,
                    FileSource::embedded(),
                ),
                EntrySource::Provider {
                    provider,
                    project_id,
                    version_id,
                } => {
                    let platform = if provider == "modrinth" {
                        Provider::Modrinth
                    } else {
                        Provider::Curseforge
                    };
                    let source = providers
                        .resolve_with_deadline(platform, project_id, version_id, deadline)
                        .map_err(|_| super::super::PackError::UnresolvedFile)?;
                    let bytes = content::fetch_verified_source(&source, http, deadline)
                        .map_err(|_| super::super::PackError::UnresolvedFile)?;
                    if entry.path.rsplit('/').next() != Some(source.filename.as_str()) {
                        return Err(ReleaseError::Conflict);
                    }
                    let reference = FileSource::platform(
                        if platform == Provider::Modrinth {
                            Platform::Modrinth
                        } else {
                            Platform::Curseforge
                        },
                        project_id,
                        version_id,
                        if platform == Provider::Modrinth {
                            Some(&source.download_url)
                        } else {
                            None
                        },
                    )
                    .map_err(super::super::PackError::from)?;
                    (bytes, reference)
                }
                EntrySource::Manual { .. } => {
                    return Err(super::super::PackError::UnresolvedFile.into());
                }
            };
            total = total
                .checked_add(bytes.len())
                .filter(|n| *n <= MAX_BLOBS)
                .ok_or(super::super::PackError::TooLarge)?;
            files.push(
                PackFile::new(&entry.path, &digest(&bytes), bytes.len() as u64, source)
                    .map_err(super::super::PackError::from)?,
            );
            bodies.insert(
                RelativePath::parse(&entry.path).map_err(super::super::PackError::from)?,
                bytes.into_boxed_slice(),
            );
        }
        if let Some(sha) = &m.icon {
            let bytes = self.db.read_with(|s| self.blob_in(s.connection(), sha))?;
            let path = if icon_mime(&bytes)? == "image/png" {
                "pack-icon.png"
            } else {
                "pack-icon.jpg"
            };
            total = total
                .checked_add(bytes.len())
                .filter(|n| *n <= MAX_BLOBS)
                .ok_or(super::super::PackError::TooLarge)?;
            files.push(
                PackFile::new(path, sha, bytes.len() as u64, FileSource::embedded())
                    .map_err(super::super::PackError::from)?,
            );
            bodies.insert(
                RelativePath::parse(path).map_err(super::super::PackError::from)?,
                bytes.into_boxed_slice(),
            );
        }
        let metadata = ManifestMetadata::new(
            metadata.name(),
            metadata.version(),
            metadata.revision(),
            metadata.minimum_revision(),
        )
        .map_err(super::super::PackError::from)?;
        Ok(PreparedSnapshot {
            manifest: Manifest::new(metadata, PackContent::Custom(files))
                .map_err(super::super::PackError::from)?,
            bodies,
            total,
            private_files: Vec::new(),
            missing_directories: Vec::new(),
        })
    }
}
fn diff(before: Option<&PackManifest>, after: &PackManifest) -> PackDiff {
    let mut result = PackDiff::default();
    let old = before.map(|m| m.entries.as_slice()).unwrap_or_default();
    for e in &after.entries {
        match old.iter().find(|b|b.path==e.path) {
            None=>result.added.push(e.clone()),Some(b) if b!=e=>result.changed.push(EntryDiff{before:b.clone(),after:e.clone(),version_changed:matches!((&b.source,&e.source),(EntrySource::Provider{version_id:a,..},EntrySource::Provider{version_id:b,..}) if a!=b)}),_=>{}
        }
        if matches!(e.source, EntrySource::Upload { .. }) {
            result.custom_overrides.push(e.path.clone());
        }
    }
    for e in old {
        if !after.entries.iter().any(|a| a.path == e.path) {
            result.removed.push(e.clone());
        }
    }
    result.metadata_changed = before.is_none_or(|b| {
        b.name != after.name
            || b.summary != after.summary
            || b.icon != after.icon
            || b.minecraft_version != after.minecraft_version
            || b.loader != after.loader
            || b.loader_version != after.loader_version
            || b.display_version != after.display_version
            || b.override_files != after.override_files
    });
    result.custom_overrides.extend(
        after
            .override_files
            .iter()
            .filter(|f| !f.trashed && f.sha256.is_some())
            .map(|f| format!("{}/{}", f.root, f.path)),
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{ContentError, HttpResponse, ProviderClient, ResolvedSource};
    struct Providers;
    impl ProviderClient for Providers {
        fn resolve(&self, _: Provider, _: &str, _: &str) -> content::Result<ResolvedSource> {
            Err(ContentError::DownloadFailed)
        }
    }
    impl WorkspaceProvider for Providers {
        fn pack_metadata(&self, _: &str, project: &str, _: &str) -> Result<ProviderMetadata> {
            Ok(ProviderMetadata {
                client_required: false,
                server_requirement: Side::Unsupported,
                minecraft_versions: vec!["1.21.1".into()],
                loaders: vec!["fabric".into()],
                kind: EntryKind::Mod,
                client_supported: true,
                downloadable: project != "manual",
                filename: format!("{project}.jar"),
                dependencies: if project == "root" {
                    vec![ProviderDependency {
                        project_id: "dep".into(),
                        version_id: Some("v1".into()),
                        incompatible: false,
                    }]
                } else {
                    vec![]
                },
                published: "2026-10-26".into(),
            })
        }
        fn newer(&self, _: &str, _: &str, _: &str, _: &PackManifest) -> Result<Option<String>> {
            Ok(None)
        }
    }
    struct Http;
    impl DownloadHttp for Http {
        fn get(&self, _: &url::Url) -> content::Result<HttpResponse> {
            panic!("uploaded files must not use network")
        }
    }
    struct DownloadProviders;
    impl ProviderClient for DownloadProviders {
        fn resolve(
            &self,
            provider: Provider,
            project: &str,
            version: &str,
        ) -> content::Result<ResolvedSource> {
            Ok(ResolvedSource {
                provider,
                filename: format!("{project}.jar"),
                download_url: format!(
                    "https://cdn.modrinth.com/data/{project}/versions/{version}/{project}.jar"
                ),
                expected_hash: content::ExpectedHash::Sha512(hex::encode(sha2::Sha512::digest(
                    b"provider bytes",
                ))),
            })
        }
    }
    impl WorkspaceProvider for DownloadProviders {
        fn pack_metadata(&self, p: &str, project: &str, v: &str) -> Result<ProviderMetadata> {
            Providers.pack_metadata(p, project, v)
        }
        fn newer(&self, _: &str, _: &str, _: &str, _: &PackManifest) -> Result<Option<String>> {
            Ok(None)
        }
    }
    struct ProviderHttp<'a> {
        edit: Option<&'a super::super::tests::Fixture>,
        corrupt: bool,
    }
    impl DownloadHttp for ProviderHttp<'_> {
        fn get(&self, _: &url::Url) -> content::Result<HttpResponse> {
            if let Some(f) = self.edit {
                f.workspace
                    .delete(&f.subject, "1.21.1", "concurrent-edit", "1")
                    .unwrap();
            }
            let bytes = if self.corrupt {
                b"corrupted body".as_slice()
            } else {
                b"provider bytes".as_slice()
            };
            Ok(HttpResponse {
                status: 200,
                location: None,
                content_encoding: None,
                content_length: Some(bytes.len() as u64),
                body: Box::new(std::io::Cursor::new(bytes.to_vec())),
            })
        }
    }
    fn base(f: &super::super::tests::Fixture) -> PackMetadata {
        crate::config::HostConfiguration::parse_toml(
            br#"schema-version = 1
[paths]
state = 'private-state'
game = 'game'
scan-roots = ['game/config']
launch-definition = 'private/launch.toml'
[pack]
name = 'Published'
version = '1.0.0'
revision = 1
minimum-revision = 1
minecraft-version = '1.21.1'
[pack-access]
mode = 'minecraft'
[listener]
mode = 'disabled'
"#,
            f.temp.path(),
            std::path::Path::new("host.toml"),
        )
        .unwrap()
        .pack()
        .clone()
    }
    fn provider_entry(project: &str) -> PackEntry {
        PackEntry {
            id: project.into(),
            kind: EntryKind::Mod,
            path: format!("mods/{project}.jar"),
            required: true,
            environment: Environment {
                client: Side::Required,
                server: Side::Unsupported,
            },
            source: EntrySource::Provider {
                provider: "modrinth".into(),
                project_id: project.into(),
                version_id: "v1".into(),
            },
        }
    }
    #[test]
    fn publication_embeds_client_override_precedence_and_never_server_only_bytes() {
        use std::io::Read;
        let f = super::super::tests::Fixture::new();
        let releases = PackReleases::open(&f.db).unwrap();
        for (i, root) in ["overrides", "client-overrides", "server-overrides"]
            .iter()
            .enumerate()
        {
            f.workspace
                .file_mutate(
                    &f.subject,
                    "1.21.1",
                    &format!("file-{i}"),
                    &FileMutation {
                        expected_revision: i.to_string(),
                        change: FileOperation::Write {
                            root: (*root).into(),
                            path: if i == 2 {
                                "config/server-secret.txt"
                            } else {
                                "config/a.txt"
                            }
                            .into(),
                            content_base64: STANDARD.encode(root),
                        },
                    },
                )
                .unwrap();
        }
        let preview = f
            .workspace
            .preview(
                &f.subject,
                "1.21.1",
                &PreviewRequest {
                    draft_revision: "3".into(),
                    resolved_entries: vec![],
                },
                &releases,
                &Providers,
                vec![],
            )
            .unwrap();
        assert!(preview.valid);
        assert!(
            preview
                .diff
                .custom_overrides
                .contains(&"server-overrides/config/server-secret.txt".into())
        );
        f.workspace
            .publish(
                &f.subject,
                "1.21.1",
                "publish-overrides",
                &WorkspacePublish {
                    expected_revision: "0".into(),
                    draft_revision: "3".into(),
                    resolved_entries: vec![],
                    review_sha256: preview.review_sha256,
                    required_update: false,
                    acknowledge_lower_version: false,
                },
                &releases,
                &Providers,
                &ProviderHttp {
                    edit: None,
                    corrupt: false,
                },
                &base(&f),
                vec![],
            )
            .unwrap();
        let release = releases.current().unwrap().unwrap();
        let artifact = releases
            .artifacts()
            .load(release.artifact(), crate::packs::MAX_ARTIFACT_BYTES)
            .unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(artifact.bytes())).unwrap();
        assert!(!zip.file_names().any(|name| name.contains("server-secret")));
        let mut body = String::new();
        zip.by_name("overrides/config/a.txt")
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();
        assert_eq!(body, "client-overrides");
    }
    #[test]
    fn validation_requires_compatible_complete_dependencies_without_editing_draft() {
        let f = super::super::tests::Fixture::new();
        let releases = PackReleases::open(&f.db).unwrap();
        let mut m = f.manifest();
        m.loader = ClientLoader::Fabric;
        m.loader_version = Some("0.16.10".into());
        m.entries.push(provider_entry("root"));
        f.workspace
            .write(
                &f.subject,
                "1.21.1",
                "draft",
                &DraftWrite {
                    expected_revision: "0".into(),
                    manifest: m,
                },
            )
            .unwrap();
        let preview = f
            .workspace
            .preview(
                &f.subject,
                "1.21.1",
                &PreviewRequest {
                    draft_revision: "1".into(),
                    resolved_entries: vec![],
                },
                &releases,
                &Providers,
                vec![],
            )
            .unwrap();
        assert!(!preview.valid);
        assert!(
            preview
                .issues
                .iter()
                .any(|i| i.code == "required_dependency_missing")
        );
        let preview = f
            .workspace
            .preview(
                &f.subject,
                "1.21.1",
                &PreviewRequest {
                    draft_revision: "1".into(),
                    resolved_entries: vec![provider_entry("dep")],
                },
                &releases,
                &Providers,
                vec![],
            )
            .unwrap();
        assert!(preview.valid);
        assert_eq!(preview.diff.added.len(), 2);
        assert_eq!(
            f.workspace
                .read(&f.subject, "1.21.1")
                .unwrap()
                .manifest
                .entries
                .len(),
            1
        );
    }
    #[test]
    fn publish_is_atomic_monotonic_binds_review_and_preserves_optional_environment() {
        let f = super::super::tests::Fixture::new();
        let releases = PackReleases::open(&f.db).unwrap();
        let base = base(&f);
        let sha = f
            .workspace
            .upload(
                &f.subject,
                "upload",
                &UploadRequest {
                    content_base64: STANDARD.encode(b"client"),
                },
            )
            .unwrap();
        let mut m = f.manifest();
        m.summary = "Published presentation".into();
        let icon = b"\x89PNG\r\n\x1a\nowned-image-fixture";
        m.icon = Some(
            f.workspace
                .upload(
                    &f.subject,
                    "icon",
                    &UploadRequest {
                        content_base64: STANDARD.encode(icon),
                    },
                )
                .unwrap(),
        );
        m.entries.push(PackEntry {
            id: "config".into(),
            kind: EntryKind::ConfigOverride,
            path: "config/client.toml".into(),
            required: false,
            environment: Environment {
                client: Side::Optional,
                server: Side::Unsupported,
            },
            source: EntrySource::Upload { sha256: sha },
        });
        f.workspace
            .write(
                &f.subject,
                "1.21.1",
                "save",
                &DraftWrite {
                    expected_revision: "0".into(),
                    manifest: m,
                },
            )
            .unwrap();
        let publish = |expected: &str, required: bool, key: &str| {
            let preview = f
                .workspace
                .preview(
                    &f.subject,
                    "1.21.1",
                    &PreviewRequest {
                        draft_revision: "1".into(),
                        resolved_entries: vec![],
                    },
                    &releases,
                    &Providers,
                    vec![],
                )
                .unwrap();
            let input = WorkspacePublish {
                expected_revision: expected.into(),
                draft_revision: "1".into(),
                resolved_entries: vec![],
                review_sha256: preview.review_sha256,
                required_update: required,
                acknowledge_lower_version: false,
            };
            let outcome = f
                .workspace
                .publish(
                    &f.subject,
                    "1.21.1",
                    key,
                    &input,
                    &releases,
                    &Providers,
                    &Http,
                    &base,
                    vec![],
                )
                .unwrap();
            (input, outcome)
        };
        let (first, one) = publish("0", false, "p1");
        assert_eq!((one.hosted_revision, one.minimum_revision), (1, 1));
        let (_, two) = publish("1", false, "p2");
        assert_eq!((two.hosted_revision, two.minimum_revision), (2, 1));
        let (required_request, three) = publish("2", true, "p3");
        assert_eq!((three.hosted_revision, three.minimum_revision), (3, 3));
        assert!(
            !f.workspace
                .view(&f.subject, "1.21.1", &releases)
                .unwrap()
                .pending_publication
        );
        let loaded = releases
            .artifacts()
            .load(
                releases.current().unwrap().unwrap().artifact(),
                super::super::super::MAX_ARTIFACT_BYTES,
            )
            .unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(loaded.bytes())).unwrap();
        let index: serde_json::Value =
            serde_json::from_reader(zip.by_name(vesta_protocol::pack::INDEX_NAME).unwrap())
                .unwrap();
        assert_eq!(index["files"][0]["env"]["client"], "optional");
        assert_eq!(index["summary"], "Published presentation");
        assert_eq!(index["icon"], "pack-icon.png");
        assert!(zip.by_name("overrides/pack-icon.png").is_ok());
        assert_eq!(index["files"][0]["env"]["server"], "unsupported");
        f.workspace
            .delete(&f.subject, "1.21.1", "later-edit", "1")
            .unwrap();
        assert_eq!(
            f.workspace
                .publish(
                    &f.subject,
                    "1.21.1",
                    "p1",
                    &first,
                    &releases,
                    &Providers,
                    &Http,
                    &base,
                    vec![]
                )
                .unwrap(),
            one
        );
        let mut changed = first;
        changed.required_update = true;
        assert_eq!(
            f.workspace
                .publish(
                    &f.subject,
                    "1.21.1",
                    "p1",
                    &changed,
                    &releases,
                    &Providers,
                    &Http,
                    &base,
                    vec![]
                )
                .unwrap_err()
                .code(),
            "idempotency_conflict"
        );
        f.db.write(|c|{c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES('deny-required-replay',?,'pack.force_update','*','deny','fixture',?)",params![f.subject.principal.as_str(),f.db.clock().millis()])?;Ok(())}).unwrap();
        assert_eq!(
            f.workspace
                .publish(
                    &f.subject,
                    "1.21.1",
                    "p3",
                    &required_request,
                    &releases,
                    &Providers,
                    &Http,
                    &base,
                    vec![]
                )
                .unwrap_err()
                .code(),
            "forbidden"
        );
    }
    #[test]
    fn chunked_upload_checks_offsets_hash_and_durable_replay() {
        let f = super::super::tests::Fixture::new();
        let bytes = b"chunked config";
        let input = UploadBegin {
            upload_id: "upload-1".into(),
            total_size: bytes.len().to_string(),
            sha256: digest(bytes),
            archive: false,
        };
        f.workspace
            .upload_begin(&f.subject, "begin", &input)
            .unwrap();
        let chunk = UploadChunk {
            upload_id: input.upload_id.clone(),
            offset: "0".into(),
            content_base64: STANDARD.encode(bytes),
        };
        let first = f
            .workspace
            .upload_chunk(&f.subject, "chunk", &chunk)
            .unwrap();
        assert_eq!(
            f.workspace
                .upload_chunk(&f.subject, "chunk", &chunk)
                .unwrap()
                .size,
            first.size
        );
        assert!(
            f.workspace
                .upload_chunk(&f.subject, "wrong-offset", &chunk)
                .is_err()
        );
        let done = f
            .workspace
            .upload_finish(&f.subject, "finish", &input.upload_id)
            .unwrap();
        assert!(done.complete);
        assert_eq!(done.sha256, input.sha256);
        assert!(
            f.workspace
                .upload_finish(&f.subject, "finish", &input.upload_id)
                .unwrap()
                .complete
        );
    }
    #[test]
    fn provider_build_verifies_bytes_and_never_activates_over_a_concurrent_draft_edit() {
        for race in [false, true] {
            let f = super::super::tests::Fixture::new();
            let releases = PackReleases::open(&f.db).unwrap();
            let base = base(&f);
            let mut m = f.manifest();
            m.loader = ClientLoader::Fabric;
            m.loader_version = Some("0.16.10".into());
            let mut leaf = provider_entry("LeafProj");
            if let EntrySource::Provider { version_id, .. } = &mut leaf.source {
                *version_id = "Version1".into();
            }
            m.entries.push(leaf);
            f.workspace
                .write(
                    &f.subject,
                    "1.21.1",
                    "save",
                    &DraftWrite {
                        expected_revision: "0".into(),
                        manifest: m,
                    },
                )
                .unwrap();
            let p = f
                .workspace
                .preview(
                    &f.subject,
                    "1.21.1",
                    &PreviewRequest {
                        draft_revision: "1".into(),
                        resolved_entries: vec![],
                    },
                    &releases,
                    &DownloadProviders,
                    vec![],
                )
                .unwrap();
            assert!(p.valid);
            let request = WorkspacePublish {
                expected_revision: "0".into(),
                draft_revision: "1".into(),
                resolved_entries: vec![],
                review_sha256: p.review_sha256,
                required_update: false,
                acknowledge_lower_version: false,
            };
            assert!(
                f.workspace
                    .publish(
                        &f.subject,
                        "1.21.1",
                        "publish",
                        &request,
                        &releases,
                        &DownloadProviders,
                        &ProviderHttp {
                            edit: None,
                            corrupt: true
                        },
                        &base,
                        vec![]
                    )
                    .is_err()
            );
            assert!(releases.current().unwrap().is_none());
            let result = f.workspace.publish(
                &f.subject,
                "1.21.1",
                "publish",
                &request,
                &releases,
                &DownloadProviders,
                &ProviderHttp {
                    edit: race.then_some(&f),
                    corrupt: false,
                },
                &base,
                vec![],
            );
            if race {
                assert_eq!(result.unwrap_err().code(), "pack_release_conflict");
                assert!(releases.current().unwrap().is_none());
                assert_eq!(
                    f.workspace.read(&f.subject, "1.21.1").unwrap().revision,
                    "2"
                );
            } else {
                assert_eq!(result.unwrap().hosted_revision, 1);
                let release = releases.current().unwrap().unwrap();
                let bytes = releases
                    .artifacts()
                    .load(release.artifact(), super::super::super::MAX_ARTIFACT_BYTES)
                    .unwrap();
                let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes.bytes())).unwrap();
                let index: serde_json::Value =
                    serde_json::from_reader(z.by_name(vesta_protocol::pack::INDEX_NAME).unwrap())
                        .unwrap();
                assert_eq!(index["files"][0]["source"]["project_id"], "LeafProj");
                assert!(z.by_name("overrides/mods/LeafProj.jar").is_err());
            }
        }
    }
}
