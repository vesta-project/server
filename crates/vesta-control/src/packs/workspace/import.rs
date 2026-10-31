//! Bounded archive ingestion. Imported download URLs never become fetch authority.
use super::*;
use crate::content::{Provider, ProviderClient, UreqProviderClient};
use crate::packs::payload::{Payload, Store};
use std::io::{Cursor, Read, Seek};

enum ArchiveBody {
    Memory(Vec<u8>),
    Disk(std::sync::Arc<Store>, Payload),
}
impl ArchiveBody {
    fn size(&self) -> u64 {
        match self {
            Self::Memory(b) => b.len() as u64,
            Self::Disk(_, p) => p.size,
        }
    }
    fn hash(&self) -> String {
        match self {
            Self::Memory(b) => digest(b),
            Self::Disk(_, p) => p.sha256.clone(),
        }
    }
    fn bytes(&self) -> Result<Vec<u8>> {
        match self {
            Self::Memory(b) => Ok(b.clone()),
            Self::Disk(store, p) => {
                let mut file = store.verify(p).map_err(|_| invalid())?;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).map_err(|_| invalid())?;
                Ok(bytes)
            }
        }
    }
    fn payload(self, store: &Store) -> Result<Payload> {
        match self {
            Self::Disk(_, p) => Ok(p),
            Self::Memory(b) => store
                .put(&mut Cursor::new(b))
                .map_err(|e| super::super::PackError::from(e).into()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImportRequest {
    Provider { expected_revision: String, provider: String, project_id: String, version_id: String },
    Unlink { expected_revision: String, choice: String },
    UploadedArchive {
        expected_revision: String,
        upload_id: String,
    },
    Archive {
        expected_revision: String,
        content_base64: String,
    },
    Published {
        expected_revision: String,
        expected_published_revision: String,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InspectRequest {
    pub upload_id: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct ArchiveInspection {
    pub format: String,
    pub manifest: PackManifest,
    pub entry_count: usize,
    pub provider_entries: Vec<crate::content::ContentProvider>,
    pub unresolved_files: Vec<String>,
    pub overrides: Vec<String>,
    pub provider_pack: Option<crate::content::ContentProvider>,
    pub warnings: Vec<String>,
}
impl PackWorkspace {
    pub fn inspect_archive(
        &self,
        subject: &Subject,
        mc: &str,
        input: &InspectRequest,
        providers: &UreqProviderClient,
    ) -> Result<ArchiveInspection> {
        let store = Store::open(&self.db).map_err(super::super::PackError::from)?;
        let _cleanup = store.cleanup();
        let _lease = store.lease().map_err(super::super::PackError::from)?;
        let payload = self.archive_payload(subject, &input.upload_id, &store)?;
        let bounded_providers = providers
            .with_deadline(std::time::Instant::now() + std::time::Duration::from_secs(120))
            .map_err(ReleaseError::from)?;
        let providers = &bounded_providers;
        let zip = zip::ZipArchive::new(
            store
                .verify(&payload)
                .map_err(super::super::PackError::from)?,
        )
        .map_err(|_| invalid())?;
        let names: Vec<_> = zip.file_names().map(str::to_owned).collect();
        let format = if names.iter().any(|n| n == "modrinth.index.json") {
            "mrpack"
        } else if names.iter().any(|n| n == "manifest.json") {
            "curseforge"
        } else if names.iter().any(|n| n == vesta_protocol::pack::INDEX_NAME) {
            "vesta"
        } else {
            "zip"
        };
        let (mut manifest, _) = parse_spooled(&payload, &store, providers, |hash| {
            Ok(providers.identify_sha512(hash).ok().flatten())
        })?;
        if manifest.minecraft_version.is_empty() {
            manifest.minecraft_version = mc.into();
        }
        let override_paths = manifest
            .override_files
            .iter()
            .filter(|f| !f.trashed)
            .map(|f| format!("{}/{}", f.root, f.path))
            .collect();
        let provider_pack = if format == "mrpack" {
            use sha2::Sha512;
            let mut hash = Sha512::new();
            let mut file = store
                .verify(&payload)
                .map_err(super::super::PackError::from)?;
            let mut buffer = [0u8; 65536];
            loop {
                let count = file.read(&mut buffer).map_err(|_| invalid())?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
            providers
                .identify_sha512(&hex::encode(hash.finalize()))
                .ok()
                .flatten()
        } else {
            None
        };
        let mut result = ArchiveInspection {
            format: format.into(),
            entry_count: manifest.entries.len(),
            provider_entries: Vec::new(),
            unresolved_files: Vec::new(),
            overrides: override_paths,
            provider_pack,
            warnings: Vec::new(),
            manifest: manifest.clone(),
        };
        if result.provider_pack.is_none() {
            result
                .warnings
                .push("archive_does_not_establish_pack_provider_identity".into());
        }
        if format == "zip" {
            result
                .warnings
                .push("archive_runtime_inferred_from_server".into());
        }
        for entry in manifest.entries {
            match entry.source {
                EntrySource::Provider {
                    provider,
                    project_id,
                    version_id,
                } => result
                    .provider_entries
                    .push(crate::content::ContentProvider {
                        provider,
                        project_id,
                        version_id,
                    }),
                EntrySource::Manual { .. } => result.unresolved_files.push(entry.path.clone()),
                EntrySource::Upload { .. } if entry.kind != EntryKind::ConfigOverride => {
                    result.unresolved_files.push(entry.path.clone())
                }
                _ => {}
            }
            if entry.kind == EntryKind::ConfigOverride {
                result.overrides.push(entry.path);
            }
        }
        Ok(result)
    }

    pub fn import(
        &self,
        subject: &Subject,
        mc: &str,
        key: &str,
        input: &ImportRequest,
        releases: &super::super::PackReleases,
        providers: &UreqProviderClient,
    ) -> Result<PackDraft> {
        if let Some(outcome) = self.replay(subject, key, input)? {
            return Ok(outcome);
        }
        if let ImportRequest::Unlink { expected_revision, choice } = input {
            return self.mutate(subject, key, input, |c| {
                let mut manifest = self.read_in(c, mc)?.manifest;
                client_link::unlink(&mut manifest, choice)?;
                self.write_link_in(c, subject, mc, expected_revision, manifest)
            });
        }
        let expected = match input {
            ImportRequest::Provider { expected_revision, .. } | ImportRequest::Unlink { expected_revision, .. } => expected_revision,
            ImportRequest::UploadedArchive {
                expected_revision, ..
            }
            | ImportRequest::Archive {
                expected_revision, ..
            }
            | ImportRequest::Published {
                expected_revision, ..
            } => expected_revision,
        };
        if self.read(subject, mc)?.revision != *expected {
            return Err(ReleaseError::Conflict);
        }
        let mut release_revision = None;
        let mut import_warnings = Vec::new();
        let (mut manifest, blobs) = match input {
            ImportRequest::Unlink { .. } => unreachable!(),
            ImportRequest::Provider { provider, project_id, version_id, .. } => {
                let platform = match provider.as_str() { "modrinth" => Provider::Modrinth, "curseforge" => Provider::Curseforge, _ => return Err(invalid()) };
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
                let source = providers.resolve_with_deadline(platform, project_id, version_id, deadline).map_err(ReleaseError::from)?;
                let archive = crate::content::fetch_verified_source(&source, &crate::content::UreqDownloadHttp::default(), deadline).map_err(ReleaseError::from)?;
                let (base, blobs) = parse_archive(&archive, providers, |hash| Ok(providers.identify_sha512(hash).ok().flatten()))?;
                let mut custom = self.read(subject, mc)?.manifest;
                client_link::link(&mut custom, base, crate::content::ContentProvider { provider: provider.clone(), project_id: project_id.clone(), version_id: version_id.clone() }, mc)?;
                (custom, blobs)
            },
            ImportRequest::UploadedArchive { upload_id, .. } => {
                let bytes = self.archive_upload(subject, upload_id)?;
                import_warnings = archive_review_warnings(&bytes)?;
                parse_archive(&bytes, providers, |hash| Ok(providers.identify_sha512(hash).ok().flatten()))?
            },
            ImportRequest::Archive { content_base64, .. } => {
                let bytes = STANDARD.decode(content_base64).map_err(|_| invalid())?;
                import_warnings = archive_review_warnings(&bytes)?;
                parse_archive(&bytes, providers, |hash| Ok(providers.identify_sha512(hash).ok().flatten()))?
            },
            ImportRequest::Published {
                expected_published_revision,
                ..
            } => {
                let current = releases.current()?.ok_or(ReleaseError::Conflict)?;
                if current.revision() != revision(expected_published_revision)? {
                    return Err(ReleaseError::Conflict);
                }
                release_revision = Some(current.revision());
                if let Some((r, manifest)) = self.published(subject)?
                    && r == current.revision()
                    && current.artifact().format() != super::super::ArtifactFormat::Catalogue
                {
                    (manifest, Vec::new())
                } else {
                    let loaded = releases
                        .artifacts()
                        .load(current.artifact(), super::super::MAX_ARTIFACT_BYTES)?;
                    parse_archive(loaded.bytes(), providers, |hash| {
                        Ok(providers.identify_sha512(hash).ok().flatten())
                    })?
                }
            }
        };
        if manifest.minecraft_version.is_empty() {
            manifest.minecraft_version = mc.into();
        }
        self.mutate(subject, key, input, |c| {
            if let Some(release_revision) = release_revision {
                let actual: i64 = c.query_row(
                    "SELECT coalesce(max(revision),0) FROM pack_releases WHERE server_id=?",
                    [self.db.server_id()],
                    |r| r.get(0),
                )?;
                if actual as u64 != release_revision {
                    return Err(ReleaseError::Conflict);
                }
            }
            for bytes in blobs {
                self.store_blob(c, &bytes)?;
            }
            if let ImportRequest::UploadedArchive{upload_id,..}=input {
                c.execute("DELETE FROM pack_workspace_uploads WHERE server_id=? AND principal_id=? AND device_id=? AND upload_id=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),upload_id])?;
            }
            let draft = self.write_link_in(c, subject, mc, expected, manifest)?;
            c.execute("INSERT OR REPLACE INTO pack_import_warnings VALUES(?,?,?)", params![self.db.server_id(), draft.revision, serde_json::to_string(&import_warnings).map_err(|_| invalid())?])?;
            if matches!(input, ImportRequest::Published { .. }) {
                c.execute("UPDATE pack_workspace SET customizing_provider=1 WHERE server_id=?", [self.db.server_id()])?;
            }
            Ok(draft)
        })
    }
}
fn string<'a>(v: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    v.get(key).and_then(|v| v.as_str()).ok_or_else(invalid)
}
pub(super) fn kind(path: &str) -> Result<EntryKind> {
    match path.split('/').next().unwrap_or_default() {
        "mods" => Ok(EntryKind::Mod),
        "resourcepacks" => Ok(EntryKind::ResourcePack),
        "shaderpacks" => Ok(EntryKind::Shader),
        "datapacks" => Ok(EntryKind::Datapack),
        "config" => Ok(EntryKind::ConfigOverride),
        _ => Ok(EntryKind::ConfigOverride),
    }
}
fn entry(
    id: String,
    path: String,
    source: EntrySource,
    required: bool,
    server: Side,
) -> Result<PackEntry> {
    Ok(PackEntry {
        id,
        kind: kind(&path)?,
        path,
        required,
        environment: Environment {
            client: if required {
                Side::Required
            } else {
                Side::Optional
            },
            server,
        },
        source,
    })
}
pub(super) fn parse_archive(
    bytes: &[u8],
    providers: &dyn ProviderClient,
    identify: impl Fn(&str) -> Result<Option<crate::content::ContentProvider>>,
) -> Result<(PackManifest, Vec<Vec<u8>>)> {
    // Archive structure failures are not invalid user-entered file paths.
    let invalid = || ReleaseError::Pack(super::super::PackError::InvalidArchive);
    if bytes.iter().copied().find(|b| !b.is_ascii_whitespace()) == Some(b'{') {
        let manifest = vesta_protocol::pack::catalogue::Manifest::decode(bytes)
            .map_err(super::super::PackError::from)?;
        let vesta_protocol::pack::catalogue::PackContent::Platform(pack) = manifest.content()
        else {
            return Err(invalid());
        };
        let reference = pack.source();
        let provider = match reference.platform_name() {
            Some(vesta_protocol::pack::catalogue::Platform::Modrinth) => Provider::Modrinth,
            Some(vesta_protocol::pack::catalogue::Platform::Curseforge) => Provider::Curseforge,
            _ => return Err(invalid()),
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let source = providers
            .resolve_with_deadline(
                provider,
                reference.project_id().ok_or_else(invalid)?,
                reference.version_id().ok_or_else(invalid)?,
                deadline,
            )
            .map_err(ReleaseError::from)?;
        let archive = crate::content::fetch_verified_source(
            &source,
            &crate::content::UreqDownloadHttp::default(),
            deadline,
        )
        .map_err(|_| super::super::PackError::UnresolvedFile)?;
        if archive.iter().copied().find(|b| !b.is_ascii_whitespace()) == Some(b'{') {
            return Err(invalid());
        }
        return parse_archive(&archive, providers, identify);
    }
    let (manifest, blobs) = parse_zip(Cursor::new(bytes), providers, identify, None)?;
    Ok((
        manifest,
        blobs
            .into_iter()
            .map(|b| b.bytes())
            .collect::<Result<_>>()?,
    ))
}

pub(super) fn parse_spooled(
    payload: &Payload,
    store: &Store,
    providers: &dyn ProviderClient,
    identify: impl Fn(&str) -> Result<Option<crate::content::ContentProvider>>,
) -> Result<(PackManifest, Vec<Payload>)> {
    let file = store
        .verify(payload)
        .map_err(super::super::PackError::from)?;
    let (manifest, blobs) = parse_zip(file, providers, identify, Some(store))?;
    Ok((
        manifest,
        blobs
            .into_iter()
            .map(|body| body.payload(store))
            .collect::<Result<_>>()?,
    ))
}

fn parse_zip(
    reader: impl Read + Seek,
    providers: &dyn ProviderClient,
    identify: impl Fn(&str) -> Result<Option<crate::content::ContentProvider>>,
    store: Option<&Store>,
) -> Result<(PackManifest, Vec<ArchiveBody>)> {
    let invalid = || ReleaseError::Pack(super::super::PackError::InvalidArchive);
    let mut zip = zip::ZipArchive::new(reader).map_err(|_| invalid())?;
    let store = store.map(|store| std::sync::Arc::new(store.clone()));
    let mut files = std::collections::BTreeMap::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|_| invalid())?;
        let name = f.name().to_string();
        vesta_protocol::pack::catalogue::RelativePath::parse(name.trim_end_matches('/'))
            .map_err(|_| ReleaseError::Refusal { code: "pack_path_invalid", rule: "safe_relative_path", item: name.clone() })?;
        if f.is_symlink() { return Err(ReleaseError::Refusal { code: "pack_path_invalid", rule: "no_symbolic_links", item: name }); }
        if f.encrypted() {
            return Err(invalid());
        }
        if f.is_dir() {
            continue;
        }
        total = total
            .checked_add(f.size())
            .ok_or(super::super::PackError::TooLarge)?;
        let body = if let Some(store) = &store {
            let declared = f.size();
            let payload = store.put(&mut f).map_err(super::super::PackError::from)?;
            if payload.size != declared {
                return Err(invalid());
            }
            ArchiveBody::Disk(store.clone(), payload)
        } else {
            let mut body = Vec::new();
            f.read_to_end(&mut body)
                .map_err(|_| invalid())?;
            ArchiveBody::Memory(body)
        };
        if files.insert(name, body).is_some() {
            return Err(invalid());
        }
    }
    if let Some(index) = files.remove(vesta_protocol::pack::INDEX_NAME) {
        let index: vesta_protocol::pack::model::VestaIndexV1 =
            serde_json::from_slice(&index.bytes()?).map_err(|_| invalid())?;
        if index.format_version != 1 {
            return Err(invalid());
        }
        let loader = match index.modloader_type.as_deref() {
            None | Some("none") => ClientLoader::None,
            Some("fabric") => ClientLoader::Fabric,
            Some("quilt") => ClientLoader::Quilt,
            Some("forge") => ClientLoader::Forge,
            Some("neoforge") => ClientLoader::Neoforge,
            _ => return Err(invalid()),
        };
        let icon_path = index.icon.clone();
        let mut manifest = PackManifest {
            modpack_link: None,
            name: index.name.unwrap_or_else(|| "Client pack".into()),
            summary: index.summary.unwrap_or_default(),
            icon: None,
            display_version: index.version.unwrap_or_else(|| "1.0".into()),
            minecraft_version: index.minecraft_version.ok_or_else(invalid)?,
            loader,
            loader_version: index.modloader_version,
            override_files: Vec::new(),
            entries: Vec::new(),
        };
        let mut blobs = Vec::new();
        for (i, file) in index.files.into_iter().enumerate() {
            let source = if file.source.r#type == "modrinth"
                || file.source.r#type == "curseforge"
                || file.source.platform.is_some()
            {
                EntrySource::Provider {
                    provider: file.source.platform.unwrap_or(file.source.r#type),
                    project_id: file.source.project_id.ok_or_else(invalid)?,
                    version_id: file.source.version_id.ok_or_else(invalid)?,
                }
            } else if let Some(bytes) = files.remove(&format!("overrides/{}", file.path)) {
                let sha = bytes.hash();
                if file.hashes.sha256.as_deref() != Some(&sha) || file.size as u64 != bytes.size() {
                    return Err(invalid());
                }
                blobs.push(bytes);
                EntrySource::Upload { sha256: sha }
            } else {
                EntrySource::Manual {
                    reason: "published_file_unavailable".into(),
                }
            };
            if icon_path.as_deref() == Some(&file.path) {
                if let EntrySource::Upload { sha256 } = source {
                    icon_mime(&blobs.last().ok_or_else(invalid)?.bytes()?)?;
                    manifest.icon = Some(sha256);
                    continue;
                } else {
                    return Err(invalid());
                }
            }
            let mut imported = entry(
                format!("published-{i}"),
                file.path,
                source,
                file.env.client != "optional",
                match file.env.server.as_str() {
                    "required" => Side::Required,
                    "optional" => Side::Optional,
                    "unsupported" => Side::Unsupported,
                    _ => Side::Optional,
                },
            )?;
            imported.environment.client = match file.env.client.as_str() {
                "unsupported" => Side::Unsupported,
                "optional" => Side::Optional,
                _ => Side::Required,
            };
            manifest.entries.push(imported);
        }
        validate_manifest(&manifest)?;
        return Ok((manifest, blobs));
    }
    let mr = files.remove("modrinth.index.json");
    let cf = files.remove("manifest.json");
    if mr.is_none() && cf.is_none() {
        let mut manifest = PackManifest {
            modpack_link: None,
            name: "Imported pack".into(),
            summary: String::new(),
            icon: None,
            display_version: "1.0".into(),
            minecraft_version: "unknown".into(),
            loader: ClientLoader::None,
            loader_version: None,
            override_files: Vec::new(),
            entries: Vec::new(),
        };
        let mut blobs = Vec::new();
        for (path, bytes) in files {
            manifest.entries.push(entry(
                format!("import-{}", manifest.entries.len()),
                path,
                EntrySource::Upload {
                    sha256: bytes.hash(),
                },
                true,
                Side::Optional,
            )?);
            blobs.push(bytes);
        }
        if manifest.entries.is_empty() {
            return Err(invalid());
        }
        validate_manifest(&manifest)?;
        manifest.minecraft_version.clear();
        return Ok((manifest, blobs));
    }
    if mr.is_some() == cf.is_some() {
        return Err(invalid());
    }
    let document: serde_json::Value =
        serde_json::from_slice(&mr.as_ref().or(cf.as_ref()).ok_or_else(invalid)?.bytes()?)
            .map_err(|_| invalid())?;
    let mut manifest = PackManifest {
            modpack_link: None,
        name: string(&document, "name")?.into(),
        summary: document["summary"].as_str().unwrap_or_default().into(),
        icon: None,
        display_version: document["versionId"]
            .as_str()
            .or(document["version"].as_str())
            .filter(|s| {
                s.split('.')
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            })
            .unwrap_or("1.0")
            .into(),
        minecraft_version: String::new(),
        loader: ClientLoader::None,
        loader_version: None,
        override_files: Vec::new(),
        entries: Vec::new(),
    };
    if mr.is_some() {
        if document["formatVersion"] != 1 || document["game"] != "minecraft" {
            return Err(invalid());
        }
        manifest.minecraft_version = string(&document["dependencies"], "minecraft")?.into();
        for (key, loader) in [
            ("fabric-loader", ClientLoader::Fabric),
            ("quilt-loader", ClientLoader::Quilt),
            ("forge", ClientLoader::Forge),
            ("neoforge", ClientLoader::Neoforge),
        ] {
            if let Some(version) = document["dependencies"][key].as_str() {
                if manifest.loader != ClientLoader::None {
                    return Err(invalid());
                }
                manifest.loader = loader;
                manifest.loader_version = Some(version.into());
            }
        }
        let rows = document["files"].as_array().ok_or_else(invalid)?;
        for (i, row) in rows.iter().enumerate() {
            let reference = if let Some(hash) = row["hashes"]["sha512"].as_str() {
                identify(hash)?
            } else {
                None
            };
            let source = reference.map_or(
                EntrySource::Manual {
                    reason: "import_provider_identity_missing".into(),
                },
                |r| EntrySource::Provider {
                    provider: r.provider,
                    project_id: r.project_id,
                    version_id: r.version_id,
                },
            );
            let server = match row["env"]["server"].as_str() {
                Some("required") => Side::Required,
                Some("optional") => Side::Optional,
                Some("unsupported") => Side::Unsupported,
                _ => Side::Optional,
            };
            let mut imported = entry(
                format!("import-{i}"),
                string(row, "path")?.into(),
                source,
                row["env"]["client"] != "optional",
                server,
            )?;
            imported.environment.client = match row["env"]["client"].as_str() {
                Some("unsupported") => Side::Unsupported,
                Some("optional") => Side::Optional,
                _ => Side::Required,
            };
            manifest.entries.push(imported);
        }
    } else {
        if document["manifestType"] != "minecraftModpack" || document["manifestVersion"] != 1 {
            return Err(invalid());
        }
        manifest.minecraft_version = string(&document["minecraft"], "version")?.into();
        let loaders = document["minecraft"]["modLoaders"]
            .as_array()
            .ok_or_else(invalid)?;
        for row in loaders {
            if row["primary"] == false {
                continue;
            }
            let id = string(row, "id")?;
            let (prefix, version) = id.split_once('-').ok_or_else(invalid)?;
            if manifest.loader != ClientLoader::None {
                return Err(invalid());
            }
            manifest.loader = match prefix {
                "fabric" => ClientLoader::Fabric,
                "quilt" => ClientLoader::Quilt,
                "forge" => ClientLoader::Forge,
                "neoforge" => ClientLoader::Neoforge,
                _ => return Err(invalid()),
            };
            manifest.loader_version = Some(version.into());
        }
        let rows = document["files"].as_array().ok_or_else(invalid)?;
        let references = rows
            .iter()
            .map(|row| {
                Ok((
                    row["projectID"].as_u64().ok_or_else(invalid)?.to_string(),
                    row["fileID"].as_u64().ok_or_else(invalid)?.to_string(),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        providers
            .prefetch_curseforge(&references)
            .map_err(ReleaseError::from)?;
        for ((i, row), (project, version)) in rows.iter().enumerate().zip(references) {
            let source = providers.archive_filename(Provider::Curseforge, &project, &version);
            let filename = match source {
                Ok(filename) => filename,
                // A draft can retain a restricted file's real identity. Installing
                // it still requires a permitted provider download in preflight.
                Err(crate::content::ContentError::DownloadUnavailable { filename }) => filename,
                Err(error) => {
                    return Err(ReleaseError::provider_file(
                        error,
                        "curseforge",
                        &project,
                        &version,
                        "metadata",
                        None,
                    ));
                }
            };
            let path = format!("mods/{filename}");
            manifest.entries.push(entry(
                format!("import-{i}"),
                path,
                EntrySource::Provider {
                    provider: "curseforge".into(),
                    project_id: project,
                    version_id: version,
                },
                row["required"] != false,
                Side::Optional,
            )?);
        }
    }
    let cf_prefix = document["overrides"].as_str().unwrap_or("overrides");
    vesta_protocol::pack::catalogue::RelativePath::parse(cf_prefix)
        .map_err(super::super::PackError::from)?;
    let mut blobs = Vec::new();
    for (name, bytes) in files {
        if cf.is_some() && name == "modlist.html" {
            continue;
        }
        // CurseForge exports may include profile artwork beside the manifest.
        // It is metadata, never a game override or a server install destination.
        if cf.is_some() && name.starts_with("profileImage/")
            && matches!(name.rsplit('.').next(), Some("png" | "jpg" | "jpeg"))
        {
            continue;
        }
        let scoped = name
            .strip_prefix("client-overrides/")
            .map(|p| ("client-overrides", p))
            .or_else(|| {
                name.strip_prefix("server-overrides/")
                    .map(|p| ("server-overrides", p))
            })
            .or_else(|| {
                name.strip_prefix(&format!("{cf_prefix}/"))
                    .map(|p| ("overrides", p))
            });
        if let Some((root, path)) = scoped {
            manifest.override_files.push(OverrideFile {
                root: if root == "overrides" && (path.starts_with("global_packs/") || path.starts_with("journeymap/") || path.starts_with("fancymenu_data/") || path.to_ascii_lowercase().ends_with(".png")) { "client-overrides" } else { root }.into(),
                path: path.into(),
                sha256: Some(bytes.hash()),
                trashed: false,
            });
            blobs.push(bytes);
        } else {
            return Err(invalid());
        }
    }
    super::files::validate_files(&manifest.override_files)?;
    validate_manifest(&manifest)?;
    Ok((manifest, blobs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    struct Providers;
    impl ProviderClient for Providers {
        fn resolve(
            &self,
            provider: Provider,
            project: &str,
            version: &str,
        ) -> crate::content::Result<crate::content::ResolvedSource> {
            assert_eq!(
                (provider, project, version),
                (Provider::Curseforge, "123", "456")
            );
            Ok(crate::content::ResolvedSource {
                provider,
                filename: "example's+26.3.jar".into(),
                download_url: "https://edge.forgecdn.net/example.jar".into(),
                expected_hash: crate::content::ExpectedHash::Sha1("a".repeat(40)),
            })
        }
    }
    fn archive(rows: &[(&str, &[u8])]) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in rows {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(bytes).unwrap();
        }
        z.finish().unwrap().into_inner()
    }
    #[test]
    fn mrpack_preserves_optional_side_and_embeds_overrides_without_trusting_urls() {
        let document = serde_json::json!({"formatVersion":1,"game":"minecraft","name":"Imported","versionId":"2.0","dependencies":{"minecraft":"1.21.1","fabric-loader":"0.16.10"},"files":[{"path":"mods/example.jar","hashes":{"sha512":"a".repeat(128)},"downloads":["http://127.0.0.1/private"],"env":{"client":"optional","server":"required"}}]});
        let bytes = archive(&[
            (
                "modrinth.index.json",
                &serde_json::to_vec(&document).unwrap(),
            ),
            ("overrides/config/client.toml", b"foo=true"),
        ]);
        let (m, blobs) = parse_archive(&bytes, &Providers, |_| Ok(None)).unwrap();
        assert_eq!(m.loader, ClientLoader::Fabric);
        assert!(!m.entries[0].required);
        assert_eq!(m.entries[0].environment.server, Side::Required);
        assert!(matches!(m.entries[0].source, EntrySource::Manual { .. }));
        assert_eq!(blobs, vec![b"foo=true".to_vec()]);
    }
    #[test]
    fn mrpack_retains_server_only_mods_and_separates_override_roots() {
        let document = serde_json::json!({"formatVersion":1,"game":"minecraft","name":"Sides","versionId":"1.0","dependencies":{"minecraft":"1.21.1","fabric-loader":"0.16.10"},"files":[
            {"path":"mods/server+1.jar","hashes":{"sha512":"a".repeat(128)},"env":{"client":"unsupported","server":"required"}},
            {"path":"mods/client+1.jar","hashes":{"sha512":"b".repeat(128)},"env":{"client":"required","server":"unsupported"}}
        ]});
        let (manifest, blobs) = parse_archive(
            &archive(&[
                (
                    "modrinth.index.json",
                    &serde_json::to_vec(&document).unwrap(),
                ),
                ("overrides/config/shared.json", b"shared"),
                ("server-overrides/config/shared.json", b"server"),
                ("client-overrides/config/shared.json", b"client"),
            ]),
            &Providers,
            |hash| {
                Ok(Some(crate::content::ContentProvider {
                    provider: "modrinth".into(),
                    project_id: if hash.starts_with('a') {
                        "server"
                    } else {
                        "client"
                    }
                    .into(),
                    version_id: "v1".into(),
                }))
            },
        )
        .unwrap();
        assert_eq!(manifest.entries.len(), 2);
        assert_eq!(manifest.entries[0].environment.client, Side::Unsupported);
        assert_eq!(manifest.entries[0].environment.server, Side::Required);
        assert_eq!(manifest.entries[1].environment.server, Side::Unsupported);
        assert_eq!(manifest.override_files.len(), 3);
        assert_eq!(blobs.len(), 3);
    }
    #[test]
    fn curseforge_import_pins_ids_and_uses_the_provider_filename() {
        let document=br#"{"manifestType":"minecraftModpack","manifestVersion":1,"name":"CF","version":"1.2","minecraft":{"version":"1.21.1","modLoaders":[{"id":"forge-52.0.0","primary":true}]},"files":[{"projectID":123,"fileID":456,"required":true}]}"#;
        let (m, _) = parse_archive(
            &archive(&[
                ("manifest.json", document),
                ("modlist.html", b"<html>metadata</html>"),
            ]),
            &Providers,
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(m.entries[0].path, "mods/example's+26.3.jar");
        assert_eq!(m.loader_version.as_deref(), Some("52.0.0"));
    }
    #[test]
    fn curseforge_client_assets_retain_colour_codes_and_stay_client_only() {
        let document = br#"{"manifestType":"minecraftModpack","manifestVersion":1,"name":"CF","version":"3.4","minecraft":{"version":"26.3","modLoaders":[{"id":"fabric-0.19.5","primary":true}]},"files":[]}"#;
        let (m, _) = parse_archive(&archive(&[
            ("manifest.json", document),
            ("overrides/global_packs/required_resources/3 \u{00a7}lGUI Experience+\u{00a7}0.zip", b"asset"),
            ("overrides/journeymap/server/minecraft~overworld.config", b"{}"),
            ("overrides/fancymenu_data/menu.json", b"{}"),
            ("overrides/Evon's ModPack Logo Fabric.png", b"image"),
            ("profileImage/Evon's ModPack Logo Fabric.png", b"image"),
        ]), &Providers, |_| unreachable!()).unwrap();
        assert_eq!(m.override_files.len(), 4);
        assert!(m.override_files.iter().all(|f| f.root == "client-overrides"));
    }
    #[test]
    fn curseforge_draft_retains_restricted_file_identity_and_preserves_provider_errors() {
        struct Restricted(bool);
        impl ProviderClient for Restricted {
            fn resolve(
                &self,
                _: Provider,
                _: &str,
                _: &str,
            ) -> crate::content::Result<crate::content::ResolvedSource> {
                Err(if self.0 {
                    crate::content::ContentError::DownloadUnavailable {
                        filename: "mielon's-the-sift+26.3.jar".into(),
                    }
                } else {
                    crate::content::ContentError::DownloadFailed
                })
            }
        }
        let document=br#"{"manifestType":"minecraftModpack","manifestVersion":1,"name":"CF","version":"1.2","minecraft":{"version":"26.3","modLoaders":[{"id":"fabric-0.19.5","primary":true}]},"files":[{"projectID":1691925,"fileID":9022781,"required":true}]}"#;
        let bytes = archive(&[("manifest.json", document)]);
        let (manifest, _) = parse_archive(&bytes, &Restricted(true), |_| unreachable!()).unwrap();
        assert_eq!(manifest.entries[0].path, "mods/mielon's-the-sift+26.3.jar");
        assert!(
            matches!(&manifest.entries[0].source, EntrySource::Provider { project_id, version_id, .. } if project_id == "1691925" && version_id == "9022781")
        );
        let error = parse_archive(&bytes, &Restricted(false), |_| unreachable!()).unwrap_err();
        assert_eq!(error.code(), "content_pack_download_failed");
        assert!(
            matches!(error, ReleaseError::ProviderFile { details, .. } if details.project_id == "1691925" && details.version_id == "9022781" && details.stage == "metadata")
        );
    }
    #[test]
    fn plain_zip_retains_bytes_and_defers_runtime_to_the_server() {
        let (m, blobs) = parse_archive(
            &archive(&[
                ("mods/example.jar", b"jar"),
                ("config/example.toml", b"x=true"),
            ]),
            &Providers,
            |_| unreachable!(),
        )
        .unwrap();
        assert!(m.minecraft_version.is_empty());
        assert_eq!(m.entries.len(), 2);
        assert_eq!(blobs.len(), 2);
        assert!(
            m.entries
                .iter()
                .all(|e| matches!(e.source, EntrySource::Upload { .. }))
        );
        assert!(
            parse_archive(
                &archive(&[("mods/a.jar", b"x"), ("mods/A.jar", b"y")]),
                &Providers,
                |_| unreachable!()
            )
            .is_err()
        );
        assert!(
            parse_archive(
                &archive(&[("server.properties", b"x")]),
                &Providers,
                |_| unreachable!()
            )
            .is_ok()
        );
    }
    #[test]
    fn archive_traversal_refuses_but_unusual_non_executable_content_can_be_reviewed() {
        assert!(parse_archive(&archive(&[("../secret", b"x")]), &Providers, |_| Ok(None)).is_err());
        assert!(parse_archive(&archive(&[("hello.txt", b"x")]), &Providers, |_| Ok(None)).is_ok());
    }
}

#[cfg(test)]
mod live_triage {
    use super::*;
    #[test]
    #[ignore = "requires downloaded provider archive and live CurseForge credentials"]
    fn evon_provider_archive_ingests() {
        let bytes = std::fs::read(std::env::var("VESTA_TRIAGE_ARCHIVE").unwrap()).unwrap();
        let providers = UreqProviderClient::new(Some(std::env::var("CURSEFORGE_API_KEY").unwrap()))
            .with_deadline(std::time::Instant::now() + std::time::Duration::from_secs(180)).unwrap();
        let fixture = super::super::tests::Fixture::new();
        let store = Store::open(&fixture.db).unwrap();
        let (manifest, _) = parse_zip(Cursor::new(bytes), &providers, |_| Ok(None), Some(&store)).unwrap();
        assert_eq!(manifest.loader, ClientLoader::Fabric);
        assert_eq!(manifest.minecraft_version, "26.3");
        assert!(manifest.override_files.iter().any(|f| f.root == "client-overrides" && f.path.starts_with("global_packs/")));
    }
}

fn archive_review_warnings(bytes: &[u8]) -> Result<Vec<InstallWarning>> {
    if bytes.iter().copied().find(|b| !b.is_ascii_whitespace()) == Some(b'{') { return Ok(Vec::new()); }
    super::install::archive_policy::inspect(Cursor::new(bytes), bytes.len() as u64, "imported archive")
}
