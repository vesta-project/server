//! Durable owner-reviewed hosting, separate from the client modpack workspace.
//! No implicit listener, restart, public certificate or background polling.
mod model;
pub mod native;
mod store;
mod uploads;
mod worker;
use super::{BuildLimits, Input, PackFormat};
use crate::{identity::Subject, operations::ConfirmationToken};
pub use model::*;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::File,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};
use vesta_state::{
    StateDatabase,
    artifacts::{ArtifactDirectory, ArtifactName},
};

pub trait RuntimeHooks: Send + Sync + 'static {
    /// Keep starts blocked while a native activation needs reconciliation.
    fn recovery_block(&self, blocked: bool) -> Result<()>;
    /// Authoritative selected software version; no HTTP-supplied target authority.
    fn minecraft_version(&self) -> Result<String>;
    fn preview(
        &self,
        actor: &Subject,
        changes: &BTreeMap<String, String>,
    ) -> Result<(String, String, String)>;
    /// Return the stopped-game/maintenance hold for the durable publication.
    fn apply(
        &self,
        actor: &Subject,
        key: &str,
        before: &str,
        expected_version: &str,
        changes: &BTreeMap<String, String>,
    ) -> Result<Box<dyn Send>>;
    /// Trusted local bounded observation, used only for crash reconciliation.
    fn properties_hash(&self) -> Result<String>;
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Pending {
    release: Release,
    before: String,
    after: String,
    principal: String,
    device: String,
    key: String,
    digest: String,
}
struct Prepared {
    actor: Subject,
    token: String,
    expires: i64,
    revision: String,
    release: Release,
    before: String,
    after: String,
    external: bool,
}
struct Work {
    actor: Subject,
    job: Job,
    draft: Draft,
}
#[derive(Default)]
struct Queue {
    stop: bool,
    jobs: VecDeque<Work>,
}
struct Inner {
    db: StateDatabase,
    audit: vesta_state::audit::AuditLog,
    hooks: Arc<dyn RuntimeHooks>,
    queue: Mutex<Queue>,
    wake: Condvar,
    cancel: AtomicBool,
    admission: Mutex<()>,
    plans: Mutex<BTreeMap<String, Prepared>>,
}
pub struct Service {
    inner: Arc<Inner>,
    worker: Option<JoinHandle<()>>,
}
#[derive(Clone)]
pub struct Handle {
    inner: Arc<Inner>,
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
fn valid_sha(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn revision(s: &str) -> Result<i64> {
    let n = s.parse::<i64>().map_err(|_| Error::Invalid)?;
    if n < 0 || n > crate::config::MAX_REVISION as i64 || n.to_string() != s {
        return Err(Error::Invalid);
    }
    Ok(n)
}
fn origin(s: &str) -> Result<String> {
    let url = url::Url::parse(s).map_err(|_| Error::Invalid)?;
    if s.len() > 2048
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(Error::Invalid);
    }
    Ok(url.origin().ascii_serialization())
}
fn validate(settings: &Settings) -> Result<()> {
    if origin(&settings.public_origin)? != settings.public_origin.trim_end_matches('/')
        || settings.minecraft_version.is_empty()
        || settings.minecraft_version.len() > 64
        || !settings
            .minecraft_version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        || settings.description.len() > 1024
        || settings.prompt.len() > 1024
        || settings
            .description
            .chars()
            .chain(settings.prompt.chars())
            .any(char::is_control)
        || settings.layers.len() > 16
    {
        return Err(Error::Invalid);
    }
    PackFormat::new(settings.format.major, settings.format.minor)?;
    let mut ids = BTreeSet::new();
    for layer in &settings.layers {
        if !valid_id(&layer.id) || !ids.insert(&layer.id) {
            return Err(Error::Invalid);
        }
        match &layer.source {
            Source::Upload { sha256 } if valid_sha(sha256) => (),
            Source::Provider {
                provider,
                project_id,
                version_id,
            } if matches!(provider.as_str(), "modrinth" | "curseforge")
                && [project_id, version_id].iter().all(|s| {
                    !s.is_empty()
                        && s.len() <= 128
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                }) => {}
            _ => return Err(Error::Invalid),
        }
    }
    // Refuse unsupported version-specific options rather than silently omitting them.
    let (prompt, _) = version_features(&settings.minecraft_version)?;
    if !prompt && (settings.required || !settings.prompt.is_empty()) {
        return Err(Error::Invalid);
    }
    Ok(())
}

// Keep settings-key support separate from arbitrary dotted-version sorting. Release
// versions are supported; snapshots need an explicit catalogue adapter first.
fn version_features(version: &str) -> Result<(bool, bool)> {
    let parts = version
        .split('.')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Error::Invalid)?;
    if !(2..=3).contains(&parts.len()) {
        return Err(Error::Invalid);
    }
    let major = parts[0];
    let minor = parts[1];
    let patch = parts.get(2).copied().unwrap_or(0);
    if major == 1 && minor < 7 || major == 0 {
        return Err(Error::Invalid);
    }
    Ok((
        major > 1 || minor >= 17,
        major > 1 || minor > 20 || minor == 20 && patch >= 3,
    ))
}
fn changes(release: &Release) -> Result<BTreeMap<String, String>> {
    let s = &release.settings;
    let (prompt, id) = version_features(&s.minecraft_version)?;
    if !prompt && (s.required || !s.prompt.is_empty()) {
        return Err(Error::Invalid);
    }
    let mut values = BTreeMap::from([
        ("resource-pack".into(), release.url.clone()),
        ("resource-pack-sha1".into(), release.artifact.sha1().into()),
    ]);
    if prompt {
        values.insert("require-resource-pack".into(), s.required.to_string());
        values.insert(
            "resource-pack-prompt".into(),
            if s.prompt.is_empty() {
                String::new()
            } else {
                serde_json::json!({"text":s.prompt}).to_string()
            },
        );
    }
    if id {
        values.insert("resource-pack-id".into(), release.pack_id.clone());
    }
    Ok(values)
}

impl Service {
    pub fn open(db: StateDatabase, hooks: Arc<dyn RuntimeHooks>) -> Result<Self> {
        db.write(|c| {
            c.execute_batch(include_str!("schema.sql"))?;
            c.execute(
                "INSERT OR IGNORE INTO hosted_pack_state(singleton,revision,pack_id) VALUES(1,0,?)",
                [uuid::Uuid::new_v4().to_string()],
            )?;
            Ok(())
        })?;
        db.write_with(|tx| -> Result<()> {
            let c = tx.connection();
            for mut job in store::jobs(c, 64)? {
                if matches!(job.state.as_str(), "queued" | "running") {
                    job.state = "interrupted".into();
                    job.step = "finished".into();
                    job.error = Some("resource_pack_build_interrupted".into());
                    job.updated_at_ms = db.clock().millis();
                    store::put_job(c, &job)?;
                }
            }
            Ok(())
        })?;
        let lease = db.retain_runtime()?;
        let audit = vesta_state::audit::AuditLog::new(db.clone())?;
        let inner = Arc::new(Inner {
            db,
            audit,
            hooks,
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            cancel: AtomicBool::new(false),
            admission: Mutex::new(()),
            plans: Mutex::new(BTreeMap::new()),
        });
        let handle = Handle {
            inner: inner.clone(),
        };
        let pending: bool = inner.db.read(|c| {
            Ok(c.query_row(
                "SELECT pending IS NOT NULL FROM hosted_pack_state WHERE singleton=1",
                [],
                |r| r.get(0),
            )?)
        })?;
        inner.hooks.recovery_block(pending)?;
        if handle.reconcile().is_err() {
            inner.db.write(|c| {
                c.execute(
                    "UPDATE hosted_pack_state SET recovery=1 WHERE singleton=1",
                    [],
                )?;
                Ok(())
            })?;
        }
        let owned = inner.clone();
        let worker = std::thread::Builder::new()
            .name("vesta-resource-packs".into())
            .spawn(move || {
                let _lease = lease;
                worker::run(owned);
            })
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            inner,
            worker: Some(worker),
        })
    }
    pub fn handle(&self) -> Handle {
        Handle {
            inner: self.inner.clone(),
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.inner.cancel.store(true, Ordering::Release);
        if let Ok(mut queue) = self.inner.queue.lock() {
            queue.stop = true;
            self.inner.wake.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Handle {
    /// Explicit owner decision after inspecting the current properties. This
    /// abandons management's uncertain activation; it never rewrites a file,
    /// deletes a published artifact or starts the game.
    pub fn abandon_recovery(
        &self,
        actor: &Subject,
        key: &str,
        input: &AbandonRecovery,
    ) -> Result<bool> {
        self.authorize(actor)?;
        if !input.confirm_abandon || !valid_sha(&input.expected_properties_sha256) {
            return Err(Error::Invalid);
        }
        let _lock = self.inner.admission.try_lock().map_err(|_| Error::Busy)?;
        let replay: Option<bool> = self.replay(actor, key, &("abandon-recovery", input))?;
        let result = if let Some(result) = replay {
            result
        } else {
            let (current, _, _) = self.inner.hooks.preview(actor, &BTreeMap::new())?;
            if current != input.expected_properties_sha256 {
                return Err(Error::Conflict);
            }
            self.mutate(actor,key,&("abandon-recovery",input),|c|{
            let pending:bool=c.query_row("SELECT pending IS NOT NULL FROM hosted_pack_state WHERE singleton=1",[],|r|r.get(0))?;
            if !pending{return Err(Error::Conflict);}
            c.execute("UPDATE hosted_pack_state SET pending=NULL,recovery=0,active=NULL WHERE singleton=1",[])?;
            Ok(true)
            })?
        };
        let pending: bool = self.inner.db.read(|c| {
            Ok(c.query_row(
                "SELECT pending IS NOT NULL FROM hosted_pack_state WHERE singleton=1",
                [],
                |r| r.get(0),
            )?)
        })?;
        self.inner.hooks.recovery_block(pending)?;
        Ok(result)
    }
    pub fn status(&self, actor: &Subject) -> Result<Status> {
        self.inner
            .db
            .read_with(|tx| -> Result<_> {
                let c = tx.connection();
                store::authorize(c, &self.inner.db, actor)?;
                let (active, recovery): (Option<String>, bool) = c.query_row(
                    "SELECT active,recovery FROM hosted_pack_state WHERE singleton=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                Ok(Status {
                    draft: store::draft(c)?,
                    active: active.map(|s| store::decode(&s)).transpose()?,
                    jobs: store::jobs(c, 8)?,
                    recovery_required: recovery,
                })
            })
            .map_err(Into::into)
    }
    pub fn save(&self, actor: &Subject, key: &str, input: &Save) -> Result<Draft> {
        validate(&input.settings)?;
        let _lock = self.inner.admission.try_lock().map_err(|_| Error::Busy)?;
        self.mutate(actor, key, &("save", input), |c| {
            let current = store::draft(c)?;
            if current.revision != input.expected_revision {
                return Err(Error::Conflict);
            }
            let next = revision(&current.revision)?
                .checked_add(1)
                .filter(|n| *n <= crate::config::MAX_REVISION as i64)
                .ok_or(Error::Limit)?;
            for layer in &input.settings.layers {
                if let Source::Upload { sha256 } = &layer.source {
                    let exists: bool = c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM hosted_pack_blobs WHERE sha256=?)",
                        [sha256],
                        |r| r.get(0),
                    )?;
                    if !exists {
                        return Err(Error::NotFound);
                    }
                }
            }
            c.execute(
                "UPDATE hosted_pack_state SET revision=?,settings=? WHERE singleton=1",
                params![next, store::encode(&input.settings)?],
            )?;
            store::draft(c)
        })
    }
    pub fn build(&self, actor: &Subject, key: &str, input: &BuildRequest) -> Result<Job> {
        self.authorize(actor)?;
        let minecraft_version = self.inner.hooks.minecraft_version()?;
        let _lock = self.inner.admission.try_lock().map_err(|_| Error::Busy)?;
        let mut queue = self.inner.queue.lock().map_err(|_| Error::Unavailable)?;
        if queue.stop {
            return Err(Error::Unavailable);
        }
        let mut work = None;
        let job = self.mutate(actor, key, &("build", input), |c| {
            let draft = store::draft(c)?;
            if draft.revision != input.expected_revision {
                return Err(Error::Conflict);
            }
            let settings = draft.settings.as_ref().ok_or(Error::Invalid)?;
            if settings.layers.is_empty() {
                return Err(Error::Invalid);
            }
            if settings.minecraft_version != minecraft_version {
                return Err(Error::TargetChanged);
            }
            let jobs = store::jobs(c, 64)?;
            if jobs
                .iter()
                .any(|j| matches!(j.state.as_str(), "running" | "queued"))
            {
                return Err(Error::Busy);
            }
            if jobs.len() >= 64 {
                return Err(Error::Limit);
            }
            let now = self.inner.db.clock().millis();
            let job = Job {
                id: id(),
                state: "queued".into(),
                step: "queued".into(),
                created_at_ms: now,
                updated_at_ms: now,
                error: None,
                release: None,
            };
            store::put_job(c, &job)?;
            work = Some(Work {
                actor: actor.clone(),
                job: job.clone(),
                draft,
            });
            Ok(job)
        })?;
        if let Some(work) = work {
            queue.jobs.push_back(work);
            self.inner.wake.notify_one();
        }
        Ok(job)
    }
    pub fn plan(&self, actor: &Subject, input: &PlanRequest) -> Result<Plan> {
        self.authorize(actor)?;
        let _lock = self.inner.admission.try_lock().map_err(|_| Error::Busy)?;
        let (draft, release) = self.inner.db.read_with(|tx| -> Result<_> {
            Ok((
                store::draft(tx.connection())?,
                store::release(tx.connection(), &input.release_id)?,
            ))
        })?;
        if input.expected_revision != draft.revision || release.draft_revision != draft.revision {
            return Err(Error::Conflict);
        }
        if release.settings.minecraft_version != self.inner.hooks.minecraft_version()? {
            return Err(Error::TargetChanged);
        }
        let (before, after, previous) = self.inner.hooks.preview(actor, &changes(&release)?)?;
        let active = self.status(actor)?.active;
        let external = !previous.is_empty() && active.as_ref().is_none_or(|a| a.url != previous);
        let token = ConfirmationToken::random().map_err(|_| Error::Unavailable)?;
        let plan_id = id();
        let expires = self
            .inner
            .db
            .clock()
            .millis()
            .checked_add(120_000)
            .ok_or(Error::Unavailable)?;
        let mut plans = self.inner.plans.lock().map_err(|_| Error::Unavailable)?;
        plans.retain(|_, p| p.expires > self.inner.db.clock().millis());
        if plans.len() >= 16 {
            return Err(Error::Busy);
        }
        plans.insert(
            plan_id.clone(),
            Prepared {
                actor: actor.clone(),
                token: token.hash().to_string(),
                expires,
                revision: draft.revision,
                release: release.clone(),
                before,
                after,
                external,
            },
        );
        Ok(Plan {
            plan_id,
            confirmation_token: token.expose_secret().into(),
            expires_at_ms: expires,
            release,
            replaces_external_pack: external,
            restart_needed: true,
        })
    }
    pub fn apply(&self, actor: &Subject, key: &str, input: &Apply) -> Result<Applied> {
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Invalid);
        }
        self.authorize(actor)?;
        let _lock = self.inner.admission.try_lock().map_err(|_| Error::Busy)?;
        self.reconcile()?;
        // Durable replay precedes in-memory plan lookup. Tokens are hashed in the
        // receipt digest, never stored in cleartext in SQLite/audit.
        let digest = hash(store::encode(&("apply", input))?.as_bytes());
        let replay: Option<Applied> = self.replay(actor, key, &("apply", input))?;
        if let Some(result) = replay {
            return Ok(result);
        }
        let mut plans = self.inner.plans.lock().map_err(|_| Error::Unavailable)?;
        let plan = plans.get(&input.plan_id).ok_or(Error::Confirmation)?;
        let token = ConfirmationToken::try_from(input.confirmation_token.clone())
            .map_err(|_| Error::Confirmation)?;
        if plan.actor != *actor
            || plan.expires <= self.inner.db.clock().millis()
            || plan.token != *token.hash()
            || plan.external && !input.acknowledge_external_pack
        {
            return Err(Error::Confirmation);
        }
        let release = plan.release.clone();
        let before = plan.before.clone();
        let after = plan.after.clone();
        let expected_revision = plan.revision.clone();
        if release.settings.minecraft_version != self.inner.hooks.minecraft_version()? {
            return Err(Error::TargetChanged);
        }
        release
            .artifact
            .open_verified(&self.inner.db.directory().artifacts()?)?;
        self.inner.db.write_with(|tx| -> Result<()> {
            let c = tx.connection();
            store::authorize(c, &self.inner.db, actor)?;
            let (pending, recovery): (Option<String>, bool) = c.query_row(
                "SELECT pending,recovery FROM hosted_pack_state WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if pending.is_some() || recovery {
                return Err(Error::RecoveryRequired);
            }
            if store::draft(c)?.revision != expected_revision {
                return Err(Error::Conflict);
            }
            let count: i64 = c.query_row("SELECT count(*) FROM hosted_pack_receipts", [], |r| {
                r.get(0)
            })?;
            if count >= 4096 {
                return Err(Error::Limit);
            }
            c.execute(
                "UPDATE hosted_pack_state SET pending=? WHERE singleton=1",
                [store::encode(&Pending {
                    release: release.clone(),
                    before: before.clone(),
                    after,
                    principal: actor.principal.as_str().into(),
                    device: actor.device.as_str().into(),
                    key: key.into(),
                    digest,
                })?],
            )?;
            store::audit(c, &self.inner.audit, actor, "resource_pack.apply_pending")?;
            Ok(())
        })?;
        // Canonical file writer independently rechecks capabilities at completion.
        self.inner.hooks.recovery_block(true)?;
        let effect = self.inner.hooks.apply(
            actor,
            &format!("hosted-{}", input.plan_id),
            &before,
            &release.settings.minecraft_version,
            &changes(&release)?,
        );
        let reconciled = self.reconcile();
        if let Err(e) = effect {
            reconciled?;
            return Err(e);
        }
        reconciled?;
        let result = Applied {
            release,
            restart_needed: true,
        };
        plans.remove(&input.plan_id);
        Ok(result)
    }
    fn reconcile(&self) -> Result<()> {
        let pending: Option<String> = self.inner.db.read(|c| {
            Ok(c.query_row(
                "SELECT pending FROM hosted_pack_state WHERE singleton=1",
                [],
                |r| r.get(0),
            )?)
        })?;
        let Some(pending) = pending else {
            return Ok(());
        };
        let pending: Pending = store::decode(&pending)?;
        let current = self.inner.hooks.properties_hash()?;
        let outcome=self.inner.db.write_with(|tx|->Result<bool>{let c=tx.connection();
            if current==pending.after {
                let result=Applied{release:pending.release.clone(),restart_needed:true};
                c.execute("INSERT INTO hosted_pack_receipts VALUES(?,?,?,?,?)",params![pending.principal,pending.device,pending.key,pending.digest,store::encode(&result)?])?;
                c.execute("INSERT OR IGNORE INTO hosted_pack_artifacts VALUES(?,?)",params![pending.release.artifact.sha256(),pending.release.id])?;
                c.execute("UPDATE hosted_pack_state SET active=?,pending=NULL,recovery=0 WHERE singleton=1",[store::encode(&pending.release)?])?;Ok(true)
            }else if current==pending.before{store::clear_pending(c)?;Ok(true)}else{
                c.execute("UPDATE hosted_pack_state SET recovery=1 WHERE singleton=1",[])?;Ok(false)
            }
        })?;
        if outcome {
            self.inner.hooks.recovery_block(false)?;
            Ok(())
        } else {
            Err(Error::RecoveryRequired)
        }
    }
    /// Public access is limited to completed release records, never draft inputs,
    /// arbitrary artifacts or filesystem names. No bearer/session is needed.
    pub fn open_public(&self, sha256: &str) -> Result<(File, Release)> {
        if !valid_sha(sha256) {
            return Err(Error::NotFound);
        }
        let release:Release = self.inner.db.read_with(|tx| -> Result<_> {
            let body:Option<String>=tx.connection().query_row(
                "SELECT body FROM hosted_pack_releases r JOIN hosted_pack_artifacts a ON r.id=a.release_id WHERE a.sha256=?",
                [sha256],|r|r.get(0)).optional()?;
            store::decode(&body.ok_or(Error::NotFound)?)
        })?;
        // The streaming transport checks SHA-256 while reading fixed chunks. Do
        // not hash a 128 MiB pack twice before sending its first byte.
        let name = ArtifactName::new(release.artifact.artifact_name())?;
        let file = self
            .inner
            .db
            .directory()
            .artifacts()?
            .open(&name)?
            .ok_or(Error::NotFound)?;
        if file.metadata()?.len() != release.artifact.size() {
            return Err(Error::Unavailable);
        }
        Ok((file, release))
    }
}

#[cfg(test)]
mod tests;
