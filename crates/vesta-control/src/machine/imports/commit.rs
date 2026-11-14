//! Import commit and start (H17). After a verified copy: record EULA consent
//! and resource choices in destination config, freeze a trusted launch spec
//! built only from detected facts, register the server exactly once, create its
//! in-process worker through the H05 entry point, re-check conflicts and start
//! through the authorized operations runtime. Nothing from the source folder is
//! executed or trusted as a script; AMP and other processes are never signalled.
use super::*;
use crate::{
    config::HostConfiguration,
    identity::{BuiltinRole, DeviceId, PrincipalId, Subject},
    machine::{
        owners::Actor,
        registry::{ServerEntry, ServerState, Visibility},
    },
    operations::GameCommand,
    worker::{
        ServerRole, ServerSubject, ServerWorker, Worker, WorkerGrant, WorkerHandle, WorkerPolicy,
    },
};
use vesta_game_runtime::readiness::Readiness;
use vesta_server_install::{ManagedArtifact, ManagedLaunch};
use vesta_state::Clock;

const MAX_PROPERTIES: u64 = 1024 * 1024;
/// Time a queued start may take to spawn before `Stopped` means it failed.
const START_GRACE_MS: i64 = 30_000;

/// Launch built from detected facts: the trusted Java runtime, the detected
/// jar or Java argument file inside the managed copy, `nogui` and a heap cap.
/// Source scripts, `user_jvm_args.txt` and other source-supplied arguments are
/// never used. A Forge/NeoForge argument file is read by Java itself as plain
/// arguments; it is not executed by a shell.
pub(crate) fn trusted_launch(
    game: &Path,
    launch: &LaunchChoice,
    java: &Path,
    memory_mib: u64,
    entry: &crate::files::copied_export::CopyEntry,
) -> JobResult<(ManagedLaunch, Vec<String>)> {
    let relative = RelativePath::parse(&launch.relative_path).map_err(|_| JobError::INVALID)?;
    if entry.directory || entry.path != relative.as_str() {
        return Err(JobError::INVALID);
    }
    let game = &plain_drive_path(game);
    let mut path = game.to_path_buf();
    path.extend(relative.as_str().split('/'));
    let text = path.to_str().ok_or(JobError::INVALID)?.to_owned();
    let args = match launch.kind.as_str() {
        "jar" => vec!["-jar".into(), text, "nogui".into()],
        "java_argfile" => {
            let mut args = super::super::arguments::parse(game, &path).map_err(JobError)?;
            args.push("nogui".into());
            args
        }
        _ => return Err(JobError("launch_unresolved")),
    };
    Ok((
        ManagedLaunch {
            java: java.to_path_buf(),
            working_directory: game.to_path_buf(),
            args,
            required_files: vec![path.clone()],
            artifacts: vec![ManagedArtifact {
                path,
                size: entry.size,
                sha256: entry.sha256.clone(),
            }],
        },
        vec![format!("-Xmx{memory_mib}M")],
    ))
}

/// Java resolves `-jar`/`@argfile` paths through its own class loader, which
/// cannot open Windows verbatim (`\\?\C:\...`) spellings that canonicalization
/// produces. Plain drive paths drop the prefix; UNC and other forms are kept.
pub(crate) fn plain_drive_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest)
            if cfg!(windows)
                && rest.len() >= 3
                && rest.as_bytes()[0].is_ascii_alphabetic()
                && &rest.as_bytes()[1..3] == b":\\" =>
        {
            PathBuf::from(rest)
        }
        _ => path.to_path_buf(),
    }
}

/// `server-port` set to the reserved port and RCON disabled; every other line
/// kept verbatim. Vanilla and its common forks bind RCON to `server-ip`, the
/// game address, so it cannot be confined to loopback without also hiding the
/// game; the managed console uses the process console instead. `rcon.password`
/// is left in the host-only file and is never returned by the API (the file is
/// protected and the properties view redacts it).
pub(crate) fn rewrite_properties(text: &str, port: u16) -> String {
    super::super::properties::rewrite(text, port)
}

pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> JobResult<()> {
    let leaf = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(JobError::INVALID)?;
    let temp = path.with_file_name(format!(".{leaf}.vesta-new"));
    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        std::io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|_| JobError("configure_failed"))
}

/// Complete effective snapshot for a new server: every host owner's active
/// devices with the host-owner expansion. No per-server grants exist yet.
pub fn owner_policy(host: &HostState, server_id: Uuid) -> JobResult<WorkerPolicy> {
    let capabilities: Vec<String> = BuiltinRole::Owner
        .capabilities()
        .iter()
        .filter(|c| !c.local_only())
        .map(|c| c.id().to_owned())
        .collect();
    let mut out = None;
    host.read(|c| {
        let snapshot = super::super::super::state::snapshot_in(c)?;
        let mut stmt = c.prepare(
            "SELECT d.account_id,d.device_id FROM machine_devices d JOIN machine_accounts a ON a.account_id=d.account_id WHERE d.revoked_at_ms IS NULL AND a.disabled=0 AND (d.account_id IN (SELECT account_id FROM machine_owners) OR d.account_id=(SELECT primary_account_id FROM machine_host WHERE singleton=1)) ORDER BY d.device_id",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut grants = Vec::new();
        for (account, device) in rows {
            if let (Ok(account_id), Ok(device_id)) =
                (Uuid::parse_str(&account), Uuid::parse_str(&device))
            {
                grants.push(WorkerGrant {
                    account_id,
                    device_id,
                    role: ServerRole::Owner,
                    capabilities: capabilities.clone(),
                });
            }
        }
        out = Some(WorkerPolicy {
            host_id: snapshot.host_id,
            server_id,
            generation: snapshot.auth_generation,
            grants,
        });
        Ok(())
    })?;
    out.ok_or(JobError::UNAVAILABLE)
}

impl ImportJobs {
    /// Configure the verified copy and register it once. Idempotent on retry.
    pub(super) fn commit(&self, job_id: &str, review: &Review) -> JobResult<()> {
        let (private, eula_ms, eula_account, name): (Private, i64, String, String) =
            self.read(|c| {
                let (private, ms, account, request): (Option<String>, Option<i64>, Option<String>, String) =
                    c.query_row(
                        "SELECT private,eula_accepted_ms,eula_account,request FROM machine_import_jobs WHERE job_id=?1",
                        [job_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )?;
                let request: PlanRequest =
                    serde_json::from_str(&request).map_err(|_| JobError::UNAVAILABLE)?;
                Ok((
                    decode(private)?,
                    ms.ok_or(JobError::EULA)?,
                    account.ok_or(JobError::EULA)?,
                    request.name.trim().to_owned(),
                ))
            })?;
        let server_id = review.destination.server_id;
        let dir = self.server_dir(server_id);
        let game = dir.join("game");
        let port = review.resources.game_port.ok_or(JobError::BLOCKED)?;
        let memory = review.resources.memory_mib.ok_or(JobError::BLOCKED)?;
        let launch = review
            .detected
            .launch
            .as_ref()
            .ok_or(JobError("launch_unresolved"))?;
        let snapshot = private.snapshot.as_ref().ok_or(JobError::BLOCKED)?;
        let entry = snapshot
            .entries
            .iter()
            .find(|e| e.path == launch.relative_path)
            .ok_or(JobError("launch_unresolved"))?;

        self.set_phase(job_id, "applying", "configure")?;
        // EULA consent comes from the owner's apply, never from the source.
        write_atomic(
            &game.join("eula.txt"),
            format!(
                "# Accepted by Vesta Server owner {eula_account} at {eula_ms} (https://aka.ms/MinecraftEULA)\neula=true\n"
            )
            .as_bytes(),
        )?;
        let properties = game.join("server.properties");
        let current = match fs::symlink_metadata(&properties) {
            Ok(meta) if meta.is_file() && meta.len() <= MAX_PROPERTIES => {
                fs::read_to_string(&properties).map_err(|_| JobError("configure_failed"))?
            }
            Ok(_) => return Err(JobError("unsafe_path")),
            Err(_) => String::new(),
        };
        write_atomic(&properties, rewrite_properties(&current, port).as_bytes())?;

        let java = self
            .0
            .env
            .ensure_java(
                &review.runtime,
                &[private.source.clone(), self.0.servers.clone()],
            )
            .map_err(JobError)?;
        let (managed, jvm_args) = trusted_launch(&game, launch, &java, memory, entry)?;
        files::create_local_directory(&dir.join("private"), true)
            .map_err(|_| JobError("configure_failed"))?;
        let document = crate::host_software::managed_launch_document(&managed, &jvm_args, &dir)
            .map_err(|_| JobError("launch_unresolved"))?;
        write_atomic(&dir.join("private/launch.toml"), document.as_bytes())?;
        let scan = if game.join("config").is_dir() {
            "game/config"
        } else {
            "game"
        };
        let value = serde_json::json!({
            "schema-version": 1,
            "paths": {"state": "private/state", "game": "game", "scan-roots": [scan], "launch-definition": "private/launch.toml"},
            "pack": {"name": name, "version": "initial", "revision": 1, "minimum-revision": 1,
                     "minecraft-version": review.detected.minecraft_version.clone().unwrap_or_default()},
            "pack-access": {"mode": "minecraft"},
            "listener": {"mode": "disabled"},
        });
        let host_toml = crate::onboarding::provisioning::host_document_toml(value, &dir)
            .map_err(|_| JobError("configure_failed"))?;
        write_atomic(&dir.join("host.toml"), host_toml.as_bytes())?;

        self.set_phase(job_id, "applying", "register")?;
        self.register_once(job_id, server_id, name, port, &eula_account)?;
        let now = self.0.host.now_ms();
        self.write(|c| {
            c.execute(
                "UPDATE machine_import_jobs SET state='ready',phase='register',error=NULL,updated_ms=?1 WHERE job_id=?2",
                params![now, job_id],
            )?;
            audit(c, "import.registered", &eula_account, job_id, now)?;
            Ok(())
        })
    }

    /// Register the committed server exactly once under its reserved id and
    /// port; a retry after a crash finds the existing entry and stops.
    pub(super) fn register_once(
        &self,
        job_id: &str,
        server_id: Uuid,
        name: String,
        port: u16,
        account: &str,
    ) -> JobResult<()> {
        let account = Uuid::parse_str(account).map_err(|_| JobError::UNAVAILABLE)?;
        let entry = ServerEntry {
            server_id,
            name,
            game_port: port,
            state: ServerState::Registered,
            visibility: Visibility::Private,
        };
        for _ in 0..4 {
            if self.0.host.server(server_id)?.is_some() {
                break;
            }
            let revision = self.0.host.snapshot()?.revision;
            match self
                .0
                .host
                .register_server(Actor::Account(account), &entry, revision)
            {
                Ok(_) => break,
                Err(crate::ControlError::Rejected {
                    code: crate::IdentityErrorCode::Conflict,
                    ..
                }) if self.0.host.server(server_id)?.is_none() => {
                    // Revision race or capacity; retry once the revision settles.
                    if self
                        .read(|c| reservations_in(c, Some(job_id)))?
                        .ports
                        .contains(&port)
                    {
                        return Err(JobError("game_port_in_use"));
                    }
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
        }
        if self.0.host.server(server_id)?.is_none() {
            return Err(JobError::CAPACITY);
        }
        Ok(())
    }

    /// Build the registered server's worker through the H05 composition entry
    /// point. The caller (host lifecycle) owns the returned worker.
    pub fn create_worker(&self, server_id: Uuid, clock: Arc<dyn Clock>) -> JobResult<Worker> {
        let entry = self.0.host.server(server_id)?.ok_or(JobError::NOT_FOUND)?;
        let dir = self.server_dir(server_id);
        let text = fs::read(dir.join("host.toml")).map_err(|_| JobError::NOT_FOUND)?;
        let config = HostConfiguration::parse_toml(&text, &dir, Path::new("host.toml"))
            .map_err(|_| JobError("configure_failed"))?;
        let host_id = self.0.host.snapshot()?.host_id;
        Worker::from_registry(Some(&entry), config, clock, None, host_id)
            .map_err(|_| JobError("worker_unavailable"))?
            .ok_or(JobError::NOT_FOUND)
    }

    pub(super) fn subject(
        &self,
        binding: &OwnerBinding,
        server_id: Uuid,
        capabilities: Vec<String>,
    ) -> JobResult<(ServerSubject, Subject)> {
        let snapshot = self.0.host.snapshot()?;
        let now = self.0.host.now_ms();
        let account = Uuid::parse_str(&binding.account_id).map_err(|_| JobError::FORBIDDEN)?;
        let device = Uuid::parse_str(&binding.device_id).map_err(|_| JobError::FORBIDDEN)?;
        Ok((
            ServerSubject {
                host_id: snapshot.host_id,
                server_id,
                account_id: account,
                device_id: device,
                generation: snapshot.auth_generation,
                issued_at_ms: now,
                expires_at_ms: now + 30_000,
                capabilities,
            },
            Subject {
                principal: PrincipalId::parse(&binding.account_id)
                    .map_err(|_| JobError::FORBIDDEN)?,
                device: DeviceId::parse(&binding.device_id).map_err(|_| JobError::FORBIDDEN)?,
            },
        ))
    }

    /// Re-check conflicts and start the committed server once. A conflict or
    /// bind failure leaves the copy and registration in place with a
    /// resolvable error; the retry reuses the same operation idempotency key.
    pub fn start(
        &self,
        binding: &OwnerBinding,
        job_id: &str,
        worker: &WorkerHandle,
    ) -> JobResult<ImportJob> {
        self.start_kind(binding, job_id, worker, Kind::Import)?;
        self.status(binding, job_id)
    }

    /// The committed server's id and reserved game port, from the durable
    /// job row and its reservation (the same for imports and creates).
    fn committed(&self, c: &Connection, job_id: &str, kind: Kind) -> JobResult<Committed> {
        let row: Option<(String, Option<String>, Option<String>, i64, Option<u16>)> = c
            .query_row(
                "SELECT j.state,j.server_id,j.private,j.start_attempt,r.game_port FROM machine_import_jobs j
                 LEFT JOIN machine_import_reservations r ON r.job_id=j.job_id AND r.released=0
                 WHERE j.job_id=?1 AND j.kind=?2",
                params![job_id, kind.id()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let (state, server, private, attempt, port) = row.ok_or(kind.not_found())?;
        Ok(Committed {
            state,
            server_id: server.and_then(|s| Uuid::parse_str(&s).ok()),
            private,
            attempt,
            port,
        })
    }

    pub(crate) fn start_kind(
        &self,
        binding: &OwnerBinding,
        job_id: &str,
        worker: &WorkerHandle,
        kind: Kind,
    ) -> JobResult<()> {
        let job = self.read(|c| {
            self.owner(c, binding)?;
            self.committed(c, job_id, kind)
        })?;
        let (state, attempt) = (job.state.as_str(), job.attempt);
        if !matches!(state, "ready" | "starting" | "online") {
            return Err(JobError::CONFLICT);
        }
        let server_id = job.server_id.ok_or(JobError::CONFLICT)?;
        if worker.server_id() != server_id || self.0.host.server(server_id)?.is_none() {
            return Err(JobError::CONFLICT);
        }
        if state == "ready" {
            let port = job.port.ok_or(JobError::BLOCKED)?;
            // An imported source must still be stopped; a created server has none.
            let source = match kind {
                Kind::Import => Some(decode::<Private>(job.private)?.source),
                Kind::Create => None,
            };
            let managed = self.managed_ports()?;
            let source_conflict = source.and_then(|source| {
                let (processes, _observed) = self.0.env.processes(std::slice::from_ref(&source));
                if processes
                    .iter()
                    .any(|p| p.relation == "source" && p.state == "running")
                {
                    Some("source_running")
                } else {
                    inspection::stopped_evidence(
                        &source,
                        inspection::read_properties(&source.join("server.properties"))
                            .ok()
                            .and_then(|p| p.get("server-port").and_then(|v| v.parse().ok()))
                            .or(Some(25565)),
                        &managed,
                    )
                }
            });
            let conflict = source_conflict.or(match self.0.env.port_state(port) {
                "free" => None,
                "occupied" => Some("game_port_in_use"),
                _ => Some("port_state_unknown"),
            });
            if let Some(code) = conflict {
                return self.resolvable(job_id, "ready", code);
            }
        }
        let policy = owner_policy(&self.0.host, server_id)?;
        // Same generation and document is accepted again; a newer host sync wins.
        let _ = worker.sync_policy(policy);
        let (server_subject, subject) = self.subject(
            binding,
            server_id,
            vec![crate::identity::Capability::LifecycleStart.id().to_owned()],
        )?;
        let network = worker
            .authorize(server_subject)
            .map_err(|_| JobError::FORBIDDEN)?;
        network
            .runtime()
            .submit_game(
                &subject,
                GameCommand::Start,
                &format!("{}-start-{job_id}-{attempt}", kind.id()),
            )
            .map_err(|_| JobError("start_failed"))?;
        if state == "ready" {
            let now = self.0.host.now_ms();
            self.write(|c| {
                c.execute(
                    "UPDATE machine_import_jobs SET state='starting',phase=CASE kind WHEN 'create' THEN 'starting' ELSE 'start' END,error=NULL,updated_ms=?1 WHERE job_id=?2",
                    params![now, job_id],
                )?;
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Observe readiness: `online` only after the runtime reports the game
    /// ready, never on spawn alone. A failed start keeps the copy.
    pub fn readiness(
        &self,
        binding: &OwnerBinding,
        job_id: &str,
        worker: &WorkerHandle,
    ) -> JobResult<ImportJob> {
        self.readiness_kind(binding, job_id, worker, Kind::Import)?;
        self.status(binding, job_id)
    }

    pub(crate) fn readiness_kind(
        &self,
        binding: &OwnerBinding,
        job_id: &str,
        worker: &WorkerHandle,
        kind: Kind,
    ) -> JobResult<()> {
        let (job, since) = self.read(|c| {
            self.owner(c, binding)?;
            let job = self.committed(c, job_id, kind)?;
            let since: i64 = c.query_row(
                "SELECT updated_ms FROM machine_import_jobs WHERE job_id=?1",
                [job_id],
                |r| r.get(0),
            )?;
            Ok((job, since))
        })?;
        if job.state != "starting" {
            return Ok(());
        }
        let server_id = job.server_id.ok_or(JobError::UNAVAILABLE)?;
        if worker.server_id() != server_id {
            return Err(JobError::CONFLICT);
        }
        let (server_subject, _) = self.subject(binding, server_id, Vec::new())?;
        let network = worker
            .authorize(server_subject)
            .map_err(|_| JobError::FORBIDDEN)?;
        let view = network
            .runtime()
            .readiness()
            .map_err(|_| JobError::UNAVAILABLE)?;
        // The start operation is queued: until it spawns the game the runtime
        // still reports `Stopped`. Only a stop observed after this grace window
        // is a failed start. While the game runs it owns its own port, so a
        // busy port is only meaningful once it has stopped.
        let settled = self.0.host.now_ms().saturating_sub(since) >= START_GRACE_MS;
        match view.readiness {
            Readiness::Ready => self.resolvable(job_id, "online", "")?,
            Readiness::Starting => {}
            Readiness::Stopped if settled => {
                let port = job.port.unwrap_or(0);
                let code = if self.0.env.port_state(port) == "occupied" {
                    "game_port_in_use"
                } else {
                    "start_failed"
                };
                self.resolvable(job_id, "ready", code)?;
            }
            Readiness::Stopped => {}
            Readiness::Degraded(
                vesta_game_runtime::readiness::DegradedReason::StartupTimeout
                | vesta_game_runtime::readiness::DegradedReason::FatalOutput,
            ) => self.resolvable(job_id, "ready", "start_failed")?,
            // A failed status probe or console gap may recover; keep waiting.
            Readiness::Degraded(_) => {}
        }
        Ok(())
    }

    /// Preserve an installed server when its worker cannot start.
    pub fn installed_start_failed(&self, job_id: &str) -> JobResult<()> {
        self.resolvable(job_id, "ready", "start_failed")
    }

    fn resolvable(&self, job_id: &str, state: &str, code: &str) -> JobResult<()> {
        let now = self.0.host.now_ms();
        let error = (!code.is_empty()).then_some(code);
        self.write(|c| {
            c.execute(
                "UPDATE machine_import_jobs SET state=?1,phase=CASE kind WHEN 'create' THEN 'starting' ELSE 'start' END,error=?2,updated_ms=?3,start_attempt=start_attempt+(?2 IS NOT NULL) WHERE job_id=?4",
                params![state, error, now, job_id],
            )?;
            Ok(())
        })
    }
}

/// Durable facts a start needs, whichever flow committed the server.
struct Committed {
    state: String,
    server_id: Option<Uuid>,
    private: Option<String>,
    attempt: i64,
    port: Option<u16>,
}
