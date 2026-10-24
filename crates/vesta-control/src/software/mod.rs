//! Owner-only, provider-pinned software switching. Transport authentication is
//! external; canonical owner/capability checks are repeated here on every request.
//! Downloads never run on the urgent lifecycle worker. Activation never stops or
//! starts Minecraft, and an ambiguous activation blocks starts until rollback.
mod model;
mod runtime;
mod store;
mod worker;
pub use model::{
    ApplyRequest, Installation, Job, PlanRequest, PreparedPlan, PreparedRollback, Result,
    RollbackRequest, SoftwareError, Status,
};
pub use runtime::{RuntimeHooks, StoppedGuard};

use crate::{identity::Subject, operations::ConfirmationToken};
use model::StoredLaunch;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};
use tokio::sync::Semaphore;
use vesta_server_install::{
    Acknowledgments, Approval, InstallPlan, ServerInstaller, SilentProgressReporter, Software,
};
use vesta_state::{StateDatabase, audit::AuditLog};

const PLAN_TTL_MS: i64 = 120_000;
const MAX_PLANS: usize = 16;

struct PendingPlan {
    actor: Subject,
    token_hash: String,
    expires: i64,
    revision: u64,
    plan: InstallPlan,
}
struct PendingRollback {
    actor: Subject,
    token_hash: String,
    expires: i64,
    revision: u64,
}
enum WorkKind {
    Apply(InstallPlan, Option<Approval>),
    Rollback,
}
struct Work {
    id: String,
    actor: Subject,
    kind: WorkKind,
}
#[derive(Default)]
struct Queue {
    stop: bool,
    work: VecDeque<Work>,
}
struct Inner {
    db: StateDatabase,
    audit: AuditLog,
    installer: Arc<ServerInstaller>,
    java: PathBuf,
    game: PathBuf,
    runtime: Arc<dyn RuntimeHooks>,
    plans: Mutex<BTreeMap<String, PendingPlan>>,
    rollbacks: Mutex<BTreeMap<String, PendingRollback>>,
    query_permit: Semaphore,
    queue: Mutex<Queue>,
    wake: Condvar,
    cancel: AtomicBool,
}

/// The owning host keeps this value alive until shutdown. Handle clones do not
/// own the worker lifetime; drop joins admitted work before database shutdown.
pub struct SoftwareService {
    inner: Arc<Inner>,
    worker: Option<JoinHandle<()>>,
}
#[derive(Clone)]
pub struct SoftwareHandle {
    inner: Arc<Inner>,
}

impl SoftwareService {
    /// Trusted local composition only. Roots/Java/current launch never come from
    /// HTTP. Installer base must be separate from game and protected state.
    pub fn open(
        db: StateDatabase,
        installer: Arc<ServerInstaller>,
        java: PathBuf,
        game: PathBuf,
        original_launch_document: String,
        runtime: Arc<dyn RuntimeHooks>,
    ) -> Result<Self> {
        if original_launch_document.is_empty()
            || original_launch_document.len() > 65_536
            || !java.is_absolute()
            || !game.is_absolute()
            || !game.is_dir()
        {
            return Err(SoftwareError::InvalidRequest);
        }
        let game = installer.validate_managed_game(&game)?;
        let original = StoredLaunch {
            installation: Installation {
                software: None,
                version: None,
                build: None,
                installation_id: "original".into(),
            },
            document: original_launch_document,
            artifacts: Vec::new(),
        };
        let audit = AuditLog::new(db.clone())?;
        let recovery = store::initialize(&db, &original)?;
        runtime.recovery_block(recovery)?;
        let lease = db.retain_runtime()?;
        let inner = Arc::new(Inner {
            db,
            audit,
            installer,
            java,
            game,
            runtime,
            plans: Mutex::new(BTreeMap::new()),
            rollbacks: Mutex::new(BTreeMap::new()),
            query_permit: Semaphore::new(1),
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            cancel: AtomicBool::new(false),
        });
        let owned = inner.clone();
        let worker = std::thread::Builder::new()
            .name("vesta-software".into())
            .spawn(move || {
                let _lease = lease;
                worker::run(owned);
            })
            .map_err(|_| SoftwareError::RuntimeUnavailable)?;
        Ok(Self {
            inner,
            worker: Some(worker),
        })
    }
    pub fn handle(&self) -> SoftwareHandle {
        SoftwareHandle {
            inner: self.inner.clone(),
        }
    }
}
impl Drop for SoftwareService {
    fn drop(&mut self) {
        self.inner.cancel.store(true, Ordering::Release);
        if let Ok(mut q) = self.inner.queue.lock() {
            q.stop = true;
            self.inner.wake.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl SoftwareHandle {
    fn authorize(&self, subject: &Subject) -> Result<()> {
        self.inner
            .db
            .read_with(|tx| store::authorize(tx.connection(), &self.inner.db, subject))
            .map_err(Into::into)
    }
    pub fn status(&self, subject: &Subject) -> Result<Status> {
        self.inner
            .db
            .read_with(|tx| -> Result<_> {
                store::authorize(tx.connection(), &self.inner.db, subject)?;
                let s = store::state(tx.connection())?;
                Ok(Status {
                    available: true,
                    unavailable_reason: None,
                    revision: s.revision,
                    current: s.current.installation,
                    previous: s.previous.map(|p| p.installation),
                    recovery_required: s.recovery,
                    catalog: Software::ALL
                        .into_iter()
                        .map(Software::descriptor)
                        .collect(),
                })
            })
            .map_err(Into::into)
    }
    pub fn jobs(&self, subject: &Subject) -> Result<Vec<Job>> {
        self.inner
            .db
            .read_with(|tx| -> Result<_> {
                store::authorize(tx.connection(), &self.inner.db, subject)?;
                store::jobs(tx.connection())
            })
            .map_err(Into::into)
    }
    /// Demand-only metadata query; call on a non-urgent async/background executor.
    /// Exactly one query/plan may use provider/Java resources for this service.
    pub async fn versions(&self, subject: &Subject, software: Software) -> Result<Vec<String>> {
        self.authorize(subject)?;
        let _permit = self
            .inner
            .query_permit
            .try_acquire()
            .map_err(|_| SoftwareError::Busy)?;
        let result = self
            .inner
            .installer
            .versions(software, &SilentProgressReporter)
            .await?;
        self.authorize(subject)?;
        Ok(result)
    }
    pub async fn plan(&self, subject: &Subject, request: PlanRequest) -> Result<PreparedPlan> {
        self.authorize(subject)?;
        validate_request(&request)?;
        let _permit = self
            .inner
            .query_permit
            .try_acquire()
            .map_err(|_| SoftwareError::Busy)?;
        let before = self.status(subject)?;
        if before.recovery_required {
            return Err(SoftwareError::RecoveryRequired);
        }
        let plan = self
            .inner
            .installer
            .plan(
                request.software,
                &vesta_server_install::Selection {
                    version: Some(request.version),
                    build: request.build,
                    loader_version: request.loader_version,
                    experimental: request.experimental,
                },
                &self.inner.java,
                &SilentProgressReporter,
            )
            .await?;
        let after = self.status(subject)?;
        if after.revision != before.revision || after.recovery_required {
            return Err(SoftwareError::RevisionConflict);
        }
        let token = ConfirmationToken::random().map_err(|_| SoftwareError::RuntimeUnavailable)?;
        let now = self.inner.db.clock().millis();
        let expires = now
            .checked_add(PLAN_TTL_MS)
            .ok_or(SoftwareError::StateUnavailable)?;
        let id = uuid::Uuid::new_v4().to_string();
        let response = PreparedPlan {
            plan_id: id.clone(),
            confirmation_token: token.expose_secret().into(),
            expires_at_ms: expires,
            expected_revision: after.revision,
            software: plan.software(),
            version: plan.version().into(),
            build: plan.build(),
            java: plan.java_requirement().clone(),
            warnings: plan.warnings().to_vec(),
            risks: vec![
                "review_backup_before_switch",
                "world_downgrade_not_supported",
                "mods_plugins_and_dimensions_may_be_incompatible",
            ],
        };
        let mut plans = self
            .inner
            .plans
            .lock()
            .map_err(|_| SoftwareError::StateUnavailable)?;
        plans.retain(|_, p| p.expires > now);
        if plans.len() >= MAX_PLANS {
            return Err(SoftwareError::Busy);
        }
        plans.insert(
            id,
            PendingPlan {
                actor: subject.clone(),
                token_hash: token.hash().to_string(),
                expires,
                revision: after.revision,
                plan,
            },
        );
        Ok(response)
    }

    pub fn apply(&self, subject: &Subject, request: ApplyRequest) -> Result<Job> {
        self.authorize(subject)?;
        valid_key(&request.idempotency_key)?;
        if request.plan_id.len() != 36 || request.confirmation_token.len() > 128 {
            return Err(SoftwareError::InvalidRequest);
        }
        let token_hash = hash(request.confirmation_token.as_bytes());
        let request_hash = hash(
            serde_json::json!([
                "apply",
                request.plan_id,
                token_hash,
                request.acknowledge_warnings,
                request.acknowledge_legacy_checksum
            ])
            .to_string()
            .as_bytes(),
        );
        // Serialize admission with worker wake publication and service shutdown.
        let mut queue = self
            .inner
            .queue
            .lock()
            .map_err(|_| SoftwareError::StateUnavailable)?;
        if queue.stop {
            return Err(SoftwareError::RuntimeUnavailable);
        }
        if let Some(existing) = self.inner.db.read_with(|tx| -> Result<_> {
            store::authorize(tx.connection(), &self.inner.db, subject)?;
            store::existing(
                tx.connection(),
                subject,
                &request.idempotency_key,
                &request_hash,
            )
        })? {
            return Ok(existing);
        }
        let mut plans = self
            .inner
            .plans
            .lock()
            .map_err(|_| SoftwareError::StateUnavailable)?;
        let candidate = plans
            .get(&request.plan_id)
            .ok_or(SoftwareError::PlanExpired)?;
        if candidate.actor != *subject {
            return Err(SoftwareError::ConfirmationInvalid);
        }
        let candidate = plans
            .remove(&request.plan_id)
            .ok_or(SoftwareError::PlanExpired)?;
        if candidate.expires <= self.inner.db.clock().millis() {
            return Err(SoftwareError::PlanExpired);
        }
        use subtle::ConstantTimeEq;
        if !bool::from(candidate.token_hash.as_bytes().ct_eq(token_hash.as_bytes())) {
            return Err(SoftwareError::ConfirmationInvalid);
        }
        let approval = Approval::review(
            &candidate.plan,
            Acknowledgments {
                warnings: request.acknowledge_warnings,
                legacy_checksum: request.acknowledge_legacy_checksum,
            },
        )?;
        let job = self.inner.db.write_with(|tx| -> Result<_> {
            let c = tx.connection();
            store::authorize(c, &self.inner.db, subject)?;
            if let Some(existing) =
                store::existing(c, subject, &request.idempotency_key, &request_hash)?
            {
                return Ok(existing);
            }
            let s = store::state(c)?;
            if s.recovery {
                return Err(SoftwareError::RecoveryRequired);
            }
            if s.revision != candidate.revision {
                return Err(SoftwareError::RevisionConflict);
            }
            store::admit(
                c,
                &self.inner.db,
                &self.inner.audit,
                subject,
                "apply",
                &request.idempotency_key,
                &request_hash,
                s.revision,
            )
        })?;
        queue.work.push_back(Work {
            id: job.id.clone(),
            actor: subject.clone(),
            kind: WorkKind::Apply(candidate.plan, Some(approval)),
        });
        self.inner.wake.notify_one();
        Ok(job)
    }

    pub fn prepare_rollback(
        &self,
        subject: &Subject,
        expected_revision: u64,
    ) -> Result<PreparedRollback> {
        let status = self.status(subject)?;
        if status.revision != expected_revision {
            return Err(SoftwareError::RevisionConflict);
        }
        let target = status.previous.ok_or(SoftwareError::PreviousUnavailable)?;
        let token = ConfirmationToken::random().map_err(|_| SoftwareError::RuntimeUnavailable)?;
        let now = self.inner.db.clock().millis();
        let expires = now
            .checked_add(PLAN_TTL_MS)
            .ok_or(SoftwareError::StateUnavailable)?;
        let id = uuid::Uuid::new_v4().to_string();
        let mut plans = self
            .inner
            .rollbacks
            .lock()
            .map_err(|_| SoftwareError::StateUnavailable)?;
        plans.retain(|_, p| p.expires > now);
        if plans.len() >= MAX_PLANS {
            return Err(SoftwareError::Busy);
        }
        plans.insert(
            id.clone(),
            PendingRollback {
                actor: subject.clone(),
                token_hash: token.hash().to_string(),
                expires,
                revision: expected_revision,
            },
        );
        Ok(PreparedRollback {
            plan_id: id,
            confirmation_token: token.expose_secret().into(),
            expires_at_ms: expires,
            expected_revision,
            target,
            risks: vec![
                "rollback_does_not_restore_world_data",
                "review_backup_before_world_downgrade",
            ],
        })
    }

    pub fn rollback(&self, subject: &Subject, request: RollbackRequest) -> Result<Job> {
        self.authorize(subject)?;
        valid_key(&request.idempotency_key)?;
        if !request.confirm || request.plan_id.len() != 36 || request.confirmation_token.len() > 128
        {
            return Err(SoftwareError::ConfirmationInvalid);
        }
        let token_hash = hash(request.confirmation_token.as_bytes());
        let request_hash = hash(
            serde_json::json!([
                "rollback",
                request.expected_revision,
                request.plan_id,
                token_hash,
                true
            ])
            .to_string()
            .as_bytes(),
        );
        let mut queue = self
            .inner
            .queue
            .lock()
            .map_err(|_| SoftwareError::StateUnavailable)?;
        if queue.stop {
            return Err(SoftwareError::RuntimeUnavailable);
        }
        if let Some(existing) = self.inner.db.read_with(|tx| -> Result<_> {
            store::authorize(tx.connection(), &self.inner.db, subject)?;
            store::existing(
                tx.connection(),
                subject,
                &request.idempotency_key,
                &request_hash,
            )
        })? {
            return Ok(existing);
        }
        let mut plans = self
            .inner
            .rollbacks
            .lock()
            .map_err(|_| SoftwareError::StateUnavailable)?;
        let plan = plans
            .get(&request.plan_id)
            .ok_or(SoftwareError::PlanExpired)?;
        if plan.actor != *subject {
            return Err(SoftwareError::ConfirmationInvalid);
        }
        let plan = plans
            .remove(&request.plan_id)
            .ok_or(SoftwareError::PlanExpired)?;
        if plan.expires <= self.inner.db.clock().millis() {
            return Err(SoftwareError::PlanExpired);
        }
        use subtle::ConstantTimeEq;
        if plan.revision != request.expected_revision
            || !bool::from(plan.token_hash.as_bytes().ct_eq(token_hash.as_bytes()))
        {
            return Err(SoftwareError::ConfirmationInvalid);
        }
        let (job, fresh) = self.inner.db.write_with(|tx| -> Result<_> {
            let c = tx.connection();
            store::authorize(c, &self.inner.db, subject)?;
            if let Some(existing) =
                store::existing(c, subject, &request.idempotency_key, &request_hash)?
            {
                return Ok((existing, false));
            }
            let s = store::state(c)?;
            if s.revision != request.expected_revision {
                return Err(SoftwareError::RevisionConflict);
            }
            if s.previous.is_none() {
                return Err(SoftwareError::PreviousUnavailable);
            }
            Ok((
                store::admit(
                    c,
                    &self.inner.db,
                    &self.inner.audit,
                    subject,
                    "rollback",
                    &request.idempotency_key,
                    &request_hash,
                    s.revision,
                )?,
                true,
            ))
        })?;
        if fresh {
            queue.work.push_back(Work {
                id: job.id.clone(),
                actor: subject.clone(),
                kind: WorkKind::Rollback,
            });
            self.inner.wake.notify_one();
        }
        Ok(job)
    }
}

fn validate_request(r: &PlanRequest) -> Result<()> {
    fn identifier(s: &str) -> bool {
        !s.is_empty()
            && s.len() <= 64
            && !s.starts_with('.')
            && !s.contains("..")
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_' | b'+'))
    }
    if !identifier(&r.version)
        || r.loader_version.as_deref().is_some_and(|s| !identifier(s))
        || r.build == Some(0)
        || !r.software.descriptor().automatic_backend
    {
        Err(SoftwareError::InvalidRequest)
    } else {
        Ok(())
    }
}
fn valid_key(key: &str) -> Result<()> {
    if !(8..=128).contains(&key.len())
        || !key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    {
        Err(SoftwareError::InvalidRequest)
    } else {
        Ok(())
    }
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests;
