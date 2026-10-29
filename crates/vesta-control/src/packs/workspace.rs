//! Protected, revisioned client-pack workspace. No writes to the game tree.
mod server_link;
pub use server_link::*;
mod server_updates;
pub use server_updates::*;
mod install;
pub use install::*;
mod files;
mod provider_pack;
pub use provider_pack::ProviderPackPublish;
mod import;
pub use files::*;
mod providers;
mod publication;
mod uploads;
pub use uploads::{UploadBegin, UploadChunk, UploadReceipt};
mod suggestions;
use super::ReleaseError;
use crate::identity::{self, Capability, Decision, Subject};
use base64::{Engine, engine::general_purpose::STANDARD};
pub use import::{ArchiveInspection, ImportRequest, InspectRequest};
pub use providers::{ProviderDependency, ProviderMetadata, WorkspaceProvider};
pub use publication::{PackDiff, PackIssue, PackPreview, PreviewRequest, WorkspacePublish};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
pub use suggestions::{PackSuggestion, PackSuggestions};
use vesta_state::{
    StateDatabase,
    audit::{AuditActor, AuditLog, AuditRecord},
};

pub type Result<T> = std::result::Result<T, ReleaseError>;
pub const MAX_ENTRIES: usize = 256;
pub const MAX_BLOBS: usize = 64 * 1024 * 1024;
pub fn invalid() -> ReleaseError {
    ReleaseError::Pack(super::PackError::InvalidPath)
}
pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub fn revision(value: &str) -> Result<u64> {
    let n = value.parse::<u64>().map_err(|_| invalid())?;
    if n.to_string() != value || n > vesta_protocol::pack::catalogue::MAX_REVISION {
        return Err(invalid());
    }
    Ok(n)
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientLoader {
    None,
    Fabric,
    Quilt,
    Neoforge,
    Forge,
}
impl ClientLoader {
    pub fn id(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Fabric => "fabric",
            Self::Quilt => "quilt",
            Self::Neoforge => "neoforge",
            Self::Forge => "forge",
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Mod,
    ResourcePack,
    Shader,
    Datapack,
    ConfigOverride,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Required,
    Optional,
    Unsupported,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub client: Side,
    pub server: Side,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EntrySource {
    Provider {
        provider: String,
        project_id: String,
        version_id: String,
    },
    Upload {
        sha256: String,
    },
    /// Import retained an unresolvable file identity. No imported URL is fetched.
    Manual {
        reason: String,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackEntry {
    pub id: String,
    pub kind: EntryKind,
    pub path: String,
    pub required: bool,
    pub environment: Environment,
    pub source: EntrySource,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    pub name: String,
    pub summary: String,
    pub icon: Option<String>,
    pub display_version: String,
    pub minecraft_version: String,
    pub loader: ClientLoader,
    pub loader_version: Option<String>,
    pub entries: Vec<PackEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub override_files: Vec<OverrideFile>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PackDraft {
    pub revision: String,
    pub manifest: PackManifest,
}
#[derive(Clone, Debug, Serialize)]
pub struct WorkspaceView {
    pub server_modpack: Option<ServerModpackLink>,
    pub client_pack: ClientPackConfig,
    pub customizing_provider: bool,
    pub provider_pack: Option<crate::content::ContentProvider>,
    pub draft: PackDraft,
    pub server_minecraft_version: String,
    pub minecraft_drift: bool,
    pub published_revision: String,
    pub minimum_revision: String,
    pub pending_publication: bool,
    pub safety_snapshot_recommended: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DraftWrite {
    pub expected_revision: String,
    pub manifest: PackManifest,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DraftAdd {
    pub expected_revision: String,
    pub entries: Vec<PackEntry>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSelection {
    pub provider: String,
    pub project_id: String,
    pub version_id: String,
    pub required: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderAdd {
    pub expected_revision: String,
    pub provider: String,
    pub project_id: String,
    pub version_id: String,
    pub required: bool,
    pub dependencies: Vec<ProviderSelection>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UploadRequest {
    pub content_base64: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct PackIcon {
    pub mime_type: String,
    pub content_base64: String,
}
pub(super) fn icon_mime(bytes: &[u8]) -> Result<&'static str> {
    if bytes.len() > 262144 {
        return Err(super::PackError::TooLarge.into());
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Ok("image/jpeg")
    } else {
        Err(invalid())
    }
}

pub struct PackWorkspace {
    db: StateDatabase,
    audit: AuditLog,
}
impl PackWorkspace {
    pub fn icon(&self, subject: &Subject, mc: &str) -> Result<Option<PackIcon>> {
        let draft = self.read(subject, mc)?;
        draft
            .manifest
            .icon
            .map(|sha| {
                let bytes = self.db.read_with(|s| self.blob_in(s.connection(), &sha))?;
                Ok(PackIcon {
                    mime_type: icon_mime(&bytes)?.into(),
                    content_base64: STANDARD.encode(bytes),
                })
            })
            .transpose()
    }
    pub fn add_provider(
        &self,
        subject: &Subject,
        mc: &str,
        key: &str,
        input: &ProviderAdd,
        providers: &dyn WorkspaceProvider,
    ) -> Result<PackDraft> {
        if let Some(outcome) = self.replay(subject, key, input)? {
            return Ok(outcome);
        }
        let draft = self.read(subject, mc)?;
        if draft.revision != input.expected_revision || input.dependencies.len() >= MAX_ENTRIES {
            return Err(ReleaseError::Conflict);
        }
        let mut selected = vec![ProviderSelection {
            provider: input.provider.clone(),
            project_id: input.project_id.clone(),
            version_id: input.version_id.clone(),
            required: input.required,
        }];
        selected.extend(input.dependencies.clone());
        let mut entries = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        for selection in selected {
            if std::time::Instant::now() > deadline {
                return Err(super::PackError::Unavailable.into());
            }
            if !matches!(selection.provider.as_str(), "modrinth" | "curseforge")
                || !identifier(&selection.project_id)
                || !identifier(&selection.version_id)
            {
                return Err(invalid());
            }
            let meta = providers.pack_metadata(
                &selection.provider,
                &selection.project_id,
                &selection.version_id,
            )?;
            if !providers::compatible(&meta, &draft.manifest) {
                return Err(invalid());
            }
            let prefix = match meta.kind {
                EntryKind::Mod => "mods",
                EntryKind::ResourcePack => "resourcepacks",
                EntryKind::Shader => "shaderpacks",
                EntryKind::Datapack => "datapacks",
                EntryKind::ConfigOverride => return Err(invalid()),
            };
            if meta.filename.contains('/') || meta.filename.contains('\\') {
                return Err(invalid());
            }
            entries.push(PackEntry {
                id: format!("{}-{}", selection.provider, selection.project_id),
                kind: meta.kind,
                path: format!("{prefix}/{}", meta.filename),
                required: selection.required,
                environment: Environment {
                    client: if selection.required {
                        Side::Required
                    } else {
                        Side::Optional
                    },
                    server: meta.server_requirement,
                },
                source: EntrySource::Provider {
                    provider: selection.provider,
                    project_id: selection.project_id,
                    version_id: selection.version_id,
                },
            });
        }
        self.mutate(subject, key, input, |c| {
            let mut manifest = self.read_in(c, mc)?.manifest;
            for entry in entries {
                if let Some(old) = manifest.entries.iter_mut().find(|e| e.id == entry.id) {
                    *old = entry;
                } else {
                    manifest.entries.push(entry);
                }
            }
            self.write_in(c, subject, mc, &input.expected_revision, manifest)
        })
    }
    pub fn server_mismatches(
        &self,
        subject: &Subject,
        mc: &str,
        server: &[crate::content::ContentEntry],
        truncated: bool,
        providers: &dyn WorkspaceProvider,
    ) -> Result<Vec<PackSuggestion>> {
        Ok(suggestions::verified_server_suggestions(
            &self.read(subject, mc)?,
            mc,
            server,
            truncated,
            providers,
        )
        .0)
    }
    pub fn open(db: &StateDatabase) -> Result<Self> {
        db.write(|c| c.execute_batch("CREATE TABLE IF NOT EXISTS pack_install_plans(id TEXT PRIMARY KEY, principal TEXT NOT NULL, device TEXT NOT NULL, expires INTEGER NOT NULL, token_hash TEXT NOT NULL, body TEXT NOT NULL)").map_err(Into::into))?;
        server_link::initialize(db)?;
        Ok(Self {
            db: db.clone(),
            audit: AuditLog::new(db.clone())?,
        })
    }
    pub fn authorize_in(
        &self,
        c: &Connection,
        subject: &Subject,
        capability: Capability,
    ) -> Result<()> {
        let policy = identity::policy_in(
            c,
            self.db.server_id(),
            self.db.clock().millis(),
            Some(subject),
        )?;
        if let Decision::Deny(reason) = policy.check(capability, None) {
            return Err(crate::ControlError::forbidden(reason).into());
        }
        Ok(())
    }
    pub fn read(&self, subject: &Subject, mc: &str) -> Result<PackDraft> {
        Ok(self.db.read_with(|s| {
            self.authorize_in(s.connection(), subject, Capability::PackEdit)?;
            self.read_in(s.connection(), mc)
        })?)
    }
    fn read_in(&self, c: &Connection, mc: &str) -> Result<PackDraft> {
        let row: Option<(i64, String)> = c
            .query_row(
                "SELECT revision,manifest_json FROM pack_workspace WHERE server_id=?",
                [self.db.server_id()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((rev, json)) => Ok(PackDraft {
                revision: rev.to_string(),
                manifest: serde_json::from_str(&json).map_err(|_| invalid())?,
            }),
            None => Ok(PackDraft {
                revision: "0".into(),
                manifest: PackManifest {
                    name: "Client pack".into(),
                    summary: String::new(),
                    icon: None,
                    display_version: "1.0".into(),
                    minecraft_version: mc.into(),
                    loader: ClientLoader::None,
                    loader_version: None,
                    override_files: Vec::new(),
                    entries: Vec::new(),
                },
            }),
        }
    }
    pub fn view(
        &self,
        subject: &Subject,
        mc: &str,
        releases: &super::PackReleases,
    ) -> Result<WorkspaceView> {
        let draft = self.read(subject, mc)?;
        let release = releases.current()?;
        let published = self.published(subject)?;
        let published_draft: Option<i64> = self.db.read(|c| {
            Ok(c.query_row(
                "SELECT draft_revision FROM pack_workspace_published WHERE server_id=?",
                [self.db.server_id()],
                |r| r.get(0),
            )
            .optional()?)
        })?;
        let provider_pack = if let Some(release) = &release {
            if release.artifact().format() == super::ArtifactFormat::Catalogue {
                let loaded = releases
                    .artifacts()
                    .load(release.artifact(), super::MAX_ARTIFACT_BYTES)?;
                let catalogue = vesta_protocol::pack::catalogue::Manifest::decode(loaded.bytes())
                    .map_err(super::PackError::from)?;
                if let vesta_protocol::pack::catalogue::PackContent::Platform(pack) =
                    catalogue.content()
                {
                    let source = pack.source();
                    Some(crate::content::ContentProvider {
                        provider: match source.platform_name() {
                            Some(vesta_protocol::pack::catalogue::Platform::Modrinth) => "modrinth",
                            Some(vesta_protocol::pack::catalogue::Platform::Curseforge) => {
                                "curseforge"
                            }
                            _ => return Err(invalid()),
                        }
                        .into(),
                        project_id: source.project_id().ok_or_else(invalid)?.into(),
                        version_id: source.version_id().ok_or_else(invalid)?.into(),
                    })
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        let customizing_provider = self.db.read(|c| {
            Ok(c.query_row(
                "SELECT customizing_provider FROM pack_workspace WHERE server_id=?",
                [self.db.server_id()],
                |r| r.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false))
        })?;
        Ok(WorkspaceView {
            server_modpack: self.server_link(subject)?,
            client_pack: self.client_pack(subject, release.is_some())?,
            customizing_provider,
            provider_pack,
            minecraft_drift: draft.manifest.minecraft_version != mc,
            pending_publication: published.as_ref().is_none_or(|p| {
                published_draft.map(|r| r.to_string()).as_ref() != Some(&draft.revision)
                    || release.as_ref().is_none_or(|r| r.revision() != p.0)
            }),
            draft,
            server_minecraft_version: mc.into(),
            published_revision: release.as_ref().map_or(0, |r| r.revision()).to_string(),
            minimum_revision: release
                .as_ref()
                .map_or(0, |r| r.minimum_revision())
                .to_string(),
            safety_snapshot_recommended: true,
        })
    }
    pub fn published(&self, subject: &Subject) -> Result<Option<(u64, PackManifest)>> {
        Ok(self.db.read_with(|s| {
            self.authorize_in(s.connection(),subject,Capability::PackEdit)?;
            let row: Option<(i64,String)>=s.connection().query_row("SELECT release_revision,manifest_json FROM pack_workspace_published WHERE server_id=?",[self.db.server_id()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            row.map(|(r,j)|Ok((r as u64,serde_json::from_str(&j).map_err(|_| invalid())?))).transpose()
        })?)
    }
    pub fn write(
        &self,
        subject: &Subject,
        mc: &str,
        key: &str,
        input: &DraftWrite,
    ) -> Result<PackDraft> {
        self.mutate(subject, key, input, |c| {
            self.write_in(
                c,
                subject,
                mc,
                &input.expected_revision,
                input.manifest.clone(),
            )
        })
    }
    pub fn add(
        &self,
        subject: &Subject,
        mc: &str,
        key: &str,
        input: &DraftAdd,
    ) -> Result<PackDraft> {
        self.mutate(subject, key, input, |c| {
            let mut manifest = self.read_in(c, mc)?.manifest;
            for entry in &input.entries {
                if let Some(old) = manifest.entries.iter_mut().find(|old| old.id == entry.id) {
                    *old = entry.clone();
                } else {
                    manifest.entries.push(entry.clone());
                }
            }
            self.write_in(c, subject, mc, &input.expected_revision, manifest)
        })
    }
    pub fn delete(
        &self,
        subject: &Subject,
        mc: &str,
        key: &str,
        expected: &str,
    ) -> Result<PackDraft> {
        self.mutate(subject, key, &("delete", expected), |c| {
            let mut manifest = self.read_in(c, mc)?.manifest;
            manifest.entries.clear();
            manifest.minecraft_version = mc.into();
            manifest.loader = ClientLoader::None;
            manifest.loader_version = None;
            self.write_in(c, subject, mc, expected, manifest)
        })
    }
    fn write_in(
        &self,
        c: &Connection,
        subject: &Subject,
        mc: &str,
        expected: &str,
        manifest: PackManifest,
    ) -> Result<PackDraft> {
        self.authorize_in(c, subject, Capability::PackEdit)?;
        if self.read_in(c, mc)?.revision != expected {
            return Err(ReleaseError::Conflict);
        }
        validate_manifest(&manifest)?;
        files::validate_files(&manifest.override_files)?;
        for file in &manifest.override_files {
            if let Some(sha) = &file.sha256 {
                self.blob_in(c, sha)?;
            }
        }
        if manifest.minecraft_version != mc {
            return Err(ReleaseError::RevisionMismatch);
        }
        for entry in &manifest.entries {
            if let EntrySource::Upload { sha256 } = &entry.source {
                self.blob_in(c, sha256)?;
            }
        }
        if let Some(icon) = &manifest.icon {
            icon_mime(&self.blob_in(c, icon)?)?;
        }
        let next = revision(expected)?
            .checked_add(1)
            .filter(|n| *n <= vesta_protocol::pack::catalogue::MAX_REVISION)
            .ok_or(ReleaseError::RevisionExhausted)?;
        let json = serde_json::to_string(&manifest).map_err(|_| invalid())?;
        c.execute("INSERT INTO pack_workspace(server_id,revision,manifest_json) VALUES(?,?,?) ON CONFLICT(server_id) DO UPDATE SET revision=excluded.revision,manifest_json=excluded.manifest_json",params![self.db.server_id(),next as i64,json])?;
        Ok(PackDraft {
            revision: next.to_string(),
            manifest,
        })
    }
    pub fn upload(&self, subject: &Subject, key: &str, input: &UploadRequest) -> Result<String> {
        self.mutate(subject, key, input, |c| {
            let bytes = STANDARD
                .decode(&input.content_base64)
                .map_err(|_| invalid())?;
            self.store_blob(c, &bytes)
        })
    }
    fn store_blob(&self, c: &Connection, bytes: &[u8]) -> Result<String> {
        if bytes.len() > super::MAX_PINNED_FILE_BYTES {
            return Err(super::PackError::TooLarge.into());
        }
        let sha = digest(bytes);
        let exists: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM pack_workspace_blobs WHERE server_id=? AND sha256=?)",
            params![self.db.server_id(), sha],
            |r| r.get(0),
        )?;
        if !exists {
            let total: i64 = c.query_row(
                "SELECT coalesce(sum(length(body)),0) FROM pack_workspace_blobs WHERE server_id=?",
                [self.db.server_id()],
                |r| r.get(0),
            )?;
            if total as usize + bytes.len() > MAX_BLOBS {
                return Err(super::PackError::TooLarge.into());
            }
            c.execute(
                "INSERT INTO pack_workspace_blobs(server_id,sha256,body) VALUES(?,?,?)",
                params![self.db.server_id(), sha, bytes],
            )?;
        }
        Ok(sha)
    }
    fn blob_in(&self, c: &Connection, sha: &str) -> Result<Vec<u8>> {
        let bytes: Option<Vec<u8>> = c
            .query_row(
                "SELECT body FROM pack_workspace_blobs WHERE server_id=? AND sha256=?",
                params![self.db.server_id(), sha],
                |r| r.get(0),
            )
            .optional()?;
        let bytes = bytes.ok_or(super::PackError::UnresolvedFile)?;
        if digest(&bytes) != sha {
            return Err(super::PackError::Conflict.into());
        }
        Ok(bytes)
    }
    fn replay<T: Serialize, R: serde::de::DeserializeOwned>(
        &self,
        subject: &Subject,
        key: &str,
        input: &T,
    ) -> Result<Option<R>> {
        let input = digest(&serde_json::to_vec(input).map_err(|_| invalid())?);
        Ok(self.db.read_with(|s| {
            let c=s.connection(); self.authorize_in(c,subject,Capability::PackEdit)?;
            let row:Option<(String,String)>=c.query_row("SELECT input_sha256,outcome_json FROM pack_workspace_receipts WHERE server_id=? AND principal_id=? AND device_id=? AND request_key=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            row.map(|(old,outcome)| { if old!=input {return Err(ReleaseError::IdempotencyConflict);} serde_json::from_str(&outcome).map_err(|_|invalid()) }).transpose()
        })?)
    }
    fn mutate<T: Serialize, R: Serialize + serde::de::DeserializeOwned>(
        &self,
        subject: &Subject,
        key: &str,
        input: &T,
        work: impl FnOnce(&Connection) -> Result<R>,
    ) -> Result<R> {
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(invalid());
        }
        let input = digest(&serde_json::to_vec(input).map_err(|_| invalid())?);
        let result=self.db.write_with(|t| {
            let c=t.connection(); self.authorize_in(c,subject,Capability::PackEdit)?;
            let row:Option<(String,String)>=c.query_row("SELECT input_sha256,outcome_json FROM pack_workspace_receipts WHERE server_id=? AND principal_id=? AND device_id=? AND request_key=?",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((old,outcome))=row { if old!=input { return Err(ReleaseError::IdempotencyConflict); } return serde_json::from_str(&outcome).map_err(|_|invalid()); }
            let count:i64=c.query_row("SELECT count(*) FROM pack_workspace_receipts",[],|r|r.get(0))?;
            if count>=10_000 { return Err(super::PackError::TooLarge.into()); }
            let result=work(c)?;
            c.execute("INSERT INTO pack_workspace_receipts(server_id,principal_id,device_id,request_key,input_sha256,outcome_json) VALUES(?,?,?,?,?,?)",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),key,input,serde_json::to_string(&result).map_err(|_|invalid())?])?;
            self.audit.append(c,&AuditRecord::new(AuditActor::new(subject.principal.as_str().into(),subject.device.as_str().into(),"pack-workspace".into())?,"pack_workspace.edit".into(),"pack-workspace".into(),"success".into(),None,[("input_sha256".into(),Some(input.clone()))])?)?;
            Ok(result)
        }).map_err(ReleaseError::from);
        if let Err(error) = &result {
            self.db.write(|c| {
                self.audit.append(
                    c,
                    &AuditRecord::new(
                        AuditActor::new(
                            subject.principal.as_str().into(),
                            subject.device.as_str().into(),
                            "pack-workspace".into(),
                        )?,
                        "pack_workspace.edit".into(),
                        "pack-workspace".into(),
                        "refused".into(),
                        None,
                        [("error_code".into(), Some(error.code().into()))],
                    )?,
                )
            })?;
        }
        result
    }
}
pub fn validate_manifest(m: &PackManifest) -> Result<()> {
    let text =
        |s: &str, n: usize| !s.is_empty() && s.len() <= n && !s.chars().any(char::is_control);
    if !text(&m.name, 256)
        || m.summary.len() > 4096
        || !text(&m.minecraft_version, 64)
        || !text(&m.display_version, 64)
        || !m
            .display_version
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        || m.entries.len() > MAX_ENTRIES
    {
        return Err(invalid());
    }
    match m.loader {
        ClientLoader::None if m.loader_version.is_some() => return Err(invalid()),
        ClientLoader::None => {}
        _ if !m.loader_version.as_deref().is_some_and(|s| text(s, 128)) => return Err(invalid()),
        _ => {}
    }
    if m.icon.as_ref().is_some_and(|s| !valid_sha(s)) {
        return Err(invalid());
    }
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut projects = BTreeSet::new();
    for e in &m.entries {
        vesta_protocol::pack::catalogue::RelativePath::parse(&e.path)
            .map_err(super::PackError::from)?;
        let lower = e.path.to_ascii_lowercase();
        let prefix = match e.kind {
            EntryKind::Mod => "mods/",
            EntryKind::ResourcePack => "resourcepacks/",
            EntryKind::Shader => "shaderpacks/",
            EntryKind::Datapack => "datapacks/",
            EntryKind::ConfigOverride => "config/",
        };
        let config_override = e.kind == EntryKind::ConfigOverride
            && (matches!(
                lower.as_str(),
                "options.txt" | "optionsof.txt" | "optionsshaders.txt" | "servers.dat"
            ) || ["defaultconfigs/", "kubejs/", "scripts/", "resources/"]
                .iter()
                .any(|p| lower.starts_with(p)));
        if !text(&e.id, 128)
            || !ids.insert(&e.id)
            || !(lower.starts_with(prefix) || config_override)
            || !paths.insert(lower.clone())
        {
            return Err(invalid());
        }
        for path in &paths {
            if path != &lower
                && (path.starts_with(&format!("{lower}/"))
                    || lower.starts_with(&format!("{path}/")))
            {
                return Err(invalid());
            }
        }
        match &e.source {
            EntrySource::Provider {
                provider,
                project_id,
                version_id,
            } => {
                if !matches!(provider.as_str(), "modrinth" | "curseforge")
                    || !identifier(project_id)
                    || !identifier(version_id)
                    || !projects.insert((provider, project_id))
                    || e.kind == EntryKind::ConfigOverride
                {
                    return Err(invalid());
                }
            }
            EntrySource::Upload { sha256 } if !valid_sha(sha256) => return Err(invalid()),
            EntrySource::Manual { reason } if !text(reason, 256) => return Err(invalid()),
            _ => {}
        }
    }
    if serde_json::to_vec(m).map_err(|_| invalid())?.len() > 1_048_576 {
        return Err(super::PackError::TooLarge.into());
    }
    // Use the publication catalogue's full namespace admission here as well:
    // disjoint leaves with directory-case aliases must fail during preview.
    use vesta_protocol::pack::catalogue::{
        FileSource, Manifest, ManifestMetadata, PackContent, PackFile,
    };
    let files = m
        .entries
        .iter()
        .map(|e| PackFile::new(&e.path, &"0".repeat(64), 0, FileSource::embedded()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(super::PackError::from)?;
    Manifest::new(
        ManifestMetadata::new("Draft", "1", 1, 1).map_err(super::PackError::from)?,
        PackContent::Custom(files),
    )
    .map_err(super::PackError::from)?;
    Ok(())
}
pub fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn valid_sha(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Serialize)]
pub struct PublishedRevision {
    pub revision: String,
    pub published_at: i64,
    pub published_by: Option<String>,
    pub required_update: bool,
    pub entry_count: Option<i64>,
    pub override_count: Option<i64>,
    pub provider_version: Option<String>,
}
impl PackWorkspace {
    pub fn history(&self, subject: &Subject) -> Result<Vec<PublishedRevision>> {
        Ok(self.db.read_with(|s| { let c=s.connection(); self.authorize_in(c, subject, Capability::PackEdit)?;
 let mut q=c.prepare("SELECT r.revision,r.published_at,h.published_by,coalesce(h.required_update,r.minimum_revision=r.revision),h.entry_count,h.override_count,h.provider_version FROM pack_releases r LEFT JOIN pack_publication_history h ON h.server_id=r.server_id AND h.revision=r.revision WHERE r.server_id=? ORDER BY r.revision DESC")?;
 let rows=q.query_map([self.db.server_id()], |r| Ok(PublishedRevision{revision:r.get::<_,i64>(0)?.to_string(),published_at:r.get(1)?,published_by:r.get(2)?,required_update:r.get(3)?,entry_count:r.get(4)?,override_count:r.get(5)?,provider_version:r.get(6)?}))?;
 Ok(rows.collect::<std::result::Result<Vec<_>,_>>()?)
 })?)
    }
}
