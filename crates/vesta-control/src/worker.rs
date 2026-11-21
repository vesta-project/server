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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerRole {
    /// Host-owner expansion, confined to this worker's server services.
    Owner,
    Admin,
    Custom { id: Uuid, rank: i32 },
    Maintainer,
    Moderator,
    Viewer,
    /// Old policies spelled Pack User `player`.
    #[serde(rename = "pack-user", alias = "player")]
    PackUser,
}
impl ServerRole {
    pub fn for_effective(role: crate::machine::owners::EffectiveRole) -> Self {
        use crate::machine::owners::{EffectiveRole, ServerRole as HostRole};
        match role {
            EffectiveRole::PrimaryOwner | EffectiveRole::Owner => Self::Owner,
            EffectiveRole::Custom { id, rank } => Self::Custom { id, rank },
            EffectiveRole::Server(role) => match role {
                HostRole::Admin => Self::Admin,
                HostRole::Maintainer => Self::Maintainer,
                HostRole::Moderator => Self::Moderator,
                HostRole::Viewer => Self::Viewer,
                HostRole::PackUser => Self::PackUser,
            },
        }
    }

    pub(crate) fn server_role(&self) -> Option<crate::machine::owners::ServerRole> {
        use crate::machine::owners::ServerRole as HostRole;
        match self {
            Self::Owner | Self::Custom { .. } => None,
            Self::Admin => Some(HostRole::Admin),
            Self::Maintainer => Some(HostRole::Maintainer),
            Self::Moderator => Some(HostRole::Moderator),
            Self::Viewer => Some(HostRole::Viewer),
            Self::PackUser => Some(HostRole::PackUser),
        }
    }

    pub fn capabilities(&self) -> impl Iterator<Item = Capability> + '_ {
        let caps = self.server_role().map_or_else(
            || crate::identity::BuiltinRole::Owner.capabilities(),
            crate::machine::owners::ServerRole::capabilities,
        );
        caps.iter().copied().filter(|&capability| !matches!(self, Self::Custom { .. }) && self.permits_projection(capability))
    }

    pub fn permits_projection(&self, capability: Capability) -> bool {
            !capability.local_only()
                && !matches!(capability, Capability::DevicesRevoke | Capability::CredentialsManage | Capability::SecurityManage)
                && (capability != Capability::AccessSourcesManage || matches!(self, Self::Owner))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Verified profiles of machine owners, independent of the devices they use.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub whitelist_owners: std::collections::BTreeMap<Uuid, String>,
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
    /// Only the immutable published asset service; never management authority.
    fn public_resource_packs(&self) -> Option<crate::resource_packs::hosting::Handle> {
        None
    }
    fn sync_policy(&self, policy: WorkerPolicy) -> Result<(), ManagementError>;
    fn authorize(&self, subject: ServerSubject) -> Result<NetworkHost, ManagementError>;
    /// Host-side observation of the game for the registry (no caller
    /// authority involved). `None` when the worker cannot report it.
    fn observed_state(&self) -> Option<crate::machine::registry::ServerState> {
        None
    }
    /// Host-side observation: the quoted SHA-256 ETag of the server's branding
    /// icon, `None` when there is none or the worker cannot report it.
    fn icon_etag(&self) -> Option<String> {
        None
    }
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
    pub(crate) fn recover_removal_fence(&self, generation: u64) -> vesta_state::Result<()> {
        crate::identity::worker_policy::recover_removal_fence(self.handle.network.database(), generation)
    }
    pub fn request_system_shutdown(&self) -> Result<(), HostError> {
        self.host.request_system_shutdown()
    }
    pub fn advance_system_shutdown(&self) -> bool {
        self.host.advance_system_shutdown()
    }
    pub fn shutdown_system(self, timeout: Duration) -> Result<WorkerShutdown, HostError> {
        let Self { host, handle } = self;
        match host.shutdown_system(timeout)? {
            HostShutdown::Complete => Ok(WorkerShutdown::Complete),
            HostShutdown::TimedOut(host) => Ok(WorkerShutdown::TimedOut(Box::new(Self {
                host: *host,
                handle,
            }))),
        }
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
    fn icon_etag(&self) -> Option<String> {
        crate::identity::management::permissions::branding::stored_icon_etag(
            self.network.database(),
        )
    }
    fn public_resource_packs(&self) -> Option<crate::resource_packs::hosting::Handle> {
        Some(self.network.resource_packs().clone())
    }
    fn server_id(&self) -> Uuid {
        self.server_id
    }
    fn observed_state(&self) -> Option<crate::machine::registry::ServerState> {
        use crate::machine::registry::ServerState;
        use vesta_game_runtime::readiness::Readiness;
        Some(match self.network.runtime().readiness().ok()?.readiness {
            Readiness::Stopped => match self.network.sleep().public_state(self.network.runtime()) {
                Ok(crate::motd::State::Sleeping) => ServerState::Sleeping,
                Ok(crate::motd::State::Waking) => ServerState::Starting,
                _ => ServerState::Stopped,
            },
            Readiness::Starting => ServerState::Starting,
            Readiness::Ready => ServerState::Online,
            Readiness::Degraded(_) => ServerState::Failed,
        })
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
    pub(crate) fn server_id(&self) -> Uuid {
        self.server_id
    }
    pub(crate) fn expires_at_ms(&self) -> i64 {
        self.subject.expires_at_ms
    }
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
