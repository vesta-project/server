//! Owner server creation from a modpack, `kind:"modpack"` of
//! `protocol/host-v1/create.md`.
//!
//! The source is an immutable provider version (`modrinth|curseforge`) or a
//! host-private uploaded archive. Planning reviews the pack with the same
//! provider resolution, manifest parsers and archive policy as a server's own
//! pack install, then plans exactly like `new` with the pack's own Minecraft
//! version and loader (never the client's): the same exact software
//! resolution, Mojang check, automatic Java, resources and space. Apply runs
//! the `new` install (verified base software, EULA record, trusted launch from
//! the installer's own result), then installs the pack's content through the
//! new server's own pack engine before registering it once. Pack scripts and
//! Java are never used.
use super::*;
use super::create::CreatePrivate;
use crate::machine::creates::{Catalogue, CreateInstaller};
use crate::packs::workspace::{CreatePackReview, check_create_upload_archive};
use std::io::Write;

/// Uploads expire 24 hours after they finish.
pub const UPLOAD_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// Compressed upload limit.
pub const UPLOAD_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Live (unexpired) uploads kept for the whole host.
const MAX_UPLOADS: i64 = 16;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PackSourceRequest {
    Provider {
        provider: String,
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "versionId")]
        version_id: String,
    },
    Upload {
        #[serde(rename = "uploadId")]
        upload_id: String,
    },
}
/// `{"kind":"modpack",...}` plan request. Null port/memory take the defaults.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModpackServerRequest {
    pub kind: String,
    pub name: String,
    pub source: PackSourceRequest,
    pub game_port: Option<u16>,
    #[serde(rename = "memoryMiB", alias = "memoryMib")]
    pub memory_mib: Option<u64>,
    /// Accepted for symmetry with apply; planning never accepts the EULA.
    #[serde(default)]
    pub eula_accepted: bool,
}

/// Host-private half of a modpack plan.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackPrivate {
    pub source: PackSourceRequest,
    /// `None` when the pack could not be read (the plan is blocked).
    pub review: Option<CreatePackReview>,
}
/// The pack's contribution to the shared planning core.
pub(super) struct PackPlan {
    pub summary: serde_json::Value,
    pub blockers: Vec<&'static str>,
    pub warnings: Vec<&'static str>,
    pub content_bytes: u64,
    pub private: PackPrivate,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UploadAccepted {
    pub upload_id: String,
    /// Milliseconds since the Unix epoch; transport renders RFC 3339.
    pub expires_at: u64,
}

fn provider_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn validate_modpack(request: &ModpackServerRequest) -> JobResult<()> {
    let name = request.name.trim();
    if request.kind != "modpack"
        || name.is_empty()
        || name.len() > 128
        || name.chars().any(char::is_control)
        || request.game_port == Some(0)
    {
        return Err(JobError::INVALID);
    }
    match &request.source {
        PackSourceRequest::Provider {
            provider,
            project_id,
            version_id,
        } => {
            if !matches!(provider.as_str(), "modrinth" | "curseforge")
                || !provider_id(project_id)
                || !provider_id(version_id)
                || (provider == "curseforge"
                    && !(project_id.bytes().all(|b| b.is_ascii_digit())
                        && version_id.bytes().all(|b| b.is_ascii_digit())))
            {
                return Err(JobError::INVALID);
            }
        }
        PackSourceRequest::Upload { upload_id } => {
            Uuid::parse_str(upload_id).map_err(|_| JobError::INVALID)?;
        }
    }
    Ok(())
}

/// Streams one host-private upload to disk with the compressed size limit.
/// Dropped before [`Self::finish`], the partial file is removed.
pub struct CreateUpload {
    jobs: ImportJobs,
    upload_id: String,
    account_id: String,
    device_id: String,
    path: PathBuf,
    file: Option<fs::File>,
    bytes: u64,
    hasher: Sha256,
}
impl CreateUpload {
    /// `request_too_large` past 2 GiB.
    pub fn write(&mut self, data: &[u8]) -> JobResult<()> {
        let file = self.file.as_mut().ok_or(JobError::UNAVAILABLE)?;
        self.bytes = self
            .bytes
            .checked_add(data.len() as u64)
            .filter(|n| *n <= UPLOAD_MAX_BYTES)
            .ok_or(JobError("request_too_large"))?;
        self.hasher.update(data);
        file.write_all(data).map_err(|_| JobError::UNAVAILABLE)
    }
    /// Publish the upload for the owner: 24 hours, bound to the account.
    pub fn finish(mut self) -> JobResult<UploadAccepted> {
        let file = self.file.take().ok_or(JobError::UNAVAILABLE)?;
        if self.bytes == 0 {
            let _ = fs::remove_file(&self.path);
            return Err(JobError::INVALID);
        }
        file.sync_all().map_err(|_| JobError::UNAVAILABLE)?;
        drop(file);
        let sha256 = hex::encode(std::mem::take(&mut self.hasher).finalize());
        let jobs = self.jobs.clone();
        let target = jobs.upload_file(&self.upload_id);
        if fs::rename(&self.path, &target).is_err() {
            let _ = fs::remove_file(&self.path);
            return Err(JobError::UNAVAILABLE);
        }
        let now = jobs.0.host.now_ms();
        let expires = now.saturating_add(UPLOAD_TTL_MS as i64);
        let stored = jobs.write(|c| {
            c.execute(
                "INSERT INTO machine_create_uploads(upload_id,account_id,device_id,kind,bytes,sha256,created_ms,expires_ms) VALUES(?1,?2,?3,'modpack',?4,?5,?6,?7)",
                params![self.upload_id, self.account_id, self.device_id, self.bytes as i64, sha256, now, expires],
            )?;
            audit(c, "create.upload", &self.account_id, &self.upload_id, now)?;
            Ok(())
        });
        if let Err(error) = stored {
            let _ = fs::remove_file(&target);
            return Err(error);
        }
        Ok(UploadAccepted {
            upload_id: self.upload_id.clone(),
            expires_at: expires as u64,
        })
    }
}
impl Drop for CreateUpload {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = fs::remove_file(&self.path);
        }
        if let Ok(mut uploading) = self.jobs.0.uploading.lock() {
            uploading.remove(&self.upload_id);
        }
    }
}

impl ImportJobs {
    fn uploads_dir(&self) -> PathBuf {
        self.0.servers.join(".create-uploads")
    }
    fn upload_file(&self, upload_id: &str) -> PathBuf {
        self.uploads_dir().join(format!("{upload_id}.zip"))
    }
    /// Remove expired uploads and their files.
    fn prune_uploads(&self) -> JobResult<()> {
        let now = self.0.host.now_ms();
        let expired: Vec<String> = self.write(|c| {
            let mut stmt =
                c.prepare("SELECT upload_id FROM machine_create_uploads WHERE expires_ms<=?1")?;
            let ids = stmt
                .query_map([now], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            c.execute(
                "DELETE FROM machine_create_uploads WHERE expires_ms<=?1",
                [now],
            )?;
            Ok(ids)
        })?;
        for id in expired {
            let _ = fs::remove_file(self.upload_file(&id));
        }
        // Partial files from interrupted uploads.
        if let Ok(entries) = fs::read_dir(self.uploads_dir()) {
            let live: HashSet<String> = self.read(|c| {
                let mut stmt = c.prepare("SELECT upload_id FROM machine_create_uploads")?;
                let ids = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<HashSet<_>>>()?;
                Ok(ids)
            })?;
            let running = self
                .0
                .uploading
                .lock()
                .map_err(|_| JobError::UNAVAILABLE)?
                .clone();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let id = name
                    .strip_suffix(".zip")
                    .or_else(|| name.strip_suffix(".part"))
                    .unwrap_or(&name);
                if !live.contains(id) && !running.contains(id) {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }

    /// Start a host-private owner upload (`X-Upload-Kind: modpack`). Restore
    /// uploads answer `not_supported_yet`.
    pub fn begin_create_upload(
        &self,
        binding: &OwnerBinding,
        kind: &str,
    ) -> JobResult<CreateUpload> {
        match kind {
            "modpack" => {}
            "restore" => return Err(JobError("not_supported_yet")),
            _ => return Err(JobError::INVALID),
        }
        self.read(|c| self.owner(c, binding).map(|_| ()))?;
        self.prune_uploads()?;
        let live: i64 = self.read(|c| {
            Ok(c.query_row("SELECT count(*) FROM machine_create_uploads", [], |r| {
                r.get(0)
            })?)
        })?;
        let mut uploading = self.0.uploading.lock().map_err(|_| JobError::UNAVAILABLE)?;
        if live + uploading.len() as i64 >= MAX_UPLOADS {
            return Err(Kind::Create.saturated());
        }
        files::create_local_directory(&self.uploads_dir(), true)
            .map_err(|_| JobError::UNAVAILABLE)?;
        let upload_id = Uuid::new_v4().to_string();
        let path = self.uploads_dir().join(format!("{upload_id}.part"));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|_| JobError::UNAVAILABLE)?;
        uploading.insert(upload_id.clone());
        Ok(CreateUpload {
            jobs: self.clone(),
            upload_id,
            account_id: binding.account_id.clone(),
            device_id: binding.device_id.clone(),
            path,
            file: Some(file),
            bytes: 0,
            hasher: Sha256::new(),
        })
    }

    /// The owner's live upload: its file and SHA-256.
    pub(super) fn create_upload_in(
        &self,
        c: &Connection,
        account_id: &str,
        upload_id: &str,
    ) -> JobResult<(PathBuf, String)> {
        let row: Option<(String, i64)> = c
            .query_row(
                "SELECT sha256,expires_ms FROM machine_create_uploads WHERE upload_id=?1 AND account_id=?2",
                params![upload_id, account_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((sha, expires)) if expires > self.0.host.now_ms() => {
                let path = self.upload_file(upload_id);
                if fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                    Ok((path, sha))
                } else {
                    Err(JobError("upload_unavailable"))
                }
            }
            _ => Err(JobError("upload_unavailable")),
        }
    }

    /// Validate and record a planning modpack create job (the 202 response).
    pub fn begin_create_modpack(
        &self,
        binding: &OwnerBinding,
        request: &ModpackServerRequest,
    ) -> JobResult<PlanAccepted> {
        validate_modpack(request)?;
        if let PackSourceRequest::Upload { upload_id } = &request.source {
            // Only the owner's own live upload can be named.
            self.read(|c| self.create_upload_in(c, &binding.account_id, upload_id))
                .map_err(|_| JobError::INVALID)?;
        }
        let request = ModpackServerRequest {
            name: request.name.trim().to_owned(),
            ..request.clone()
        };
        let raw = serde_json::to_string(&request).map_err(|_| JobError::INVALID)?;
        self.insert_planning(binding, Kind::Create, "resolving", &raw)
    }

    pub(super) fn compute_modpack_plan(
        &self,
        binding: &OwnerBinding,
        request: &ModpackServerRequest,
        installer: &dyn CreateInstaller,
        catalogue: &dyn Catalogue,
    ) -> JobResult<(CreateReview, CreatePrivate)> {
        validate_modpack(request)?;
        let mut blockers: Vec<&'static str> = Vec::new();
        let mut warnings: Vec<&'static str> = Vec::new();
        let upload = match &request.source {
            PackSourceRequest::Upload { upload_id } => {
                match self.read(|c| self.create_upload_in(c, &binding.account_id, upload_id)) {
                    Ok(upload) => Some(upload),
                    Err(_) => {
                        blockers.push("upload_unavailable");
                        None
                    }
                }
            }
            PackSourceRequest::Provider { .. } => None,
        };
        let review = if !blockers.is_empty() {
            None
        } else {
            let safe = match &upload {
                Some((path, _)) => check_create_upload_archive(path).map(|_| ()),
                None => Ok(()),
            };
            match safe.and_then(|()| {
                installer.review_pack(&request.source, upload.as_ref().map(|u| u.0.as_path()))
            }) {
                Ok(review) => Some(review),
                Err(code) => {
                    blockers.push(code);
                    None
                }
            }
        };
        if let (Some(review), Some((_, sha))) = (&review, &upload)
            && &review.source_digest != sha
        {
            // The archive changed between upload and review.
            blockers.push("pack_invalid");
        }
        if let Some(review) = &review {
            if review.client_only {
                blockers.push("pack_client_only");
            }
            // Loader-less packs run on Vanilla, which cannot load mods.
            if review.mods_without_loader {
                blockers.push("pack_loader_missing");
            }
            if review.issues.iter().any(|i| i.blocking) {
                blockers.push("pack_review_blocked");
            } else if !review.issues.is_empty() {
                warnings.push("pack_review_warning");
            }
        }
        let (provider, project, version) = match &request.source {
            PackSourceRequest::Provider {
                provider,
                project_id,
                version_id,
            } => (
                Some(provider.clone()),
                Some(project_id.clone()),
                Some(version_id.clone()),
            ),
            PackSourceRequest::Upload { .. } => (None, None, None),
        };
        let summary = serde_json::json!({
            "name": review.as_ref().map(|r| &r.name),
            "provider": provider, "projectId": project, "versionId": version,
            "minecraftVersion": review.as_ref().map(|r| &r.minecraft_version),
            "loader": review.as_ref().map(|r| &r.loader),
            "issues": review.as_ref().map_or(Vec::new(), |r| r.issues.clone()),
        });
        // Minecraft and loader always come from the pack itself.
        let new = NewServerRequest {
            kind: "new".into(),
            name: request.name.trim().to_owned(),
            minecraft_version: review
                .as_ref()
                .map_or_else(String::new, |r| r.minecraft_version.clone()),
            software: SoftwareChoice {
                kind: review
                    .as_ref()
                    .map_or_else(|| "unknown".into(), |r| r.loader.clone()),
                build: None,
                loader_version: review.as_ref().and_then(|r| r.loader_version.clone()),
            },
            game_port: request.game_port,
            memory_mib: request.memory_mib,
            eula_accepted: false,
        };
        let software = review.as_ref().and_then(|r| {
            // A loader pack must pin its exact loader version.
            if r.loader != "vanilla" && r.loader_version.is_none() {
                return None;
            }
            super::create::validate_new(&new).ok()
        });
        if review.is_some() && software.is_none() {
            blockers.push("pack_invalid");
        }
        let content_bytes = review.as_ref().and_then(|r| r.content_bytes).unwrap_or(0);
        let pack = super::modpack::PackPlan {
            summary,
            blockers,
            warnings,
            content_bytes,
            private: PackPrivate {
                source: request.source.clone(),
                review,
            },
        };
        let software = if pack.blockers.iter().any(|b| {
            matches!(
                *b,
                "pack_invalid" | "upload_unavailable" | "unsafe_archive" | "source_too_large"
            )
        }) {
            None
        } else {
            software
        };
        self.compute_create_core(binding, &new, software, Some(pack), None, installer, catalogue)
    }
}
