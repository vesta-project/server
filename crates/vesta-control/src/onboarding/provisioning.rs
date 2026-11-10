//! First-server provisioning in an installation explicitly selected by local setup.
//! HTTP requests select software, never host paths, executables or shell commands.
use super::{FirstServerSetup, Result};
use crate::{
    files::{
        self, FileBrowser, RelativePath,
        copied_export::{CopiedExport, CopySnapshot},
    },
    identity::{self, BuiltinRole, Subject, management::permissions::PermissionError as Error},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
use vesta_server_install::{
    Acknowledgments, Approval, InstallPlan, InstallRoots, ManagedArtifact, ManagedLaunch,
    ProgressReporter, Selection, ServerInstaller, Software, TransportConfig,
};

/// OS-local input. It is deliberately not accepted by any pending-host route.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalConfiguration {
    pub java: PathBuf,
    pub adoption_jar: Option<String>,
    #[serde(default)]
    pub adoption_argfile: Option<String>,
    #[serde(default)]
    pub copied_export_confirmed: bool,
}
impl LocalConfiguration {
    fn entrypoint(&self) -> Option<&str> {
        match (&self.adoption_jar, &self.adoption_argfile) {
            (Some(p), None) | (None, Some(p)) => Some(p),
            _ => None,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Install {
        name: String,
        software: Software,
        version: String,
        build: Option<u32>,
        loader_version: Option<String>,
        #[serde(default)]
        experimental: bool,
    },
    AdoptCopy {
        name: String,
        minecraft_version: String,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Apply {
    pub job_id: String,
    pub confirmation: String,
    pub idempotency_key: String,
    pub acknowledge_warnings: bool,
    pub acknowledge_legacy_checksum: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub name: String,
    pub minecraft_version: String,
    pub software: Option<Software>,
    pub loader_version: Option<String>,
    pub files: usize,
    pub bytes: u64,
    pub warnings: Vec<String>,
}
#[derive(Clone, Serialize)]
pub struct Job {
    pub id: String,
    pub state: String,
    pub phase: String,
    pub error: Option<String>,
    pub review: Option<Review>,
    pub confirmation: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Actor {
    principal: String,
    device: String,
    account: String,
}
enum Prepared {
    Install(InstallPlan),
    Copy(CopySnapshot),
}
#[derive(Serialize, Deserialize)]
struct Completion {
    review: Review,
    launch: String,
    artifacts: Vec<ManagedArtifact>,
    copy: Option<CopySnapshot>,
}
struct Inner {
    setup: FirstServerSetup,
    root: PathBuf,
    local: LocalConfiguration,
    source: Option<CopiedExport>,
    installer: Arc<ServerInstaller>,
    port: u16,
    plans: Mutex<HashMap<String, Prepared>>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    stop: AtomicBool,
    _owner_lock: std::fs::File,
}
#[derive(Clone)]
pub struct Provisioning(Arc<Inner>);

fn unavailable<T>(_: T) -> Error {
    Error::Unavailable
}
fn encoded<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(unavailable)
}
fn decoded<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(unavailable)
}
fn text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}
const INITIAL_EULA: &[u8] =
    b"# Read https://www.minecraft.net/eula before accepting.\neula=false\n";

impl Provisioning {
    /// Must be called once by the installation's pending-host owner. The canonical
    /// DB location prevents composing a foreign installation into this listener.
    pub fn open(
        setup: FirstServerSetup,
        root: &Path,
        local: LocalConfiguration,
        copied_source: Option<&Path>,
        port: u16,
    ) -> Result<Self> {
        if port == 0 {
            return Err(Error::InvalidRequest);
        }
        vesta_state::StateDirectory::new(root, None).map_err(unavailable)?;
        let root = std::fs::canonicalize(root).map_err(unavailable)?;
        if !files::same_directory(setup.db.directory().path(), &root.join("private/state"))
            .map_err(unavailable)?
        {
            return Err(Error::Conflict);
        }
        let owner_lock = setup
            .db
            .directory()
            .artifacts()
            .and_then(|d| {
                d.try_lock_exclusive(&vesta_state::artifacts::ArtifactName::new(
                    "setup-provisioning.lock",
                )?)
            })
            .map_err(|_| Error::Conflict)?;
        // Observe the locally selected runtime safely; no probing/execution until
        // the explicitly selected software plan calls the existing installer.
        files::read_local_document(&local.java, 1)
            .or_else(|e| {
                if e == files::FileError::TooLarge {
                    Ok(Vec::new())
                } else {
                    Err(e)
                }
            })
            .map_err(unavailable)?;
        if local.adoption_jar.is_some() && local.adoption_argfile.is_some() {
            return Err(Error::InvalidRequest);
        }
        if let Some(entrypoint) = local.entrypoint() {
            RelativePath::parse(entrypoint).map_err(|_| Error::InvalidRequest)?;
        }
        if copied_source.is_some()
            && (!local.copied_export_confirmed || local.entrypoint().is_none())
        {
            return Err(Error::PreconditionFailed);
        }
        let software = root.join("software");
        files::create_local_directory(&software, true).map_err(unavailable)?;
        let roots = InstallRoots::new(&software, &[setup.db.directory().path().to_owned()])
            .map_err(unavailable)?;
        let installer =
            Arc::new(ServerInstaller::new(roots, TransportConfig::direct()).map_err(unavailable)?);
        let source = copied_source
            .map(|s| CopiedExport::open(s, &[&root]))
            .transpose()
            .map_err(unavailable)?;
        setup.db.write(|c|{c.execute_batch("CREATE TABLE IF NOT EXISTS setup_provisioning(id TEXT PRIMARY KEY,request TEXT NOT NULL,actor TEXT NOT NULL,state TEXT NOT NULL,phase TEXT NOT NULL,error TEXT,review TEXT,confirmation TEXT,apply_key TEXT,apply_hash TEXT,completion TEXT,created_ms INTEGER NOT NULL) STRICT;")?;c.execute("UPDATE setup_provisioning SET state='interrupted',phase='interrupted',error='setup_interrupted_replan_required' WHERE state IN ('planning','applying','review')",[])?;Ok(())}).map_err(unavailable)?;
        Ok(Self(Arc::new(Inner {
            setup,
            root,
            local,
            source,
            installer,
            port,
            plans: Mutex::new(HashMap::new()),
            worker: Mutex::new(None),
            stop: AtomicBool::new(false),
            _owner_lock: owner_lock,
        })))
    }
    pub fn status(&self, token: &str, id: &str) -> Result<Job> {
        self.0.setup.with_owner(token, |c, _, _| job(c, id))
    }
    pub fn prepare(&self, token: &str, request: Request) -> Result<Job> {
        let mut worker = self.worker_slot()?;
        let request_json = encoded(&request)?;
        match &request {
            Request::Install { name, version, .. } if text(name) && text(version) => (),
            Request::AdoptCopy {
                name,
                minecraft_version,
            } if text(name)
                && text(minecraft_version)
                && self.0.source.is_some()
                && self.0.local.entrypoint().is_some() =>
            {
                ()
            }
            _ => return Err(Error::InvalidRequest),
        }
        let id = uuid::Uuid::new_v4().to_string();
        let actor=self.0.setup.with_owner(token,|c,s,now|{
            let (active,total):(bool,i64)=c.query_row("SELECT EXISTS(SELECT 1 FROM setup_provisioning WHERE state IN ('planning','applying','ready','complete')),count(*) FROM setup_provisioning",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if active{return Err(Error::Conflict);}if total>=64{return Err(Error::Saturated);}
            let actor=Actor{principal:s.principal.to_string(),device:s.device.to_string(),account:self.0.setup.account(c,s)?};
            c.execute("INSERT INTO setup_provisioning(id,request,actor,state,phase,created_ms) VALUES(?,?,?,'planning','metadata',?)",params![id,request_json,encoded(&actor)?,now])?;
            self.audit(c,s,"setup.first_server.plan",&id)?;
            Ok(actor)
        })?;
        let service = self.clone();
        let work_id = id.clone();
        *worker = Some(
            thread::Builder::new()
                .name("vesta-first-plan".into())
                .spawn(move || {
                    if let Err(e) = service.plan_work(&work_id, &actor, request) {
                        service.fail(&work_id, e);
                    }
                })
                .map_err(|_| {
                    self.fail(&id, Error::Unavailable);
                    Error::Unavailable
                })?,
        );
        drop(worker);
        self.status(token, &id)
    }
    pub fn apply(&self, token: &str, input: Apply) -> Result<Job> {
        // Serialize retry lookup with admission, including the short interval
        // before a newly accepted worker starts. Identical concurrent requests
        // therefore see the same durable job rather than a transient Busy.
        let mut worker = self.0.worker.lock().map_err(unavailable)?;
        if input.idempotency_key.is_empty() || input.idempotency_key.len() > 128 {
            return Err(Error::InvalidRequest);
        }
        let hash = hex::encode(Sha256::digest(encoded(&(
            &input.job_id,
            &input.confirmation,
            input.acknowledge_warnings,
            input.acknowledge_legacy_checksum,
        ))?));
        // A matching retry remains a read, including while the worker is running.
        let retry=self.0.setup.with_owner(token,|c,_,_|{
            let row:Option<(String,String)>=c.query_row("SELECT apply_key,apply_hash FROM setup_provisioning WHERE id=? AND apply_key IS NOT NULL",[&input.job_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((key,old))=row {if key==input.idempotency_key&&old==hash{return Ok(Some(job(c,&input.job_id)?));}return Err(Error::IdempotencyConflict);}Ok(None)
        })?;
        if let Some(job) = retry {
            return Ok(job);
        }
        self.idle_worker(&mut worker)?;
        let mut plans = self.0.plans.lock().map_err(unavailable)?;
        let prepared = plans.get(&input.job_id).ok_or(Error::PreconditionFailed)?;
        let approval = if let Prepared::Install(plan) = prepared {
            Some(
                Approval::review(
                    plan,
                    Acknowledgments {
                        warnings: input.acknowledge_warnings,
                        legacy_checksum: input.acknowledge_legacy_checksum,
                    },
                )
                .map_err(|_| Error::PreconditionFailed)?,
            )
        } else {
            None
        };
        let actor=self.0.setup.with_owner(token,|c,_,_|{
            if c.query_row("SELECT EXISTS(SELECT 1 FROM setup_provisioning WHERE id!=? AND state IN ('planning','applying','ready','complete'))",[&input.job_id],|r|r.get::<_,bool>(0))? {return Err(Error::Conflict);}
            let (state,confirmation,actor):(String,String,String)=c.query_row("SELECT state,confirmation,actor FROM setup_provisioning WHERE id=?",[&input.job_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if state!="review"||confirmation!=input.confirmation{return Err(Error::PreconditionFailed);}
            let actor:Actor=decoded(&actor)?;self.actor(c,&actor)?;
            c.execute("UPDATE setup_provisioning SET state='applying',phase='preparing',apply_key=?,apply_hash=? WHERE id=?",params![input.idempotency_key,hash,input.job_id])?;self.audit(c,&self.actor(c,&actor)?,"setup.first_server.apply",&input.job_id)?;Ok(actor)
        })?;
        let prepared = plans.remove(&input.job_id).ok_or(Error::Unavailable)?;
        drop(plans);
        let service = self.clone();
        let id = input.job_id.clone();
        *worker = Some(
            thread::Builder::new()
                .name("vesta-first-apply".into())
                .spawn(move || {
                    if let Err(e) = service.apply_work(&id, &actor, prepared, approval) {
                        service.fail(&id, e);
                    }
                })
                .map_err(|_| {
                    self.fail(&input.job_id, Error::Unavailable);
                    Error::Unavailable
                })?,
        );
        drop(worker);
        self.status(token, &input.job_id)
    }
    fn worker_slot(&self) -> Result<std::sync::MutexGuard<'_, Option<thread::JoinHandle<()>>>> {
        let mut slot = self.0.worker.lock().map_err(unavailable)?;
        self.idle_worker(&mut slot)?;
        Ok(slot)
    }
    fn idle_worker(&self, slot: &mut Option<thread::JoinHandle<()>>) -> Result<()> {
        if slot.as_ref().is_some_and(|h| !h.is_finished()) {
            return Err(Error::Conflict);
        }
        if let Some(h) = slot.take() {
            h.join().map_err(unavailable)?;
        }
        if self.0.stop.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        Ok(())
    }
    pub fn shutdown(&self) -> Result<()> {
        self.0.stop.store(true, Ordering::Release);
        let handle = self.0.worker.lock().map_err(unavailable)?.take();
        if let Some(h) = handle {
            h.join().map_err(unavailable)?;
        }
        Ok(())
    }
    fn actor(&self, c: &rusqlite::Connection, a: &Actor) -> Result<Subject> {
        if self.0.stop.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        let subject = Subject {
            principal: identity::PrincipalId::parse(&a.principal).map_err(unavailable)?,
            device: identity::DeviceId::parse(&a.device).map_err(unavailable)?,
        };
        let policy = identity::policy_in(
            c,
            self.0.setup.db.server_id(),
            self.0.setup.db.clock().millis(),
            Some(&subject),
        )
        .map_err(unavailable)?;
        if !policy.has_current_identity()
            || !policy.holds_builtin(BuiltinRole::Owner)
            || self.0.setup.account(c, &subject)? != a.account
            || self.0.setup.binding(c)? != Some((a.principal.clone(), a.account.clone()))
        {
            return Err(Error::Forbidden);
        }
        Ok(subject)
    }
    fn boundary(&self, actor: &Actor) -> Result<Subject> {
        self.0
            .setup
            .db
            .read_with(|s| self.actor(s.connection(), actor))
            .map_err(|e| match e {
                vesta_state::TransactionError::Work(e) => e,
                _ => Error::Unavailable,
            })
    }
    fn phase(&self, id: &str, phase: &str) -> Result<()> {
        self.0
            .setup
            .db
            .write(|c| {
                c.execute(
                    "UPDATE setup_provisioning SET phase=? WHERE id=?",
                    params![phase, id],
                )?;
                Ok(())
            })
            .map_err(unavailable)
    }
    fn fail(&self, id: &str, e: Error) {
        let code = match e {
            Error::Forbidden | Error::SessionInvalid => "setup_authority_revoked",
            Error::PreconditionFailed => "setup_review_changed",
            _ => "setup_provisioning_failed",
        };
        let _=self.0.setup.db.write(|c|{c.execute("UPDATE setup_provisioning SET state='failed',error=? WHERE id=? AND state IN ('planning','applying')",params![code,id])?;Ok(())});
    }
    fn plan_work(&self, id: &str, actor: &Actor, request: Request) -> Result<()> {
        self.boundary(actor)?;
        let (prepared, review) = match request {
            Request::AdoptCopy {
                name,
                minecraft_version,
            } => {
                let snapshot = self
                    .0
                    .source
                    .as_ref()
                    .ok_or(Error::InvalidRequest)?
                    .snapshot()
                    .map_err(unavailable)?;
                if !snapshot
                    .entries
                    .iter()
                    .any(|e| !e.directory && Some(e.path.as_str()) == self.0.local.entrypoint())
                {
                    return Err(Error::InvalidRequest);
                }
                let review = Review {
                    name,
                    minecraft_version,
                    software: None,
                    loader_version: None,
                    files: snapshot.entries.len(),
                    bytes: snapshot.bytes,
                    warnings: vec![
                        "copied_server_version_is_operator_selected".into(),
                        "source_is_read_only_copy_only".into(),
                    ],
                };
                (Prepared::Copy(snapshot), review)
            }
            Request::Install {
                name,
                software,
                version,
                build,
                loader_version,
                experimental,
            } => {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(unavailable)?;
                let plan = runtime
                    .block_on(self.0.installer.plan(
                        software,
                        &Selection {
                            version: Some(version),
                            build,
                            loader_version,
                            experimental,
                        },
                        &self.0.local.java,
                        &Reporter {
                            service: self,
                            id,
                            actor,
                        },
                    ))
                    .map_err(unavailable)?;
                let review = Review {
                    name,
                    minecraft_version: plan.version().into(),
                    software: Some(plan.software()),
                    loader_version: plan.loader_version().map(str::to_owned),
                    files: plan.artifacts().len(),
                    bytes: plan.artifacts().iter().filter_map(|a| a.size).sum(),
                    warnings: plan.warnings().iter().map(|w| format!("{w:?}")).collect(),
                };
                (Prepared::Install(plan), review)
            }
        };
        let confirmation = uuid::Uuid::new_v4().to_string();
        // Store the bounded private plan in memory only; restart requires a new
        // review instead of deserializing install execution authority from HTTP.
        let mut plans = self.0.plans.lock().map_err(unavailable)?;
        if plans.len() >= 16 {
            return Err(Error::Saturated);
        }
        self.0.setup.db.write_with(|t|->Result<()>{self.actor(t.connection(),actor)?;t.connection().execute("UPDATE setup_provisioning SET state='review',phase='review',review=?,confirmation=? WHERE id=?",params![encoded(&review)?,confirmation,id])?;Ok(())}).map_err(|e|match e{vesta_state::TransactionError::Work(e)=>e,_=>Error::Unavailable})?;
        plans.insert(id.to_owned(), prepared);
        Ok(())
    }
    fn apply_work(
        &self,
        id: &str,
        actor: &Actor,
        prepared: Prepared,
        approval: Option<Approval>,
    ) -> Result<()> {
        let subject = self.boundary(actor)?;
        let servers = self.0.root.join("servers");
        files::create_local_directory(&servers, true).map_err(unavailable)?;
        // The canonical file journal records paths relative to ONE game root.
        // Keep this root stable across failed jobs/reviews; never reconcile a
        // prior job's intent against a different empty directory.
        let destination = servers.join("first");
        files::create_local_directory(&destination, true).map_err(unavailable)?;
        let game = destination.join("game");
        files::create_local_directory(&game, true).map_err(unavailable)?;
        // Reuse the installer's native game-path admission/normalization before
        // constructing a launch. In particular, serializing a Windows verbatim
        // path as forward slashes produces a different, unusable native path.
        let game = self
            .0
            .installer
            .validate_managed_game(&game)
            .map_err(unavailable)?;
        let files = FileBrowser::open(
            self.0.setup.db.clone(),
            &game,
            &[self.0.setup.db.directory().path()],
        )
        .map_err(unavailable)?;
        files.reconcile_pending().map_err(unavailable)?;
        self.clear_file_intents()?;
        let copy = match &prepared {
            Prepared::Copy(snapshot) => Some(snapshot.clone()),
            _ => None,
        };
        let launch = match prepared {
            Prepared::Copy(snapshot) => {
                self.phase(id, "copying")?;
                let target = FileBrowser::open(
                    self.0.setup.db.clone(),
                    &game,
                    &[self.0.setup.db.directory().path()],
                )
                .map_err(unavailable)?;
                self.0
                    .source
                    .as_ref()
                    .ok_or(Error::Unavailable)?
                    .copy_into(&snapshot, &target, &subject, || {
                        self.boundary(actor)
                            .map(|_| ())
                            .map_err(|_| files::FileError::Unavailable)
                    })
                    .map_err(unavailable)?;
                let entrypoint = game.join(self.0.local.entrypoint().ok_or(Error::InvalidRequest)?);
                let entry = snapshot
                    .entries
                    .iter()
                    .find(|e| Some(e.path.as_str()) == self.0.local.entrypoint())
                    .ok_or(Error::InvalidRequest)?;
                ManagedLaunch {
                    java: self.0.local.java.clone(),
                    working_directory: game.clone(),
                    args: if self.0.local.adoption_argfile.is_some() {
                        vec![
                            format!("@{}", entrypoint.to_str().ok_or(Error::InvalidRequest)?),
                            "nogui".into(),
                        ]
                    } else {
                        vec![
                            "-jar".into(),
                            entrypoint.to_str().ok_or(Error::InvalidRequest)?.to_owned(),
                            "nogui".into(),
                        ]
                    },
                    required_files: vec![entrypoint.clone()],
                    artifacts: vec![ManagedArtifact {
                        path: entrypoint,
                        size: entry.size,
                        sha256: entry.sha256.clone(),
                    }],
                }
            }
            Prepared::Install(plan) => {
                // Only identical initial seed effects may survive an interrupted
                // new install. Never assimilate copied/custom worlds or configs.
                for entry in std::fs::read_dir(&game).map_err(unavailable)? {
                    let entry = entry.map_err(unavailable)?;
                    let name = entry.file_name();
                    if name == "eula.txt"
                        && files::read_local_document(&entry.path(), 65536)
                            .is_ok_and(|b| b == INITIAL_EULA)
                    {
                        continue;
                    }
                    if name == "config"
                        && entry.file_type().map_err(unavailable)?.is_dir()
                        && std::fs::read_dir(entry.path())
                            .map_err(unavailable)?
                            .next()
                            .is_none()
                    {
                        continue;
                    }
                    return Err(Error::PreconditionFailed);
                }
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(unavailable)?;
                runtime.block_on(async {
                    self.0
                        .installer
                        .install(
                            &plan,
                            approval.ok_or(Error::PreconditionFailed)?,
                            &Reporter {
                                service: self,
                                id,
                                actor,
                            },
                        )
                        .await
                        .map_err(unavailable)?;
                    self.boundary(actor)?;
                    let launch = self
                        .0
                        .installer
                        .managed_launch(&plan, &game)
                        .await
                        .map_err(unavailable)?;
                    self.boundary(actor)?;
                    if !game.join("eula.txt").exists() {
                        files
                            .write(
                                &subject,
                                &RelativePath::parse("eula.txt").map_err(unavailable)?,
                                &files::WritePrecondition::Absent,
                                INITIAL_EULA,
                            )
                            .map_err(unavailable)?;
                    }
                    if !game.join("config").exists() {
                        files
                            .mkdir(
                                &subject,
                                &RelativePath::parse("config").map_err(unavailable)?,
                            )
                            .map_err(unavailable)?;
                    }
                    files.reconcile_pending().map_err(unavailable)?;
                    Ok::<_, Error>(launch)
                })?
            }
        };
        self.boundary(actor)?;
        self.phase(id, "verifying")?;
        let document = crate::host_software::managed_launch_document(&launch, &[], &self.0.root)
            .map_err(unavailable)?;
        self.0.setup.db.write_with(|t|->Result<()>{self.actor(t.connection(),actor)?;let review:String=t.connection().query_row("SELECT review FROM setup_provisioning WHERE id=?",[id],|r|r.get(0))?;let completion=Completion{review:decoded(&review)?,launch:document,artifacts:launch.artifacts,copy};t.connection().execute("UPDATE setup_provisioning SET state='ready',phase='ready',completion=? WHERE id=?",params![encoded(&completion)?,id])?;Ok(())}).map_err(|e|match e{vesta_state::TransactionError::Work(e)=>e,_=>Error::Unavailable})
    }
    /// Host-local handoff. The callback durably publishes only these generated
    /// documents plus its receipt/consumer record. No callback comes from HTTP.
    pub fn publish_ready(
        &self,
        mut publish: impl FnMut(&str, &str, &str, &str) -> Result<()>,
    ) -> Result<bool> {
        let row=self.0.setup.db.read(|c|c.query_row("SELECT id,actor,completion FROM setup_provisioning WHERE state IN ('ready','complete')",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional().map_err(Into::into)).map_err(unavailable)?;
        let Some((id, actor, raw)) = row else {
            return Ok(false);
        };
        let actor: Actor = decoded(&actor)?;
        let completion: Completion = decoded(&raw)?;
        self.boundary(&actor)?;
        if let Some(expected) = &completion.copy {
            let actual = CopiedExport::open(
                &self.0.root.join("servers/first/game"),
                &[self.0.setup.db.directory().path()],
            )
            .and_then(|s| s.snapshot())
            .map_err(unavailable)?;
            if !expected.same_content(&actual) {
                return Err(Error::PreconditionFailed);
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(unavailable)?;
        runtime
            .block_on(vesta_server_install::verify_managed_artifacts(
                &completion.artifacts,
            ))
            .map_err(unavailable)?;
        let host = self.host_document(&id, &completion.review)?;
        self.clear_file_intents()?;
        self.0.setup.db.write_with(|t|->Result<()>{self.actor(t.connection(),&actor)?;publish(&id,self.0.setup.db.server_id(),&host,&completion.launch)?;t.connection().execute("UPDATE setup_provisioning SET state='complete',phase='complete' WHERE id=? AND state='ready'",[&id])?;Ok(())}).map_err(|e|match e{vesta_state::TransactionError::Work(e)=>e,_=>Error::Unavailable})?;
        Ok(true)
    }
    fn clear_file_intents(&self) -> Result<()> {
        let unresolved=self.0.setup.db.read(|c|c.query_row("SELECT EXISTS(SELECT 1 FROM file_intents WHERE status IN ('pending','recovery-required')) OR EXISTS(SELECT 1 FROM file_trash WHERE status IN ('pending','recovery-required'))",[],|r|r.get::<_,bool>(0)).map_err(Into::into)).map_err(unavailable)?;
        if unresolved {
            Err(Error::PreconditionFailed)
        } else {
            Ok(())
        }
    }
    fn host_document(&self, _id: &str, review: &Review) -> Result<String> {
        // Values use TOML serialization, never interpolation of remote text.
        let mut value = serde_json::json!({"schema-version":1,"paths":{"state":"private/state","game":"servers/first/game","scan-roots":["servers/first/game/config"],"launch-definition":"private/launch.toml"},"pack":{"name":review.name,"version":"initial","revision":1,"minimum-revision":1,"minecraft-version":review.minecraft_version},"pack-access":{"mode":"minecraft"},"listener":{"mode":"tls","bind":"127.0.0.1","port":self.0.port,"certificate":"private/certificate.pem","private-key":"private/key.pem","max-connections":4,"max-request-bytes":65536,"request-timeout-seconds":30}});
        if !self.0.root.join("servers/first/game/config").is_dir() {
            value["paths"]["scan-roots"] = serde_json::json!(["servers/first/game"]);
        }
        if let (Some(software), Some(loader)) = (review.software, review.loader_version.as_ref()) {
            if matches!(
                software,
                Software::Fabric | Software::Quilt | Software::Forge | Software::NeoForge
            ) {
                value["pack"]["loader"] =
                    serde_json::json!({"kind":software.id(),"version":loader});
            }
        }
        let table = toml::Value::try_from(value).map_err(unavailable)?;
        let document = toml::to_string(&table).map_err(unavailable)?;
        crate::config::HostConfiguration::parse_toml(
            document.as_bytes(),
            &self.0.root,
            Path::new("host.toml"),
        )
        .map_err(unavailable)?;
        Ok(document)
    }
    fn audit(
        &self,
        c: &rusqlite::Connection,
        subject: &Subject,
        action: &str,
        id: &str,
    ) -> Result<()> {
        use vesta_state::audit::{AuditActor, AuditRecord};
        let record = AuditRecord::new(
            AuditActor::new(
                subject.principal.to_string(),
                subject.device.to_string(),
                "setup".into(),
            )
            .map_err(unavailable)?,
            action.into(),
            format!("setup:{id}"),
            "accepted".into(),
            None,
            [],
        )
        .map_err(unavailable)?;
        self.0.setup.audit.append(c, &record).map_err(unavailable)?;
        Ok(())
    }
}
fn job(c: &rusqlite::Connection, id: &str) -> Result<Job> {
    let row = c
        .query_row(
            "SELECT id,state,phase,error,review,confirmation FROM setup_provisioning WHERE id=?",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, String>(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    Ok(Job {
        id: row.0,
        confirmation: if row.1 == "review" { row.5 } else { None },
        state: row.1,
        phase: row.2,
        error: row.3,
        review: row.4.map(|r| decoded(&r)).transpose()?,
    })
}
struct Reporter<'a> {
    service: &'a Provisioning,
    id: &'a str,
    actor: &'a Actor,
}
impl ProgressReporter for Reporter<'_> {
    fn start_step(&self, step: &str, _: Option<u32>) {
        let phase = if step.contains("extract") {
            "extracting"
        } else if step.contains("download") {
            "downloading"
        } else {
            "preparing"
        };
        let _ = self.service.phase(self.id, phase);
    }
    fn update_bytes(&self, _: u64, _: Option<u64>) {}
    fn done(&self, _: bool, _: Option<&str>) {}
    fn is_cancelled(&self) -> bool {
        self.service.boundary(self.actor).is_err()
    }
    fn is_paused(&self) -> bool {
        false
    }
    fn is_dry_run(&self) -> bool {
        false
    }
}
