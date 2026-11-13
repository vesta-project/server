//! In-process boundary for one server. No listener, credentials or SFTP startup.
//! The host supplies authenticated subjects and complete effective policy snapshots.
use crate::{
    config::HostConfiguration,
    host::{Host, HostError, HostShutdown, NetworkHost},
    identity::{Capability, DeviceId, PrincipalId, Resource, Subject, management::ManagementError},
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;
use vesta_state::{Clock, StateDatabase};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSubject {
    pub host_id: Uuid,
    pub server_id: Uuid,
    pub account_id: Uuid,
    pub device_id: Uuid,
    pub generation: u64,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub capabilities: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerRole {
    Admin,
    Moderator,
    Player,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerGrant {
    pub account_id: Uuid,
    pub device_id: Uuid,
    pub role: ServerRole,
    pub capabilities: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerPolicy {
    pub host_id: Uuid,
    pub server_id: Uuid,
    pub generation: u64,
    pub grants: Vec<WorkerGrant>,
}
/// Cloneable admission handle; dropping it never stops the game. The owner must
/// retain Worker until shutdown completes (including a timeout return).
#[derive(Clone)]
pub struct WorkerHandle {
    network: NetworkHost,
    host_id: Uuid,
    server_id: Uuid,
    active_generation: Arc<AtomicU64>,
}
/// The service owner retains the existing runtime, readiness, save leases and
/// supervisor binding. Construction does not enroll identities or start listeners.
pub struct Worker {
    host: Host,
    handle: WorkerHandle,
}
pub enum WorkerShutdown {
    Complete,
    TimedOut(Box<Worker>),
}
/// Stable seam for future authenticated IPC. Subjects and policy are data only;
/// the returned facade is the temporary in-process dispatch adapter.
pub trait ServerWorker: Send + Sync {
    fn server_id(&self) -> Uuid;
    fn sync_policy(&self, policy: WorkerPolicy) -> Result<(), ManagementError>;
    fn authorize(&self, subject: ServerSubject) -> Result<NetworkHost, ManagementError>;
}
impl Worker {
    pub fn open(
        config: HostConfiguration,
        clock: Arc<dyn Clock>,
        supervisor: Option<vesta_supervisor::RemoteProcess>,
        host_id: Uuid,
        server_id: Uuid,
    ) -> Result<Self, HostError> {
        if host_id.is_nil() || server_id.is_nil() || host_id == server_id {
            return Err(HostError::Supervisor("worker_binding_invalid"));
        }
        let host = Host::open_bound(config, clock, supervisor, Some((host_id, server_id)))?;
        let handle = WorkerHandle {
            network: host.network_handle(),
            host_id,
            server_id,
            active_generation: Arc::new(AtomicU64::new(0)),
        };
        Ok(Self { host, handle })
    }
    /// Registry is the authority for whether a worker exists. Configuration and
    /// supervisor are trusted host-local inputs, never request-supplied paths.
    pub fn from_registry(
        entry: Option<&crate::machine::registry::ServerEntry>,
        config: HostConfiguration,
        clock: Arc<dyn Clock>,
        supervisor: Option<vesta_supervisor::RemoteProcess>,
        host_id: Uuid,
    ) -> Result<Option<Self>, HostError> {
        match entry {
            None => Ok(None),
            Some(entry) if entry.state == crate::machine::registry::ServerState::Deleted => {
                Ok(None)
            }
            Some(entry) => {
                Self::open(config, clock, supervisor, host_id, entry.server_id).map(Some)
            }
        }
    }
    pub fn handle(&self) -> WorkerHandle {
        self.handle.clone()
    }
    pub fn shutdown(self, timeout: Duration) -> Result<WorkerShutdown, HostError> {
        let Self { host, handle } = self;
        match host.shutdown(timeout)? {
            HostShutdown::Complete => Ok(WorkerShutdown::Complete),
            HostShutdown::TimedOut(host) => Ok(WorkerShutdown::TimedOut(Box::new(Self {
                host: *host,
                handle,
            }))),
        }
    }
}
impl ServerWorker for WorkerHandle {
    fn server_id(&self) -> Uuid {
        self.server_id
    }
    fn sync_policy(&self, policy: WorkerPolicy) -> Result<(), ManagementError> {
        if policy.host_id != self.host_id || policy.server_id != self.server_id {
            return Err(ManagementError::Forbidden);
        }
        crate::identity::worker_policy::sync(self.network.database(), &policy)?;
        self.active_generation
            .fetch_max(policy.generation, Ordering::AcqRel);
        Ok(())
    }
    fn authorize(&self, subject: ServerSubject) -> Result<NetworkHost, ManagementError> {
        let trusted = TrustedAuthorization {
            db: self.network.database().clone(),
            host_id: self.host_id,
            server_id: self.server_id,
            active_generation: self.active_generation.clone(),
            subject,
        };
        trusted.authenticate()?;
        Ok(self.network.clone().with_trusted(trusted))
    }
}
pub(crate) struct TrustedAuthorization {
    db: StateDatabase,
    host_id: Uuid,
    server_id: Uuid,
    active_generation: Arc<AtomicU64>,
    subject: ServerSubject,
}
impl TrustedAuthorization {
    pub(crate) fn authenticate(&self) -> Result<Subject, ManagementError> {
        let s = &self.subject;
        let now = self.db.clock().millis();
        if self.active_generation.load(Ordering::Acquire) == 0
            || s.generation != self.active_generation.load(Ordering::Acquire)
            || s.host_id != self.host_id
            || s.server_id != self.server_id
            || s.issued_at_ms > now
            || s.expires_at_ms <= now
            || s.expires_at_ms
                .checked_sub(s.issued_at_ms)
                .is_none_or(|ttl| ttl > 30_000 || ttl <= 0)
        {
            return Err(ManagementError::SessionInvalid);
        }
        let subject = Subject {
            principal: PrincipalId::parse(&s.account_id.to_string())?,
            device: DeviceId::parse(&s.device_id.to_string())?,
        };
        crate::identity::worker_policy::authenticate(&self.db, s, &subject)?;
        Ok(subject)
    }
    pub(crate) fn authorize(
        &self,
        capability: Capability,
        resource: Option<&Resource>,
    ) -> Result<Subject, ManagementError> {
        let subject = self.authenticate()?;
        if !self
            .subject
            .capabilities
            .iter()
            .any(|c| c == capability.id())
        {
            return Err(ManagementError::Forbidden);
        }
        let allowed = self
            .db
            .read_with(|tx| {
                crate::identity::policy_in(
                    tx.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                    Some(&subject),
                )
                .map(|p| p.check(capability, resource).allowed())
            })
            .map_err(|_| ManagementError::Unavailable)?;
        if !allowed {
            return Err(ManagementError::Forbidden);
        }
        Ok(subject)
    }
}
