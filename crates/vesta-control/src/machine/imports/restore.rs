//! Owner server creation from a backup, `kind:"restore"` of
//! `protocol/host-v1/create.md`.
//!
//! Restore sources are restore points (profile or safety snapshots) in the
//! local backup repository of a server folder under the machine's servers
//! root that no live server owns: a server that was deleted (its tombstoned
//! folder and backups remain) or a folder the registry does not know. A live
//! server's repository is never listed. Planning reviews the snapshot with
//! the facts known for that server (Minecraft version and software recorded
//! when it was created or imported; `restore_version_unknown` otherwise),
//! automatic Java, resources and space. Apply revalidates the source,
//! restores the snapshot into a private staging directory (every chunk
//! re-hashed, every size checked; a corrupt snapshot fails with
//! `integrity_conflict` and nothing is published), derives the trusted launch
//! from the restored files exactly like an import's commit (detected jar or
//! Java argument file, never scripts), makes the matching Java available,
//! records the owner's EULA and the reserved port, publishes
//! `<serverId>/game` with one rename and registers the server once through
//! the shared create job. Uploaded recovery exports are not a source yet
//! (`not_supported_yet`).
use super::create::CreatePrivate;
use super::*;
use crate::backups::{engine::Engine as BackupEngine, store::LocalStore};
use crate::config::{BackupRepository, BackupSettings, HostConfiguration};

/// Space kept for a managed Java download (archive plus extraction).
const JAVA_RESERVE_BYTES: u64 = 512 * 1024 * 1024;
/// Server folders and restore points examined per listing.
const MAX_FOLDERS: usize = 4096;
const MAX_SOURCES: usize = 500;
/// Minimum interval between durable byte-progress writes.
const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum RestoreSourceRequest {
    Snapshot {
        #[serde(rename = "sourceId")]
        source_id: String,
    },
    Upload {
        #[serde(rename = "uploadId")]
        upload_id: String,
    },
}
/// `{"kind":"restore",...}` plan request. Null port/memory take the defaults.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoreServerRequest {
    pub kind: String,
    pub name: String,
    pub source: RestoreSourceRequest,
    pub game_port: Option<u16>,
    #[serde(rename = "memoryMiB", alias = "memoryMib")]
    pub memory_mib: Option<u64>,
    /// Accepted for symmetry with apply; planning never accepts the EULA.
    #[serde(default)]
    pub eula_accepted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RestoreSoftware {
    pub kind: String,
    pub version: Option<String>,
}
/// One `GET /v1/host/creates/restore-sources` entry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RestoreSource {
    pub source_id: String,
    pub label: String,
    /// RFC 3339 UTC time the snapshot was taken.
    pub created_at: String,
    pub minecraft_version: Option<String>,
    pub software: Option<RestoreSoftware>,
    pub bytes: u64,
}

/// Host-private half of a restore plan: exactly which snapshot of which
/// repository was reviewed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RestorePrivate {
    pub origin: Uuid,
    pub repository: PathBuf,
    pub snapshot: String,
    pub bytes: u64,
    pub files: u64,
}

/// A listed source with its private identity and the reviewable facts.
struct Candidate {
    public: RestoreSource,
    private: RestorePrivate,
    launchable: bool,
}

/// `<origin simple uuid><snapshot id>`: 64 lowercase hex characters.
fn source_id(origin: Uuid, snapshot: &str) -> String {
    format!("{}{snapshot}", origin.simple())
}
fn valid_source_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether the snapshot holds something the import launch detection can run:
/// a jar at the top of the game directory or a Forge/NeoForge argument file.
fn launchable(files: &[(String, u64)]) -> bool {
    files.iter().any(|(path, _)| {
        let lower = path.to_ascii_lowercase();
        (!lower.contains('/') && lower.ends_with(".jar"))
            || (lower.starts_with("libraries/")
                && (lower.ends_with("/unix_args.txt") || lower.ends_with("/win_args.txt")))
    })
}

fn validate_restore(request: &RestoreServerRequest) -> JobResult<()> {
    let name = request.name.trim();
    if request.kind != "restore"
        || name.is_empty()
        || name.len() > 128
        || name.chars().any(char::is_control)
        || request.game_port == Some(0)
    {
        return Err(JobError::INVALID);
    }
    match &request.source {
        RestoreSourceRequest::Snapshot { source_id } if valid_source_id(source_id) => Ok(()),
        RestoreSourceRequest::Snapshot { .. } => Err(JobError::INVALID),
        // Uploaded recovery exports have no adapter yet.
        RestoreSourceRequest::Upload { .. } => Err(JobError("not_supported_yet")),
    }
}

impl ImportJobs {
    /// The local repository a server folder's backups used: its configured
    /// local repository when that lies inside the folder, else the default
    /// `<folder>/backups/repository`. Remote (S3) repositories are not
    /// restore sources here.
    fn folder_repository(dir: &Path) -> Option<PathBuf> {
        let default = BackupSettings::for_instance(dir);
        let configured = fs::read(dir.join("host.toml"))
            .ok()
            .filter(|text| text.len() <= 1024 * 1024)
            .and_then(|text| HostConfiguration::parse_toml(&text, dir, Path::new("host.toml")).ok())
            .and_then(|config| config.backups().cloned());
        let settings = configured.unwrap_or(default);
        match settings.repository {
            BackupRepository::Local(path) if path.starts_with(dir) => Some(path),
            BackupRepository::Local(_) | BackupRepository::S3(_) => None,
        }
    }

    /// Every restore point not attached to a live server, newest first.
    fn restore_candidates(&self) -> JobResult<Vec<Candidate>> {
        let facts = self.server_facts()?;
        let Ok(entries) = fs::read_dir(&self.0.servers) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in entries.flatten().take(MAX_FOLDERS) {
            let Some(origin) = entry
                .file_name()
                .to_str()
                .and_then(|n| Uuid::parse_str(n).ok())
                .filter(|id| id.to_string() == entry.file_name().to_string_lossy())
            else {
                continue;
            };
            let dir = self.server_dir(origin);
            if !fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
            {
                continue;
            }
            // A live server's backups are its own; only deleted or unknown folders.
            let label = match self.0.host.server(origin)? {
                Some(e) if e.state != crate::machine::registry::ServerState::Deleted => continue,
                Some(e) => e.name,
                None => "Recovered server".to_owned(),
            };
            // A job still installing into this folder owns it.
            if self.read(|c| {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM machine_import_jobs WHERE server_id=?1 AND state IN ('applying','interrupted','planning'))",
                    [origin.to_string()],
                    |r| r.get::<_, bool>(0),
                )?)
            })? {
                continue;
            }
            let Some(repository) = Self::folder_repository(&dir) else {
                continue;
            };
            if !fs::symlink_metadata(&repository).is_ok_and(|m| m.is_dir()) {
                continue;
            }
            let engine = BackupEngine::new(Box::new(LocalStore::new(repository.clone())));
            let Ok(points) = engine.restore_points() else {
                continue;
            };
            let fact = facts.get(&origin);
            for (snapshot, _) in points {
                let Ok(files) = engine.snapshot_files(&snapshot.id) else {
                    continue;
                };
                let bytes = files.iter().fold(0u64, |s, (_, n)| s.saturating_add(*n));
                out.push(Candidate {
                    public: RestoreSource {
                        source_id: source_id(origin, &snapshot.id),
                        label: label.clone(),
                        created_at: snapshot.time.clone(),
                        minecraft_version: fact.and_then(|f| f.minecraft_version.clone()),
                        software: fact.and_then(|f| {
                            f.software
                                .clone()
                                .map(|(kind, version)| RestoreSoftware { kind, version })
                        }),
                        bytes,
                    },
                    private: RestorePrivate {
                        origin,
                        repository: repository.clone(),
                        snapshot: snapshot.id,
                        bytes,
                        files: files.len() as u64,
                    },
                    launchable: launchable(&files),
                });
            }
        }
        out.sort_by(|a, b| {
            b.public
                .created_at
                .cmp(&a.public.created_at)
                .then_with(|| a.public.source_id.cmp(&b.public.source_id))
        });
        out.truncate(MAX_SOURCES);
        Ok(out)
    }

    /// `GET /v1/host/creates/restore-sources` for an owner.
    pub fn restore_sources(&self, binding: &OwnerBinding) -> JobResult<Vec<RestoreSource>> {
        self.read(|c| self.owner(c, binding).map(|_| ()))?;
        Ok(self
            .restore_candidates()?
            .into_iter()
            .map(|c| c.public)
            .collect())
    }

    /// Validate and record a planning restore job (the 202 response).
    pub fn begin_create_restore(
        &self,
        binding: &OwnerBinding,
        request: &RestoreServerRequest,
    ) -> JobResult<PlanAccepted> {
        validate_restore(request)?;
        let request = RestoreServerRequest {
            name: request.name.trim().to_owned(),
            ..request.clone()
        };
        let raw = serde_json::to_string(&request).map_err(|_| JobError::INVALID)?;
        self.insert_planning(binding, Kind::Create, "resolving", &raw)
    }

    pub(super) fn compute_restore_plan(
        &self,
        binding: &OwnerBinding,
        request: &RestoreServerRequest,
    ) -> JobResult<(CreateReview, CreatePrivate)> {
        validate_restore(request)?;
        let RestoreSourceRequest::Snapshot { source_id } = &request.source else {
            return Err(JobError("not_supported_yet"));
        };
        let mut blockers: BTreeSet<&'static str> = BTreeSet::new();
        let mut warnings: BTreeSet<&'static str> = BTreeSet::new();
        let candidate = self
            .restore_candidates()?
            .into_iter()
            .find(|c| &c.public.source_id == source_id);
        if candidate.is_none() {
            blockers.insert("restore_source_unavailable");
        }
        if candidate.as_ref().is_some_and(|c| !c.launchable) {
            blockers.insert("launch_unresolved");
        }
        let version = candidate
            .as_ref()
            .and_then(|c| c.public.minecraft_version.clone());
        if candidate.is_some() && version.is_none() {
            warnings.insert("restore_version_unknown");
        }
        let java = self
            .0
            .env
            .plan_java(version.as_deref(), std::slice::from_ref(&self.0.servers));
        // An unknown version's Java is chosen from the restored files at apply.
        if version.is_some()
            && let Some(blocker) = java.blocker
        {
            blockers.insert(blocker);
        }
        // Resources against live reservations, exactly as apply re-checks them.
        let name = request.name.trim().to_owned();
        let (held, name_used) =
            self.read(|c| Ok((reservations_in(c, None)?, name_in_use(c, &name)?)))?;
        if name_used {
            blockers.insert("name_in_use");
        }
        let env = self.0.env.clone();
        let port = resources::plan_port_with(&held, request.game_port, |p| env.port_state(p));
        if let Some(blocker) = port.blocker {
            blockers.insert(blocker);
        }
        let memory =
            resources::plan_memory(self.0.env.memory_total_bytes(), &held, request.memory_mib);
        if let Some(blocker) = memory.blocker {
            blockers.insert(blocker);
        }
        let free = self.0.env.free_bytes(&self.0.servers);
        let needed = candidate
            .as_ref()
            .map_or(0, |c| c.private.bytes)
            .saturating_add(if java.download_required || version.is_none() {
                JAVA_RESERVE_BYTES
            } else {
                0
            })
            .saturating_add(SPACE_MARGIN_BYTES);
        if free.is_some_and(|free| free < needed) {
            blockers.insert("insufficient_space");
        }

        let server_id = Uuid::new_v4();
        let now = super::super::browse::now_ms();
        let software = candidate.as_ref().and_then(|c| c.public.software.clone());
        let mut review = CreateReview {
            plan_id: Uuid::new_v4().to_string(),
            confirmation: None,
            expires_at: now.saturating_add(PLAN_TTL_MS),
            summary: CreateSummary {
                kind: "restore".into(),
                name,
                minecraft_version: version.clone().unwrap_or_default(),
                software: SummarySoftware {
                    kind: software
                        .as_ref()
                        .map_or_else(|| "unknown".into(), |s| s.kind.clone()),
                    version: software.as_ref().and_then(|s| s.version.clone()),
                    build: None,
                    loader_version: None,
                },
                resources: ReviewResources {
                    game_port: port.game_port,
                    memory_mib: memory.memory_mib,
                },
                runtime: CreateRuntime {
                    required_major: java.required_major,
                    download_required: java.download_required,
                    automatic: true,
                },
                destination: ReviewDestination {
                    server_id,
                    display_path: display(&self.server_dir(server_id).join("game")),
                    free_bytes: free,
                },
                pack: None,
                restore: Some(serde_json::json!({
                    "sourceId": source_id,
                    "label": candidate.as_ref().map(|c| &c.public.label),
                    "createdAt": candidate.as_ref().map(|c| &c.public.created_at),
                })),
            },
            blockers: blockers.iter().map(|b| (*b).to_owned()).collect(),
            warnings: warnings.iter().map(|w| (*w).to_owned()).collect(),
        };
        if review.blockers.is_empty() {
            review.confirmation = Some(random_token()?);
        }
        let restore = candidate.map_or_else(
            || RestorePrivate {
                origin: Uuid::nil(),
                repository: PathBuf::new(),
                snapshot: String::new(),
                bytes: 0,
                files: 0,
            },
            |c| c.private,
        );
        let private = CreatePrivate::sealed_restore(
            binding,
            &review,
            ReviewRuntime {
                required_major: java.required_major,
                selected_runtime_id: java.selected_runtime_id.clone(),
                download_required: java.download_required,
                verified: java.verified,
                jdk: false,
            },
            restore,
        )?;
        Ok((review, private))
    }

    /// Restore the reviewed snapshot into a fresh, never-registered
    /// destination and freeze its trusted launch. Nothing is published until
    /// every restored byte is verified.
    pub(super) fn restore_install(
        &self,
        job_id: &str,
        review: &CreateReview,
        private: &CreatePrivate,
        eula: (i64, &str),
        cancel: &AtomicBool,
    ) -> Result<(), Outcome> {
        let summary = &review.summary;
        let server_id = summary.destination.server_id;
        let port = summary
            .resources
            .game_port
            .ok_or(Outcome::Failed("plan_blocked"))?;
        let memory = summary
            .resources
            .memory_mib
            .ok_or(Outcome::Failed("plan_blocked"))?;
        let restore = private
            .restore
            .as_ref()
            .ok_or(Outcome::Failed("plan_blocked"))?;
        let stop = || cancel.load(Ordering::Acquire) || self.cancel_requested(job_id);
        self.set_phase(job_id, "applying", "resolving")?;
        self.progress(job_id, CopyProgress::default(), 0, 0)?;
        if self
            .0
            .host
            .server(server_id)
            .map_err(|_| Outcome::Interrupted("state_unavailable"))?
            .is_some()
        {
            return Err(Outcome::Failed("destination_conflict"));
        }
        // Anything an interrupted attempt left is ours and is discarded.
        self.remove_unregistered(server_id);
        // The opaque source is revalidated: still exactly the reviewed
        // snapshot, still not attached to a live server.
        let candidates = self
            .restore_candidates()
            .map_err(|_| Outcome::Interrupted("state_unavailable"))?;
        if !candidates.iter().any(|c| &c.private == restore) {
            return Err(Outcome::Failed("restore_source_unavailable"));
        }
        let dir = self.server_dir(server_id);
        files::create_local_directory(&dir, true)
            .and_then(|()| files::create_local_directory(&dir.join("private"), true))
            .map_err(|_| Outcome::Failed("install_failed"))?;
        if stop() {
            return Err(Outcome::Cancelled);
        }

        self.set_phase(job_id, "applying", "installing")?;
        self.progress(job_id, CopyProgress::default(), 0, restore.bytes)?;
        let staging = dir.join("staging");
        let _ = fs::remove_dir_all(&staging);
        files::create_local_directory(&staging, true)
            .map_err(|_| Outcome::Failed("install_failed"))?;
        let engine = BackupEngine::new(Box::new(LocalStore::new(restore.repository.clone())));
        let mut last: Option<std::time::Instant> = None;
        let restored =
            engine.restore_cancellable(&restore.snapshot, &staging, &stop, &mut |done, total| {
                let now = std::time::Instant::now();
                if done < total && last.is_some_and(|l| now.duration_since(l) < PROGRESS_INTERVAL) {
                    return;
                }
                last = Some(now);
                let _ = self.progress(
                    job_id,
                    CopyProgress {
                        files: 0,
                        bytes: done,
                    },
                    0,
                    total,
                );
            });
        match restored {
            Ok(true) => {}
            Ok(false) => return Err(Outcome::Cancelled),
            Err(
                crate::backups::engine::EngineError::Corrupt
                | crate::backups::engine::EngineError::Missing
                | crate::backups::engine::EngineError::Failed
                | crate::backups::engine::EngineError::Invalid
                | crate::backups::engine::EngineError::TooLarge,
            ) => return Err(Outcome::Failed("integrity_conflict")),
            Err(_) => return Err(Outcome::Failed("install_failed")),
        }

        // The trusted launch comes from the restored files, exactly as for an
        // import: the detected jar or Java argument file, never a script.
        let mut ignored = (BTreeSet::new(), BTreeSet::new());
        let report = inspection::inspect_path(&staging, &mut ignored.0, &mut ignored.1);
        let launch = report
            .detected
            .launch
            .as_ref()
            .map(|l| LaunchChoice {
                kind: l.kind.into(),
                relative_path: l.relative_path.clone(),
            })
            .ok_or(Outcome::Failed("launch_unresolved"))?;
        let relative = RelativePath::parse(&launch.relative_path)
            .map_err(|_| Outcome::Failed("launch_unresolved"))?;
        let mut launch_path = staging.clone();
        launch_path.extend(relative.as_str().split('/'));
        let (size, sha256) = {
            let meta = fs::symlink_metadata(&launch_path)
                .map_err(|_| Outcome::Failed("launch_unresolved"))?;
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err(Outcome::Failed("launch_unresolved"));
            }
            let mut hasher = Sha256::new();
            std::io::copy(
                &mut fs::File::open(&launch_path).map_err(|_| Outcome::Failed("install_failed"))?,
                &mut hasher,
            )
            .map_err(|_| Outcome::Failed("install_failed"))?;
            (meta.len(), hex::encode(hasher.finalize()))
        };
        // Java for the restored server's own Minecraft version: the reviewed
        // runtime when the restored files agree, else planned from them.
        let detected = report.detected.minecraft_version.clone();
        let reviewed = Some(summary.minecraft_version.clone()).filter(|v| !v.is_empty());
        let runtime = match (&detected, &reviewed) {
            (Some(found), Some(known)) if found == known => private.java().clone(),
            (None, Some(_)) => private.java().clone(),
            (found, _) => {
                let plan = self
                    .0
                    .env
                    .plan_java(found.as_deref(), std::slice::from_ref(&self.0.servers));
                if let Some(blocker) = plan.blocker {
                    return Err(Outcome::Failed(blocker));
                }
                ReviewRuntime {
                    required_major: plan.required_major,
                    selected_runtime_id: plan.selected_runtime_id,
                    download_required: plan.download_required,
                    verified: plan.verified,
                    jdk: false,
                }
            }
        };
        self.set_phase(job_id, "applying", "downloading_java")?;
        let java = self
            .0
            .env
            .ensure_java(&runtime, std::slice::from_ref(&self.0.servers))
            .map_err(Outcome::Failed)?;
        if stop() {
            return Err(Outcome::Cancelled);
        }
        // EULA consent comes from the owner's apply, never from the backup.
        let (ms, account) = eula;
        commit::write_atomic(
            &staging.join("eula.txt"),
            format!(
                "# Accepted by Vesta Server owner {account} at {ms} (https://aka.ms/MinecraftEULA)\neula=true\n"
            )
            .as_bytes(),
        )
        .map_err(|_| Outcome::Failed("install_failed"))?;
        let properties = staging.join("server.properties");
        let current = match fs::symlink_metadata(&properties) {
            Ok(meta)
                if meta.is_file()
                    && !meta.file_type().is_symlink()
                    && meta.len() <= 1024 * 1024 =>
            {
                fs::read_to_string(&properties).map_err(|_| Outcome::Failed("install_failed"))?
            }
            Ok(_) => return Err(Outcome::Failed("install_failed")),
            Err(_) => String::new(),
        };
        commit::write_atomic(
            &properties,
            commit::rewrite_properties(&current, port).as_bytes(),
        )
        .map_err(|_| Outcome::Failed("install_failed"))?;
        // Last safe boundary before anything is published.
        if stop() {
            return Err(Outcome::Cancelled);
        }
        let game = dir.join("game");
        if fs::symlink_metadata(&game).is_ok() {
            return Err(Outcome::Failed("destination_conflict"));
        }
        fs::rename(&staging, &game).map_err(|_| Outcome::Failed("install_failed"))?;

        let entry = crate::files::copied_export::CopyEntry {
            path: relative.as_str().to_owned(),
            directory: false,
            size,
            sha256,
            identity: String::new(),
        };
        let (managed, jvm_args) = commit::trusted_launch(&game, &launch, &java, memory, &entry)
            .map_err(|e| {
                Outcome::Failed(if e.code() == "launch_unresolved" {
                    "launch_unresolved"
                } else {
                    "install_failed"
                })
            })?;
        let document = crate::host_software::managed_launch_document(&managed, &jvm_args, &dir)
            .map_err(|_| Outcome::Failed("install_failed"))?;
        commit::write_atomic(&dir.join("private/launch.toml"), document.as_bytes())
            .map_err(|_| Outcome::Failed("install_failed"))?;
        let minecraft = report
            .detected
            .minecraft_version
            .clone()
            .unwrap_or_else(|| summary.minecraft_version.clone());
        let scan = if game.join("config").is_dir() {
            "game/config"
        } else {
            "game"
        };
        let value = serde_json::json!({
            "schema-version": 1,
            "paths": {"state": "private/state", "game": "game", "scan-roots": [scan], "launch-definition": "private/launch.toml"},
            "pack": {"name": summary.name, "version": "initial", "revision": 1, "minimum-revision": 1,
                     "minecraft-version": minecraft},
            "pack-access": {"mode": "minecraft"},
            "listener": {"mode": "disabled"},
        });
        let host_toml = crate::onboarding::provisioning::host_document_toml(value, &dir)
            .map_err(|_| Outcome::Failed("install_failed"))?;
        commit::write_atomic(&dir.join("host.toml"), host_toml.as_bytes())
            .map_err(|_| Outcome::Failed("install_failed"))?;
        if stop() {
            return Err(Outcome::Cancelled);
        }
        Ok(())
    }
}
