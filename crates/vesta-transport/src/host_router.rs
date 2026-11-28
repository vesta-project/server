//! Host authority and HTTP routing. No legacy bearer is accepted here.
#![allow(
    clippy::result_large_err,
    reason = "early error replies are returned directly as HTTP responses"
)]
use super::*;
use crate::host_identity::HostIdentity;
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;
use vesta_control::{
    machine::{
        imports::{
            FolderBrowser, OwnerBinding,
            jobs::{ImportJobs, JobError},
        },
        owners::{Actor, Device, EffectiveRole, HostRole, ServerRole},
    },
    worker::{ServerSubject, ServerWorker, Worker, WorkerHandle},
};
mod access;
mod creates;
mod enablement;
mod host_access;
mod imports;
mod owners;
mod public_access;
mod removed;
mod startup;
mod storage;

/// Builds the in-process worker for a registered imported server.
pub type WorkerFactory = dyn Fn(Uuid, bool) -> Result<Worker, JobError> + Send + Sync;
/// Owner import jobs plus the worker factory used once a copy is committed.
pub struct ImportService {
    pub jobs: ImportJobs,
    create: Box<WorkerFactory>,
}
#[derive(Clone)]
pub struct HostRouter {
    pub identity: Arc<HostIdentity>,
    pub name: String,
    pub folders: Arc<FolderBrowser>,
    pub workers: Arc<Mutex<Vec<Arc<dyn ServerWorker>>>>,
    /// Workers this router created or adopted; the host shuts them down.
    owned: Arc<Mutex<Vec<Worker>>>,
    imports: Option<Arc<ImportService>>,
    configured_worker: Option<(Uuid, Arc<WorkerFactory>)>,
    catalogue: Option<Arc<dyn vesta_control::machine::creates::Catalogue>>,
    creator: Option<Arc<dyn vesta_control::machine::creates::CreateInstaller>>,
    removal_lock: Arc<Mutex<()>>,
    storage: Arc<storage::Measurements>,
    stopping: Arc<std::sync::atomic::AtomicBool>,
}
impl HostRouter {
    pub fn new(identity: Arc<HostIdentity>, name: String, folders: Arc<FolderBrowser>) -> Self {
        Self {
            identity,
            name,
            folders,
            workers: Default::default(),
            owned: Default::default(),
            imports: None,
            configured_worker: None,
            catalogue: None,
            creator: None,
            removal_lock: Default::default(),
            storage: Default::default(),
            stopping: Default::default(),
        }
    }
    /// Enable the import job routes. `create` builds a registered server's
    /// worker (normally `ImportJobs::create_worker` with the host clock).
    pub fn with_imports(
        self,
        jobs: ImportJobs,
        create: impl Fn(Uuid) -> Result<Worker, JobError> + Send + Sync + 'static,
    ) -> Self {
        self.with_startup_worker_factory(jobs, move |id, _| create(id))
    }

    pub fn with_startup_worker_factory(
        mut self,
        jobs: ImportJobs,
        create: impl Fn(Uuid, bool) -> Result<Worker, JobError> + Send + Sync + 'static,
    ) -> Self {
        self.catalogue = vesta_control::machine::creates::LiveCatalogue::open(jobs.servers_root())
            .ok()
            .map(|c| Arc::new(c) as Arc<dyn vesta_control::machine::creates::Catalogue>);
        self.imports = Some(Arc::new(ImportService {
            jobs,
            create: Box::new(create),
        }));
        self
    }

    pub fn with_configured_worker(
        mut self,
        server: Uuid,
        create: impl Fn(Uuid, bool) -> Result<Worker, JobError> + Send + Sync + 'static,
    ) -> Self {
        self.configured_worker = Some((server, Arc::new(create)));
        self
    }

    fn worker_factory(&self, id: Uuid) -> Result<&WorkerFactory, JobError> {
        if let Some((configured, factory)) = &self.configured_worker
            && *configured == id
        {
            return Ok(factory.as_ref());
        }
        self.imports
            .as_ref()
            .map(|service| service.create.as_ref())
            .ok_or(JobError::UNAVAILABLE)
    }

    pub fn with_storage_java_cache(mut self, java_cache: std::path::PathBuf) -> Self {
        self.storage = Arc::new(storage::Measurements::with_java_cache(java_cache));
        self
    }
    /// Substitute metadata providers (also useful for offline installations).
    pub fn with_create_catalogue(
        mut self,
        catalogue: Arc<dyn vesta_control::machine::creates::Catalogue>,
    ) -> Self {
        self.catalogue = Some(catalogue);
        self
    }
    /// Enable `new` server creation jobs with this installer (normally
    /// [`vesta_control::machine::creates::LiveCreateInstaller`]).
    pub fn with_create_installer(
        mut self,
        installer: Arc<dyn vesta_control::machine::creates::CreateInstaller>,
    ) -> Self {
        self.creator = Some(installer);
        self
    }
    /// The owner-chosen machine name, or the configured one until renamed.
    pub fn display_name(&self) -> String {
        self.identity
            .state
            .settings()
            .ok()
            .and_then(|s| s.name)
            .unwrap_or_else(|| self.name.clone())
    }
    pub fn imports(&self) -> Option<&ImportJobs> {
        self.imports.as_deref().map(|i| &i.jobs)
    }
    /// Close machine job admission before transport drain. Late installs may
    /// finish their current safe phase but cannot create/start another worker.
    pub fn request_system_shutdown(&self) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(jobs) = self.imports() {
            jobs.request_shutdown();
        }
        if let Ok(owned) = self.owned.lock() {
            for worker in owned.iter() {
                if let Err(e) = worker.request_system_shutdown() {
                    eprintln!("host_worker_shutdown_request_failed: {}", e.code());
                }
            }
        }
    }
    pub fn advance_system_shutdown(&self) {
        if let Ok(owned) = self.owned.try_lock() {
            for worker in owned.iter() {
                worker.advance_system_shutdown();
            }
        }
    }
    pub fn shutdown_jobs_complete(&self) -> bool {
        self.imports().is_none_or(|j| j.is_shutdown_complete())
    }
    fn is_stopping(&self) -> bool {
        self.stopping.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn add_worker(&self, worker: Arc<dyn ServerWorker>) {
        self.workers.lock().expect("worker registry").push(worker);
    }
    /// Route a worker the router owns from now on (registered at startup or
    /// imported while running); its management routes work immediately.
    pub fn adopt_worker(&self, worker: Worker) -> WorkerHandle {
        let handle = worker.handle();
        self.add_worker(Arc::new(handle.clone()));
        self.owned.lock().expect("owned workers").push(worker);
        handle
    }
    fn owned_handle(&self, server_id: Uuid) -> Option<WorkerHandle> {
        self.owned
            .lock()
            .ok()?
            .iter()
            .map(Worker::handle)
            .find(|h| h.server_id() == server_id)
    }
    /// The worker for an imported server, created on first use (for example
    /// the first start after the copy was committed).
    fn import_worker(&self, server_id: Uuid) -> Result<WorkerHandle, JobError> {
        use vesta_control::machine::registry::ServerState;
        let entry = self
            .identity
            .state
            .server(server_id)?
            .ok_or(JobError::NOT_FOUND)?;
        if entry.state == ServerState::Disabled {
            return Err(JobError("server_disabled"));
        }
        if entry.state == ServerState::Deleted {
            return Err(JobError::NOT_FOUND);
        }
        if self.is_stopping() || self.imports().is_some_and(ImportJobs::is_stopping) {
            return Err(JobError("host_stopping"));
        }
        let mut owned = self.owned.lock().map_err(|_| JobError::UNAVAILABLE)?;
        if self.is_stopping() || self.imports().is_some_and(ImportJobs::is_stopping) {
            return Err(JobError("host_stopping"));
        }
        if let Some(handle) = owned
            .iter()
            .map(Worker::handle)
            .find(|h| h.server_id() == server_id)
        {
            return Ok(handle);
        }
        if self
            .workers
            .lock()
            .map_err(|_| JobError::UNAVAILABLE)?
            .iter()
            .any(|w| w.server_id() == server_id)
        {
            // Routed through an externally configured worker; not ours to start.
            return Err(JobError("worker_unavailable"));
        }
        let worker = self.worker_factory(server_id)?(server_id, false)?;
        let handle = worker.handle();
        if self.is_stopping() || self.imports().is_some_and(ImportJobs::is_stopping) {
            let _ = worker.request_system_shutdown();
            owned.push(worker);
            return Err(JobError("host_stopping"));
        }
        self.add_worker(Arc::new(handle.clone()));
        owned.push(worker);
        Ok(handle)
    }
    /// Owner/role-filtered server cards: registry facts plus the software and
    /// Minecraft version recorded by the job that created or imported the
    /// server, and the branding icon ETag observed from its worker. Unknown
    /// values are null.
    fn server_cards(
        &self,
        entries: Vec<(vesta_control::machine::registry::ServerEntry, EffectiveRole)>,
    ) -> Vec<serde_json::Value> {
        let facts = self
            .imports()
            .and_then(|jobs| jobs.server_facts().ok())
            .unwrap_or_default();
        let workers = self.workers.lock().map(|w| w.clone()).unwrap_or_default();
        entries
            .into_iter()
            .map(|(e, r)| {
                let fact = facts.get(&e.server_id).cloned().unwrap_or_default();
                let worker = workers.iter().find(|w| w.server_id() == e.server_id);
                let icon = worker.and_then(|w| w.icon_etag());
                let game_icon = worker.and_then(|w| w.game_icon_etag());
                let icon_source = worker.map(|w| w.icon_source()).unwrap_or_default();
                json!({"serverId":e.server_id,"name":e.name,"gamePort":e.game_port,
                    "state":format!("{:?}",e.state).to_lowercase(),"enabled":e.state != vesta_control::machine::registry::ServerState::Disabled,"role":role_name(r),
                    "software":fact.software.map(|(kind,version)|json!({"kind":kind,"version":version})),
                    "minecraftVersion":fact.minecraft_version,"iconEtag":icon,"gameIconEtag":game_icon,"iconSource":icon_source})
            })
            .collect()
    }
    /// After a host restart, give every registered server that has a managed
    /// `<servers>/<serverId>/host.toml` (created or imported) its worker back.
    /// Servers already routed are skipped; failures are returned, not fatal.
    pub fn restore_registered_workers(&self) -> Vec<(Uuid, JobError)> {
        let state = &self.identity.state;
        let entries = match state.snapshot().map(|s| s.primary_account_id) {
            Ok(Some(primary)) => state.visible_servers(primary).unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut failed = Vec::new();
        for (entry, _) in entries {
            let id = entry.server_id;
            let routed = self
                .workers
                .lock()
                .is_ok_and(|w| w.iter().any(|w| w.server_id() == id));
            let configured = self
                .configured_worker
                .as_ref()
                .is_some_and(|(server, _)| *server == id);
            let managed = self
                .imports()
                .is_some_and(|jobs| jobs.server_dir(id).join("host.toml").is_file());
            if entry.state == vesta_control::machine::registry::ServerState::Disabled
                || routed
                || !(configured || managed)
            {
                continue;
            }
            match self
                .worker_factory(id)
                .and_then(|factory| factory(id, true))
            {
                Ok(worker) => {
                    self.adopt_worker(worker);
                }
                Err(e) => {
                    let _ = state.record_startup_open_failure(id, e.0);
                    failed.push((id, e));
                }
            }
        }
        failed
    }
    /// Detach owned workers for shutdown, after the transport has stopped.
    pub fn take_owned_workers(&self) -> Vec<Worker> {
        let taken = std::mem::take(&mut *self.owned.lock().expect("owned workers"));
        let ids: Vec<Uuid> = taken.iter().map(|w| w.handle().server_id()).collect();
        self.workers
            .lock()
            .expect("worker registry")
            .retain(|w| !ids.contains(&w.server_id()));
        taken
    }
    fn refresh_worker_policies(&self, server: Option<Uuid>) -> Result<(), Reply> {
        let unavailable = || failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable");
        let workers = self.workers.lock().map_err(|_| unavailable())?.clone();
        for worker in workers {
            if server.is_none_or(|id| worker.server_id() == id) {
                let policy = self
                    .identity
                    .state
                    .worker_policy(worker.server_id())
                    .map_err(|_| unavailable())?;
                worker.sync_policy(policy).map_err(|_| unavailable())?;
            }
        }
        Ok(())
    }

    /// Copy each worker's observed game state into the registry so
    /// `GET /v1/host/servers` reflects starting/online/stopped/failed.
    pub fn refresh_server_states(&self) {
        let workers = match self.workers.lock() {
            Ok(w) => w.clone(),
            Err(_) => return,
        };
        for worker in workers {
            if let Some(observed) = worker.observed_state() {
                let _ = self
                    .identity
                    .state
                    .observe_server_state(worker.server_id(), observed);
            }
        }
    }
}
struct Application {
    router: HostRouter,
    tls: Arc<tls::Material>,
    timeout: Duration,
    host_bodies: Arc<Semaphore>,
    bodies: Arc<Semaphore>,
    jobs: BlockingJobs,
    read_jobs: BlockingJobs,
    enrollment_jobs: BlockingJobs,
    upload_jobs: BlockingJobs,
    limiter: Mutex<PeerRateLimiter>,
    device_limiter: Mutex<RateLimiter<Uuid>>,
    session_peer_limiter: Mutex<PeerRateLimiter>,
    here_peer_limiter: Mutex<PeerRateLimiter>,
    here_host_limiter: Mutex<RateLimiter<()>>,
    tickets: Mutex<std::collections::HashMap<String, DownloadTicket>>,
    downloads: Arc<Downloads<vesta_control::pack_access::RealtimeSession>>,
    ownership: owners::Challenges,
    streams: Mutex<Vec<HostStream>>,
}

type DownloadTicket = (zeroize::Zeroizing<String>, Uuid, u64, std::time::Instant);

struct HostStream {
    stop: watch::Sender<bool>,
    shared: Arc<Shared>,
    account: Uuid,
    peer: std::net::IpAddr,
    reservation: std::sync::Weak<()>,
}
impl Transport {
    pub fn start_host(router: HostRouter, listener: &TlsListener) -> Result<Self, TransportError> {
        pending::start_small_application(listener, None, move |tls, config| {
            let cpus = thread::available_parallelism().map_or(1, |n| n.get());
            let runtime = tokio::runtime::Handle::current();
            let app = Arc::new(Application {
                downloads: Arc::new(Downloads::new(DOWNLOADS)),
                tickets: Default::default(),
                streams: Default::default(),
                ownership: Default::default(),
                router,
                tls,
                timeout: Duration::from_secs(config.request_timeout_seconds().into()),
                host_bodies: Arc::new(Semaphore::new(256 * 1024)),
                bodies: Arc::new(Semaphore::new(REQUEST_BODY_BUDGET)),
                jobs: BlockingJobs::with_workers(runtime.clone(), cpus.clamp(2, 4)),
                read_jobs: BlockingJobs::with_workers(runtime.clone(), cpus.clamp(1, 2)),
                enrollment_jobs: BlockingJobs::with_workers(runtime.clone(), 1),
                upload_jobs: BlockingJobs::with_workers(runtime, (cpus / 4).clamp(1, 2)),
                limiter: Mutex::new(PeerRateLimiter::new(
                    RatePolicy::new(
                        config.peer_requests_per_minute(),
                        Duration::from_secs(60),
                        1024,
                        Duration::from_secs(120),
                    )
                    .expect("validated policy"),
                )),
                device_limiter: Mutex::new(RateLimiter::new(
                    RatePolicy::new(
                        config.session_requests_per_minute(),
                        Duration::from_secs(60),
                        4096,
                        Duration::from_secs(120),
                    )
                    .expect("validated policy"),
                )),
                session_peer_limiter: Mutex::new(PeerRateLimiter::new(
                    RatePolicy::new(30, Duration::from_secs(60), 1024, Duration::from_secs(120))
                        .expect("fixed policy"),
                )),
                here_peer_limiter: Mutex::new(PeerRateLimiter::new(
                    RatePolicy::new(60, Duration::from_secs(60), 1024, Duration::from_secs(120))
                        .expect("fixed policy"),
                )),
                here_host_limiter: Mutex::new(RateLimiter::new(
                    RatePolicy::new(600, Duration::from_secs(60), 1, Duration::from_secs(120))
                        .expect("fixed policy"),
                )),
            });
            // Registry state follows observed worker state while the host runs.
            let weak = Arc::downgrade(&app);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(2));
                let mut projected_generation = None;
                loop {
                    tick.tick().await;
                    let Some(router) = weak.upgrade().map(|a| a.router.clone()) else {
                        break;
                    };
                    if let Ok(generation) = tokio::task::spawn_blocking(move || {
                        router.refresh_server_states();
                        let Ok(snapshot) = router.identity.state.snapshot() else {
                            return projected_generation;
                        };
                        if projected_generation != Some(snapshot.auth_generation)
                            && router.refresh_worker_policies(None).is_ok()
                        {
                            return Some(snapshot.auth_generation);
                        }
                        projected_generation
                    })
                    .await
                    {
                        projected_generation = generation;
                    }
                }
            });
            removed::start_janitor(Arc::downgrade(&app));
            storage::start(Arc::downgrade(&app));
            app
        })
    }
}
impl HttpApplication for Application {
    fn tls(&self) -> &Arc<tls::Material> {
        &self.tls
    }
    fn timeout(&self) -> Duration {
        self.timeout
    }
    fn active_upgrades(&self) -> usize {
        self.streams.lock().map_or(0, |streams| {
            streams
                .iter()
                .map(|stream| stream.shared.realtime.active())
                .sum()
        })
    }
    async fn reap(&self, all: bool) {
        let streams = {
            let mut streams = self.streams.lock().unwrap();
            if all {
                for stream in streams.iter() {
                    let _ = stream.stop.send(true);
                }
                std::mem::take(&mut *streams)
            } else {
                streams.retain(|stream| stream.reservation.strong_count() != 0);
                Vec::new()
            }
        };
        for stream in streams {
            stream.shared.reap(true).await;
        }
        self.jobs.reap(all).await;
        self.read_jobs.reap(all).await;
        self.enrollment_jobs.reap(all).await;
        self.upload_jobs.reap(all).await;
    }
    async fn dispatch(
        self: Arc<Self>,
        request: Request<Incoming>,
        peer: SocketAddr,
    ) -> Result<Reply, Infallible> {
        Ok(self.route(request, peer).await)
    }
}
/// A port refusal on a per-server management route, in that API's envelope: the closed
/// `conflict` code with the cause as its reason, which launchers translate.
fn port_conflict(reason: &str) -> Reply {
    let mut reply = routes::json(
        json!({"error":{"code":"conflict","reason":reason,"requestId":Uuid::new_v4()}}),
    );
    *reply.status_mut() = StatusCode::CONFLICT;
    reply
}
fn failure(status: StatusCode, code: &str) -> Reply {
    let mut reply = routes::json(json!({"error":{"code":code,"requestId":Uuid::new_v4()}}));
    *reply.status_mut() = status;
    reply
}
fn output<T: Serialize>(result: vesta_control::machine::sessions::Result<T>) -> Reply {
    match result {
        Ok(value) => routes::json(serde_json::to_value(value).expect("wire response")),
        Err(e) if e.0 == "rate_limited" => with_retry_after(
            failure(StatusCode::TOO_MANY_REQUESTS, e.0),
            Duration::from_secs(1),
        ),
        Err(e) => failure(
            match e.0 {
                c if c.ends_with("unavailable") => StatusCode::SERVICE_UNAVAILABLE,
                "account_has_no_access" | "device_approval_required" => StatusCode::FORBIDDEN,
                "device_key_unusable" => StatusCode::CONFLICT,
                _ => StatusCode::UNAUTHORIZED,
            },
            e.0,
        ),
    }
}
fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Reply> {
    serde_json::from_slice(bytes).map_err(|_| failure(StatusCode::BAD_REQUEST, "invalid_request"))
}
fn no_content() -> Reply {
    response(StatusCode::NO_CONTENT, Bytes::new(), "application/json")
}
fn role_name(role: EffectiveRole) -> String {
    match role {
        EffectiveRole::PrimaryOwner => "primary_owner".into(),
        EffectiveRole::Owner => "owner".into(),
        EffectiveRole::Server(role) | EffectiveRole::EditedBuiltin { role, .. } => {
            role.as_str().into()
        }
        EffectiveRole::Custom { id, .. } => id.to_string(),
    }
}
fn host_role_name(role: HostRole) -> &'static str {
    match role {
        HostRole::PrimaryOwner => "primary_owner",
        HostRole::Owner => "owner",
    }
}

fn is_enrollment_path(path: &str) -> bool {
    matches!(
        path,
        "/v1/host/sessions"
            | "/v1/host/sessions/challenge"
            | "/v1/host/sessions/key-challenge"
            | "/v1/host/pake/start"
            | "/v1/host/pake/finish"
            | "/v1/host/accounts/challenge"
            | "/v1/host/accounts/complete"
            | "/v1/host/claim"
            | "/v1/host/links"
            | "/v1/host/invites/redeem"
            | "/v1/host/sign-in"
    )
}

fn server_suffix(path: &str) -> Option<&str> {
    path.strip_prefix("/v1/servers/")?
        .split_once('/')
        .map(|(_, suffix)| suffix)
}

fn is_server_access_read(path: &str) -> bool {
    matches!(
        server_suffix(path),
        Some("manage/roles" | "manage/roles/capabilities" | "manage/default-access")
    )
}

fn conditional_worker_read(method: &Method, suffix: &str) -> bool {
    *method == Method::GET && matches!(suffix, "manage/content" | "manage/files")
}

fn conditional_read(
    method: &Method,
    path: &str,
    headers: &http::HeaderMap,
    mut reply: Reply,
) -> Reply {
    use http::header::{CONTENT_LENGTH, ETAG, IF_NONE_MATCH};
    use sha2::{Digest, Sha256};

    if *method != Method::GET
        || reply.status() != StatusCode::OK
        || !(matches!(
            path,
            "/v1/host/manage/capabilities"
                | "/v1/host/manage/people"
                | "/v1/host/manage/role-presets"
        ) || (path.starts_with("/v1/host/manage/people/") && path.ends_with("/memberships"))
            || is_server_access_read(path)
            || server_suffix(path).is_some_and(|suffix| conditional_worker_read(method, suffix)))
    {
        return reply;
    }

    // The inherited envelope carries a fresh request ID. Compare only the
    // resource data, after worker URLs and server IDs have been translated.
    let ReplyBody::Bytes(bytes) = reply.body() else {
        return reply;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return reply;
    };
    let Some(data) = value.get("data") else {
        return reply;
    };
    let digest = Sha256::digest(serde_json::to_vec(data).expect("JSON value"));
    let tag = format!("\"{digest:x}\"");
    if headers
        .get_all(IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|value| {
            value.trim() == "*"
                || value
                    .split(',')
                    .map(str::trim)
                    .any(|given| given.strip_prefix("W/").unwrap_or(given) == tag)
        })
    {
        *reply.status_mut() = StatusCode::NOT_MODIFIED;
        *reply.body_mut() = ReplyBody::Bytes(Bytes::new());
        reply.headers_mut().remove(CONTENT_LENGTH);
    }
    reply
        .headers_mut()
        .insert(ETAG, format!("W/{tag}").parse().expect("SHA-256 validator"));
    reply
}
/// Build the complete snapshot, including every active device, in stable order.
fn subject(
    router: &HostRouter,
    worker: &dyn ServerWorker,
    device: &Device,
) -> Result<ServerSubject, Reply> {
    let state = &router.identity.state;
    let unavailable = || failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable");
    let policy = state
        .worker_policy(worker.server_id())
        .map_err(|_| unavailable())?;
    let capabilities = policy
        .grants
        .iter()
        .find(|grant| grant.device_id == device.device_id && grant.account_id == device.account_id)
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "not_found"))?
        .capabilities
        .clone();
    let host_id = policy.host_id;
    let generation = policy.generation;
    worker.sync_policy(policy).map_err(|_| unavailable())?;
    let now = state.now_ms();
    Ok(ServerSubject {
        host_id,
        server_id: worker.server_id(),
        account_id: device.account_id,
        device_id: device.device_id,
        generation,
        issued_at_ms: now,
        expires_at_ms: now + 30_000,
        capabilities,
    })
}
impl Application {
    fn identity_output<T: Serialize>(
        &self,
        result: vesta_control::machine::sessions::Result<T>,
        session_endpoint: bool,
    ) -> Reply {
        if result.as_ref().err().is_some_and(|err| {
            err.0 == "sign_in_rate_limited" || session_endpoint && err.0 == "rate_limited"
        }) {
            return with_retry_after(
                failure(StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
                self.router.identity.retry_after(session_endpoint),
            );
        }
        output(result)
    }

    fn check_device_rate(&self, device: Uuid) -> Result<(), Reply> {
        let decision = self
            .device_limiter
            .lock()
            .map_or(AdmissionDecision::ClockUnavailable, |mut l| {
                l.try_acquire_key(device)
            });
        Self::check_rate(decision)
    }

    fn check_rate(decision: AdmissionDecision) -> Result<(), Reply> {
        match decision {
            AdmissionDecision::Allowed => Ok(()),
            AdmissionDecision::RateLimited { retry_after } => Err(with_retry_after(
                failure(StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
                retry_after,
            )),
            AdmissionDecision::ClockUnavailable | AdmissionDecision::Saturated => {
                Err(failure(StatusCode::SERVICE_UNAVAILABLE, "host_busy"))
            }
        }
    }

    async fn route(self: &Arc<Self>, request: Request<Incoming>, peer: SocketAddr) -> Reply {
        if self.router.is_stopping() {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "host_stopping");
        }
        let decision = self
            .limiter
            .lock()
            .map_or(AdmissionDecision::ClockUnavailable, |mut l| {
                l.try_acquire(peer.ip())
            });
        if let Err(reply) = Self::check_rate(decision) {
            return reply;
        }
        if request.headers().contains_key("cookie") {
            return failure(StatusCode::BAD_REQUEST, "invalid_request");
        }
        // Server pack uploads stream to a host-private file (up to 2 GiB)
        // instead of the bounded in-memory request body.
        if request.method() == Method::POST
            && request.uri().path() == "/v1/host/creates/uploads"
            && request.uri().query().is_none()
        {
            return self.create_upload(request).await;
        }
        if request.method() == Method::GET
            && request.uri().path().starts_with("/v1/servers/")
            && [
                "/console/stream",
                "/files/follow",
                "/metrics/stream",
                "/realtime",
            ]
            .iter()
            .any(|s| request.uri().path().ends_with(s))
        {
            return self.stream(request, peer).await;
        }
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        let jobs = if method == Method::GET
            && (matches!(
                path.as_str(),
                "/v1/host" | "/v1/host/discovery" | "/v1/host/servers"
            ) || is_server_access_read(&path)
                || path.starts_with("/v1/host/manage/"))
        {
            &self.read_jobs
        } else if method == Method::POST && is_enrollment_path(&path) {
            if path.starts_with("/v1/host/sessions") {
                let decision = self
                    .session_peer_limiter
                    .lock()
                    .map_or(AdmissionDecision::ClockUnavailable, |mut l| {
                        l.try_acquire(peer.ip())
                    });
                if let Err(reply) = Self::check_rate(decision) {
                    return reply;
                }
            }
            &self.enrollment_jobs
        } else {
            &self.jobs
        };
        let query = request.uri().query().map(str::to_owned);
        let headers = request.headers().clone();
        let connection = request
            .extensions()
            .get::<Arc<host_pake::Connection>>()
            .cloned();
        let (budget, maximum) = if path.starts_with("/v1/servers/") {
            (&self.bodies, 4 * 1024 * 1024)
        } else {
            (&self.host_bodies, 64 * 1024)
        };
        let reserve = match hyper::body::Body::size_hint(request.body()).exact() {
            Some(size) if size > u64::from(maximum) => {
                return failure(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
            }
            Some(size) => size as u32,
            None => maximum,
        };
        let permit = if reserve == 0 {
            None
        } else {
            match tokio::time::timeout(
                Duration::from_millis(250),
                budget.clone().acquire_many_owned(reserve),
            )
            .await
            {
                Ok(Ok(permit)) => Some(permit),
                _ => return failure(StatusCode::SERVICE_UNAVAILABLE, "transport_busy"),
            }
        };
        let mut body = request.into_body();
        let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(reserve as usize));
        while let Some(frame) = body.frame().await {
            let Ok(frame) = frame else {
                return failure(StatusCode::BAD_REQUEST, "invalid_request");
            };
            if let Ok(data) = frame.into_data() {
                if data.len() > reserve as usize - bytes.len() {
                    return failure(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
                }
                bytes.extend_from_slice(&data);
            } else {
                return failure(StatusCode::BAD_REQUEST, "invalid_request");
            }
        }
        let app = self.clone();
        jobs.execute_for(false, move || {
            let _permit = permit;
            let reply = app.dispatch_sync(
                &method,
                &path,
                query.as_deref(),
                &headers,
                connection,
                peer,
                &bytes,
            );
            conditional_read(&method, &path, &headers, reply)
        })
        .await
    }
    /// `POST /v1/host/creates/uploads`: owner-only raw server pack archive.
    async fn create_upload(self: &Arc<Self>, request: Request<Incoming>) -> Reply {
        let headers = request.headers();
        if headers.get_all("authorization").iter().count() > 1 {
            return failure(StatusCode::UNAUTHORIZED, "invalid_session");
        }
        let Some(token) = routes::bearer(headers) else {
            return failure(StatusCode::UNAUTHORIZED, "invalid_session");
        };
        let token = zeroize::Zeroizing::new(token.to_owned());
        let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        let valid_type = header("content-type") == Some("application/octet-stream");
        let kind = header("x-upload-kind").map(str::to_owned);
        let too_large = hyper::body::Body::size_hint(request.body()).lower()
            > vesta_control::machine::imports::jobs::UPLOAD_MAX_BYTES;
        let (send, receive) = tokio::sync::mpsc::channel(2);
        let app = self.clone();
        let job = self.upload_jobs.run(move || {
            app.create_upload_sync(&token, valid_type, kind.as_deref(), too_large, receive)
        });
        tokio::pin!(job);
        let feed = async move {
            let mut body = request.into_body();
            while let Some(frame) = body.frame().await {
                let Ok(frame) = frame else {
                    return;
                };
                if let Ok(data) = frame.into_data()
                    && send.send(Some(data)).await.is_err()
                {
                    return;
                }
            }
            let _ = send.send(None).await;
        };
        let result = tokio::select! {
            reply = &mut job => reply,
            () = feed => job.await,
        };
        result.unwrap_or_else(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "transport_busy"))
    }

    fn create_upload_sync(
        &self,
        token: &str,
        valid_type: bool,
        kind: Option<&str>,
        too_large: bool,
        mut chunks: tokio::sync::mpsc::Receiver<Option<Bytes>>,
    ) -> Reply {
        let identity = &self.router.identity;
        let Ok(device) = identity.authenticate(token) else {
            return failure(StatusCode::UNAUTHORIZED, "invalid_session");
        };
        if let Err(reply) = self.check_device_rate(device.device_id) {
            return reply;
        }
        if identity
            .state
            .host_role(device.account_id)
            .ok()
            .flatten()
            .is_none()
        {
            return failure(StatusCode::FORBIDDEN, "forbidden");
        }
        if !valid_type {
            return failure(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let Some(kind) = kind else {
            return failure(StatusCode::BAD_REQUEST, "invalid_request");
        };
        if too_large {
            return failure(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
        }
        let (Some(jobs), Some(_)) = (self.router.imports(), self.router.creator.as_ref()) else {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "creates_unavailable");
        };
        let Ok(snapshot) = identity.state.snapshot() else {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
        };
        let binding = OwnerBinding {
            host_id: snapshot.host_id.to_string(),
            account_id: device.account_id.to_string(),
            device_id: device.device_id.to_string(),
        };
        let mut upload = match jobs.begin_create_upload(&binding, kind) {
            Ok(upload) => upload,
            Err(e) if e.code() == "create_jobs_saturated" => {
                let reply = failure(StatusCode::SERVICE_UNAVAILABLE, e.code());
                return match jobs.create_upload_retry_after().ok().flatten() {
                    Some(wait) => with_retry_after(reply, wait),
                    None => reply,
                };
            }
            Err(e) => {
                return failure(
                    match e.code() {
                        "not_supported_yet" => StatusCode::NOT_IMPLEMENTED,
                        "invalid_request" => StatusCode::BAD_REQUEST,
                        "forbidden" => StatusCode::FORBIDDEN,
                        _ => StatusCode::SERVICE_UNAVAILABLE,
                    },
                    e.code(),
                );
            }
        };
        loop {
            let data = match chunks.blocking_recv() {
                Some(Some(data)) => data,
                Some(None) => break,
                None => return failure(StatusCode::BAD_REQUEST, "invalid_request"),
            };
            if let Err(e) = upload.write(&data) {
                return match e.code() {
                    "request_too_large" => failure(StatusCode::PAYLOAD_TOO_LARGE, e.code()),
                    "insufficient_space" => failure(StatusCode::SERVICE_UNAVAILABLE, e.code()),
                    _ => failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
                };
            }
        }
        match upload.finish() {
            Ok(accepted) => {
                let mut reply = routes::json(json!({
                    "uploadId": accepted.upload_id,
                    "expiresAt": chrono::DateTime::from_timestamp_millis(accepted.expires_at as i64)
                        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                }));
                *reply.status_mut() = StatusCode::CREATED;
                reply
            }
            Err(e) if e == JobError::INVALID => failure(StatusCode::BAD_REQUEST, "invalid_request"),
            Err(e) if e == JobError::FORBIDDEN => failure(StatusCode::FORBIDDEN, "forbidden"),
            Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn dispatch_sync(
        &self,
        method: &Method,
        path: &str,
        query: Option<&str>,
        headers: &http::HeaderMap,
        connection: Option<Arc<host_pake::Connection>>,
        peer: SocketAddr,
        bytes: &[u8],
    ) -> Reply {
        if self.router.is_stopping() {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "host_stopping");
        }
        if query.is_some()
            && !path.starts_with("/v1/servers/")
            && path != "/v1/host/catalogue/software"
            && !(method == Method::GET && matches!(path, "/v1/host/servers/removed" | "/v1/here"))
        {
            return failure(StatusCode::BAD_REQUEST, "invalid_request");
        }
        if matches!(*method, Method::GET | Method::HEAD) && !bytes.is_empty() {
            return failure(StatusCode::BAD_REQUEST, "invalid_request");
        }
        if method == Method::GET && path == "/v1/here" {
            return public_access::here(self, query, peer);
        }
        let identity = &self.router.identity;
        let state = &identity.state;
        let h = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        let pake = h("authorization").and_then(|v| v.strip_prefix("Pake "));
        let token = routes::bearer(headers);
        macro_rules! input {
            () => {
                match parse(bytes) {
                    Ok(v) => v,
                    Err(e) => return e,
                }
            };
        }
        if method == Method::GET && path == "/v1/host/discovery" {
            let Ok(s) = state.snapshot() else {
                return failure(StatusCode::SERVICE_UNAVAILABLE, "host_unavailable");
            };
            let mut value = json!({"hostId":s.host_id,"fingerprint":self.tls.fingerprint().unwrap_or_default(),"protocol":"host-v1"});
            if lan(peer.ip()) {
                value["name"] = json!(self.router.display_name());
                value["claimState"] = json!(if s.primary_account_id.is_none() {
                    "unclaimed"
                } else {
                    "claimed"
                });
            }
            return routes::json(value);
        }
        if matches!(*method, Method::GET | Method::HEAD)
            && let Some((id, asset)) = path
                .strip_prefix("/v1/servers/")
                .and_then(|r| r.split_once('/'))
                .filter(|(_, s)| s.starts_with("resource-packs/"))
        {
            let worker = Uuid::parse_str(id)
                .ok()
                .filter(|id| {
                    state.server(*id).ok().flatten().is_some_and(|e| {
                        !matches!(
                            e.state,
                            vesta_control::machine::registry::ServerState::Deleted
                                | vesta_control::machine::registry::ServerState::Disabled
                        )
                    })
                })
                .and_then(|id| {
                    self.router
                        .workers
                        .lock()
                        .ok()?
                        .iter()
                        .find(|w| w.server_id() == id)
                        .cloned()
                });
            if let Some(handle) = worker.and_then(|w| w.public_resource_packs()) {
                return public_resource_packs::dispatch_handle(
                    &handle,
                    method,
                    &format!("/{asset}"),
                    self.downloads.global.clone(),
                );
            }
            return failure(StatusCode::NOT_FOUND, "not_found");
        }
        if headers.get_all("authorization").iter().count() > 1 {
            return failure(StatusCode::UNAUTHORIZED, "invalid_session");
        }
        if method == Method::POST {
            match path {
                "/v1/host/sessions/challenge" => {
                    #[derive(Deserialize)]
                    #[serde(untagged)]
                    enum Challenge {
                        Id(vesta_control::machine::sessions::SessionChallengeRequest),
                        Key(vesta_control::machine::sessions::SessionKeyChallengeRequest),
                    }
                    return match input!() {
                        Challenge::Id(r) => {
                            self.identity_output(identity.session_challenge(r), true)
                        }
                        Challenge::Key(r) => {
                            self.identity_output(identity.session_key_challenge(r), true)
                        }
                    };
                }
                "/v1/host/sessions/key-challenge" => {
                    return self.identity_output(identity.session_key_challenge(input!()), true);
                }
                "/v1/host/sessions" => {
                    return self.identity_output(identity.session_complete(input!()), true);
                }
                _ => {}
            }
            if matches!(
                path,
                "/v1/host/pake/start"
                    | "/v1/host/pake/finish"
                    | "/v1/host/accounts/challenge"
                    | "/v1/host/accounts/complete"
                    | "/v1/host/claim"
                    | "/v1/host/links"
                    | "/v1/host/invites/redeem"
                    | "/v1/host/sign-in"
            ) {
                let Some(c) = connection.as_ref() else {
                    return failure(StatusCode::UNAUTHORIZED, "pairing_failed");
                };
                return match (path, pake) {
                    ("/v1/host/pake/start", _) => {
                        output(identity.pake_start(c, peer.ip().to_canonical(), input!()))
                    }
                    ("/v1/host/pake/finish", _) => output(identity.pake_finish(c, input!())),
                    ("/v1/host/invites/redeem", None) => {
                        let result = identity.redeem_invite(c, peer.ip(), input!());
                        if result.is_ok() {
                            let _ = self.router.refresh_worker_policies(None);
                        }
                        self.identity_output(result, false)
                    }
                    ("/v1/host/sign-in", None) => {
                        let result = identity.sign_in(c, input!());
                        if result.is_ok() {
                            let _ = self.router.refresh_worker_policies(None);
                        }
                        output(result)
                    }
                    ("/v1/host/accounts/challenge", None) => self
                        .identity_output(identity.sign_in_challenge(c, peer.ip(), input!()), false),
                    ("/v1/host/accounts/complete", None) => {
                        output(identity.sign_in_complete(c, input!()))
                    }
                    ("/v1/host/sign-in", Some(_)) => {
                        failure(StatusCode::UNAUTHORIZED, "pairing_failed")
                    }
                    _ => {
                        let Some(p) = pake else {
                            return failure(StatusCode::UNAUTHORIZED, "pairing_failed");
                        };
                        match path {
                            "/v1/host/accounts/challenge" => {
                                output(identity.account_challenge(c, p, input!()))
                            }
                            "/v1/host/accounts/complete" => {
                                output(identity.account_complete(c, p, input!()))
                            }
                            "/v1/host/claim" => output(identity.claim(c, p, input!())),
                            _ => output(identity.submit_link(c, p, input!())),
                        }
                    }
                };
            }
        }
        if method == Method::GET
            && path.starts_with("/v1/host/links/")
            && let Some(pake) = pake
        {
            let Some(c) = connection else {
                return failure(StatusCode::UNAUTHORIZED, "pairing_failed");
            };
            let Ok(id) = Uuid::parse_str(path.trim_start_matches("/v1/host/links/")) else {
                return failure(StatusCode::BAD_REQUEST, "invalid_request");
            };
            return output(identity.link_status(&c, pake, id));
        }
        let Some(raw_token) = token else {
            return failure(StatusCode::UNAUTHORIZED, "invalid_session");
        };
        let ticket = if path.ends_with("/pack/artifact") {
            self.tickets.lock().unwrap().remove(raw_token)
        } else {
            None
        };
        let resolved = if let Some((session, server, generation, until)) = ticket {
            if until <= std::time::Instant::now()
                || state.snapshot().map(|s| s.auth_generation).ok() != Some(generation)
                || !path.starts_with(&format!("/v1/servers/{server}/"))
            {
                return failure(StatusCode::UNAUTHORIZED, "invalid_session");
            }
            Some(session)
        } else {
            None
        };
        let token = resolved.as_ref().map(|s| s.as_str()).unwrap_or(raw_token);
        let device = match identity.authenticate(token) {
            Ok(d) => d,
            Err(_) => return failure(StatusCode::UNAUTHORIZED, "invalid_session"),
        };
        if let Err(reply) = self.check_device_rate(device.device_id) {
            return reply;
        }
        if method == Method::GET && path == "/v1/host/realtime" {
            return failure(StatusCode::NOT_IMPLEMENTED, "realtime_multiplexer_deferred");
        }
        if method == Method::DELETE && path == "/v1/host/sessions/current" {
            return match identity.logout(token) {
                Ok(()) => no_content(),
                Err(e) => failure(StatusCode::UNAUTHORIZED, e.0),
            };
        }
        if method == Method::GET && path == "/v1/host/servers" {
            self.router.refresh_server_states();
            return match state.visible_servers(device.account_id) {
                Ok(entries) => routes::json(
                    json!({"servers":self.router.server_cards(entries),"nextCursor":null}),
                ),
                Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "host_unavailable"),
            };
        }
        if path == "/v1/host/servers/removed" || path.starts_with("/v1/host/servers/removed/") {
            return removed::removed_routes(
                &self.router,
                &device,
                method,
                path,
                query,
                headers,
                bytes,
            );
        }
        if method == Method::DELETE && path.starts_with("/v1/host/servers/") && query.is_none() {
            return removed::dispatch(&self.router, &device, path, bytes);
        }
        if let Some(rest) = path.strip_prefix("/v1/servers/") {
            // Serialize admission with removal: no request can re-expand worker
            // grants or enqueue a new job while its worker is being stopped.
            let access_read =
                method == Method::GET && query.is_none() && is_server_access_read(path);
            let _removal = if access_read {
                None
            } else {
                match self.router.removal_lock.lock() {
                    Ok(lock) => Some(lock),
                    Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
                }
            };
            let Some((id, suffix)) = rest.split_once('/') else {
                return failure(StatusCode::NOT_FOUND, "not_found");
            };
            let Ok(id) = Uuid::parse_str(id) else {
                return failure(StatusCode::NOT_FOUND, "not_found");
            };
            if !state
                .effective_role(device.account_id, id)
                .is_ok_and(|r| r.is_some())
            {
                return failure(StatusCode::NOT_FOUND, "not_found");
            }
            if suffix == "manage/enabled" {
                return enablement::dispatch(&self.router, &device, id, method, query, bytes);
            }
            if suffix == "manage/default-access" {
                return public_access::settings(&self.router, &device, id, method, query, bytes);
            }
            if let Some(reply) = access::dispatch(
                &self.router,
                &device,
                id,
                method,
                suffix,
                query,
                bytes,
                &self.tls.fingerprint().unwrap_or_default(),
            ) {
                return reply;
            }
            let worker = self
                .router
                .workers
                .lock()
                .ok()
                .and_then(|w| w.iter().find(|w| w.server_id() == id).cloned());
            let entry = match state.server(id) {
                Ok(Some(entry)) => entry,
                _ => return failure(StatusCode::SERVICE_UNAVAILABLE, "host_unavailable"),
            };
            let disabled = entry.state == vesta_control::machine::registry::ServerState::Disabled;
            let worker = worker.filter(|_| !disabled).or_else(|| {
                self.router
                    .import_worker(id)
                    .ok()
                    .map(|w| Arc::new(w) as Arc<dyn ServerWorker>)
            });
            if suffix == "manage/startup" {
                return startup::dispatch(
                    state,
                    &device,
                    id,
                    worker.as_deref(),
                    method,
                    query,
                    bytes,
                );
            }
            if disabled {
                return failure(StatusCode::CONFLICT, "server_disabled");
            }
            let Some(worker) = worker else {
                return failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable");
            };
            let subject = match subject(&self.router, worker.as_ref(), &device) {
                Ok(s) => s,
                Err(e) => return e,
            };
            if suffix == "manage/storage" && self.router.imports().is_some() {
                use vesta_control::identity::Capability;
                if method != Method::GET || query.is_some() || !bytes.is_empty() {
                    return crate::manage::error(StatusCode::BAD_REQUEST, "invalid_request");
                }
                let worker_host = match worker.authorize(subject) {
                    Ok(host) => host,
                    Err(error) => return crate::manage::management_error(error),
                };
                if let Err(error) = worker_host.management().authorize(
                    "trusted-worker",
                    Capability::StorageView,
                    None,
                ) {
                    return crate::manage::management_error(error);
                }
                return self.router.storage.server_details(id);
            }
            if suffix == "pack/ticket" && *method == Method::POST {
                let host = match worker.authorize(subject.clone()) {
                    Ok(host) => host,
                    Err(error) => return crate::manage::management_error(error),
                };
                let allowed = host.with_policy(|policy| {
                    host.sessions().authenticate(
                        "trusted-worker",
                        std::time::Instant::now(),
                        policy,
                    )
                });
                if !matches!(allowed, Ok(Ok(_))) {
                    return failure(StatusCode::FORBIDDEN, "pack_access_denied");
                }
                use vesta_host_client::rand_core::RngCore;
                let mut random = [0; 32];
                vesta_host_client::rand_core::OsRng.fill_bytes(&mut random);
                let ticket = vesta_host_client::encode(&random);
                let mut tickets = self.tickets.lock().unwrap();
                tickets.retain(|_, (_, _, _, until)| *until > std::time::Instant::now());
                if tickets.len() >= 1024 {
                    return failure(StatusCode::SERVICE_UNAVAILABLE, "host_busy");
                }
                tickets.insert(
                    ticket.clone(),
                    (
                        zeroize::Zeroizing::new(token.to_owned()),
                        id,
                        subject.generation,
                        std::time::Instant::now() + Duration::from_secs(30),
                    ),
                );
                return routes::json(
                    json!({"ticket":ticket,"artifactUrl":format!("/v1/servers/{id}/pack/artifact")}),
                );
            }
            // Changing server-port in the server's settings: the port must be free on this
            // machine (no other server holds it, nothing is listening), and the registry,
            // which allocates ports for new servers, follows the change.
            let new_port = (suffix == "manage/config/server-properties"
                && *method == Method::PATCH)
                .then(|| serde_json::from_slice::<serde_json::Value>(bytes).ok())
                .flatten()
                .and_then(|body| {
                    let value = &body["changes"]["server-port"];
                    value
                        .as_u64()
                        .or_else(|| value.as_str()?.trim().parse().ok())
                })
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port > 0);
            let new_port = match new_port {
                Some(port) => match state.server(id) {
                    Ok(Some(entry)) if entry.game_port == port => None,
                    Ok(Some(_)) => {
                        if state.game_port_taken(id, port).unwrap_or(true) {
                            return port_conflict("game_port_in_use");
                        }
                        let probe = vesta_control::machine::imports::inspection::port_state(
                            port,
                            "tcp",
                            &std::collections::BTreeSet::new(),
                        );
                        match probe.state {
                            "free" => Some(port),
                            "occupied" => return port_conflict("game_port_in_use"),
                            _ => return port_conflict("port_state_unknown"),
                        }
                    }
                    _ => None,
                },
                None => None,
            };
            let mut reply = crate::worker::dispatch_host(
                worker.as_ref(),
                subject,
                crate::worker::WorkerRequest {
                    method,
                    path: suffix,
                    query,
                    body: bytes,
                    idempotency_key: h("idempotency-key"),
                    if_match: h("if-match"),
                    if_none_match: if conditional_worker_read(method, suffix) {
                        None
                    } else {
                        h("if-none-match")
                    },
                    range: h("range"),
                    if_range: h("if-range"),
                },
                &entry.name,
                self.tls.clone(),
                peer.ip(),
                self.downloads.clone(),
            );
            // A source-only branding change also works before a custom name is set.
            if suffix == "manage/server"
                && reply.status() == StatusCode::OK
                && let ReplyBody::Bytes(bytes) = reply.body_mut()
                && let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(bytes)
                && let Some(branding) = value["data"].as_object_mut()
                && branding.get("name").is_some_and(serde_json::Value::is_null)
            {
                branding.insert("name".into(), json!(entry.name));
                *bytes = serde_json::to_vec(&value).expect("JSON value").into();
                let size = bytes.len();
                reply.headers_mut().insert("content-length", size.into());
            }
            if let Some(port) = new_port {
                // 202 means the edit still awaits confirmation; only a finished edit moves the port.
                if reply.status() == StatusCode::OK {
                    // A race with another server taking the port leaves the registry as it was;
                    // the next start reports the clash.
                    let _ = state.observe_game_port(id, port);
                }
            }
            return reply;
        }
        use vesta_control::identity::Capability;
        let actor = Actor::Account(device.account_id);
        if let Some(suffix) = path.strip_prefix("/v1/host/manage/") {
            if method == Method::GET {
                return host_access::dispatch(&self.router, actor, method, suffix, query, bytes);
            }
            let _admission = match self.router.removal_lock.lock() {
                Ok(lock) => lock,
                Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            };
            return host_access::dispatch(&self.router, actor, method, suffix, query, bytes);
        }
        let owner = state.host_role(device.account_id).ok().flatten();
        let host_caps = match state.host_capabilities(actor) {
            Ok(caps) => caps,
            Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "host_unavailable"),
        };
        if method == Method::GET && path == "/v1/host" {
            if !host_caps.contains(&Capability::HostView) {
                return failure(StatusCode::FORBIDDEN, "forbidden");
            }
            let Ok(s) = state.snapshot() else {
                return failure(StatusCode::SERVICE_UNAVAILABLE, "host_unavailable");
            };
            let mut capabilities = vec![
                "host-access",
                "role-presets",
                "server-enablement",
                "public-access",
                "folders",
                "import-inspect",
                "resource-plan",
                "removed-server-retention",
                "console-history",
            ];
            if self.router.imports.is_some() {
                capabilities.extend(["removed-servers", "server-purge", "storage"]);
                capabilities.push("import-jobs");
                // Removal needs the job store (busy checks, servers root); listed so launchers
                // can tell an older machine (no route) from a server that is already gone (404).
                capabilities.push("server-remove");
                if self.router.creator.is_some() && self.router.catalogue.is_some() {
                    capabilities.push("create-jobs");
                }
            }
            if self.router.imports.is_some() {
                capabilities.extend(["server-storage", "storage-cleanup"]);
            }
            let mut value = json!({"hostId":s.host_id,"name":self.router.display_name(),"fingerprint":self.tls.fingerprint().unwrap_or_default(),"endpoints":[],"authGeneration":s.auth_generation.to_string(),"revision":s.revision.to_string(),"role":owner.map(host_role_name),"capabilities":capabilities});
            value["hostCapabilities"] =
                json!(host_caps.iter().map(|cap| cap.id()).collect::<Vec<_>>());
            if host_caps.contains(&Capability::HostSettingsEdit) {
                let Ok(settings) = state.settings() else {
                    return failure(StatusCode::SERVICE_UNAVAILABLE, "host_unavailable");
                };
                value["newDeviceSignIn"] = json!(settings.new_device_sign_in);
                value["removedServerRetentionDays"] = json!(settings.removed_server_retention_days);
                value["defaultOnAgentStart"] = json!(settings.default_on_agent_start);
            }
            return routes::json(value);
        }
        let delegated = (method == Method::PATCH
            && path == "/v1/host"
            && host_caps.contains(&Capability::HostSettingsEdit))
            || ((path.starts_with("/v1/host/catalogue/") || path.starts_with("/v1/host/creates/"))
                && host_caps.contains(&Capability::HostServersCreate))
            || (method == Method::GET
                && path == "/v1/host/devices"
                && (host_caps.contains(&Capability::HostAccessManage)
                    || host_caps.contains(&Capability::DevicesRevoke)))
            || (method == Method::DELETE
                && path.starts_with("/v1/host/devices/")
                && host_caps.contains(&Capability::DevicesRevoke));
        if owner.is_none() && !delegated {
            return failure(StatusCode::FORBIDDEN, "forbidden");
        }
        if method == Method::POST && path == "/v1/host/storage/cleanup" {
            return storage::cleanup(&self.router, &device, query, headers, bytes);
        }
        if path == "/v1/host/storage" || path.starts_with("/v1/host/storage/") {
            return storage::dispatch(&self.router, method, path, bytes);
        }
        if path.starts_with("/v1/host/catalogue/") || path.starts_with("/v1/host/creates/") {
            return creates::dispatch(&self.router, &device, method, path, query, headers, bytes);
        }
        if method == Method::PATCH && path == "/v1/host" {
            return match state.update_settings(Actor::Account(device.account_id), input!()) {
                Ok(settings) => match state.snapshot() {
                    Ok(s) => routes::json(
                        json!({"hostId":s.host_id,"name":self.router.display_name(),"newDeviceSignIn":settings.new_device_sign_in,"removedServerRetentionDays":settings.removed_server_retention_days,"defaultOnAgentStart":settings.default_on_agent_start,"revision":s.revision.to_string()}),
                    ),
                    Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "host_unavailable"),
                },
                Err(e) if e.code() == "invalid_request" => {
                    failure(StatusCode::BAD_REQUEST, "invalid_request")
                }
                Err(_) => failure(StatusCode::FORBIDDEN, "forbidden"),
            };
        }
        if method == Method::GET && path == "/v1/host/owners" {
            return match state.owners(Actor::Account(device.account_id)) {
                Ok(owners) => routes::json(
                    json!({"owners":owners.into_iter().map(|o|json!({"accountId":o.account_id,"role":host_role_name(o.role),"name":o.name})).collect::<Vec<_>>()}),
                ),
                Err(_) => failure(StatusCode::FORBIDDEN, "forbidden"),
            };
        }
        if let Some(reply) =
            self.ownership
                .dispatch(&self.router, &device, method, path, headers, bytes)
        {
            return reply;
        }
        if path.starts_with("/v1/host/folders/") || path.starts_with("/v1/host/imports/") {
            return imports::dispatch(&self.router, &device, method, path, headers, bytes);
        }
        if method == Method::GET && path == "/v1/host/devices" {
            return match state.devices(Actor::Account(device.account_id)) {
                Ok(devices) => routes::json(
                    json!({"devices":devices.into_iter().map(|d|json!({"deviceId":d.device_id,"accountId":d.account_id,"name":d.name,"revoked":d.revoked_at_ms.is_some(),"createdAt":chrono::DateTime::from_timestamp_millis(d.created_at_ms).map(|d|d.to_rfc3339())})).collect::<Vec<_>>(),"nextCursor":null}),
                ),
                Err(_) => failure(StatusCode::FORBIDDEN, "forbidden"),
            };
        }
        if method == Method::DELETE && path.starts_with("/v1/host/devices/") {
            let Ok(id) = Uuid::parse_str(path.trim_start_matches("/v1/host/devices/")) else {
                return failure(StatusCode::BAD_REQUEST, "invalid_request");
            };
            return match state.revoke_device(Actor::Account(device.account_id), id) {
                Ok(_) => self
                    .router
                    .refresh_worker_policies(None)
                    .map_or_else(|reply| reply, |_| no_content()),
                Err(_) => failure(StatusCode::FORBIDDEN, "forbidden"),
            };
        }
        if method == Method::POST && path == "/v1/host/link-codes" {
            return output(identity.owner_link_code(token, input!()));
        }
        if let Some(rest) = path.strip_prefix("/v1/host/links/") {
            let (id, action) = rest.split_once('/').unwrap_or((rest, ""));
            let Ok(id) = Uuid::parse_str(id) else {
                return failure(StatusCode::BAD_REQUEST, "invalid_request");
            };
            if method == Method::GET && action.is_empty() {
                return output(identity.owner_link_status(token, id));
            }
            if method == Method::POST && action == "approve" {
                return output(identity.owner_approve_link(token, id, input!()));
            }
        }
        failure(StatusCode::NOT_FOUND, "not_found")
    }
}
fn lan(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => v.is_loopback() || v.is_private() || v.is_link_local(),
        std::net::IpAddr::V6(v) => {
            v.is_loopback() || v.is_unique_local() || v.is_unicast_link_local()
        }
    }
}

impl Application {
    async fn stream(self: &Arc<Self>, mut request: Request<Incoming>, peer: SocketAddr) -> Reply {
        if request.headers().get_all("authorization").iter().count() != 1 {
            return failure(StatusCode::UNAUTHORIZED, "invalid_session");
        }
        let token = match routes::bearer(request.headers()) {
            Some(t) => t.to_owned(),
            None => return failure(StatusCode::UNAUTHORIZED, "invalid_session"),
        };
        let rest = request
            .uri()
            .path()
            .strip_prefix("/v1/servers/")
            .unwrap_or("");
        let Some((id, suffix)) = rest.split_once('/') else {
            return failure(StatusCode::NOT_FOUND, "not_found");
        };
        if !matches!(
            suffix,
            "manage/console/stream" | "manage/files/follow" | "manage/metrics/stream" | "realtime"
        ) {
            return failure(StatusCode::NOT_FOUND, "not_found");
        }
        let Ok(id) = Uuid::parse_str(id) else {
            return failure(StatusCode::NOT_FOUND, "not_found");
        };
        let app = self.clone();
        let session = token.clone();
        let admitted = self
            .jobs
            .run(move || {
                let router = &app.router;
                let d = router
                    .identity
                    .authenticate(&session)
                    .map_err(|_| failure(StatusCode::UNAUTHORIZED, "invalid_session"))?;
                app.check_device_rate(d.device_id)?;
                if !router
                    .identity
                    .state
                    .effective_role(d.account_id, id)
                    .is_ok_and(|r| r.is_some())
                {
                    return Err(failure(StatusCode::NOT_FOUND, "not_found"));
                }
                if router.identity.state.server(id).is_ok_and(|entry| {
                    entry.is_some_and(|entry| {
                        entry.state == vesta_control::machine::registry::ServerState::Disabled
                    })
                }) {
                    return Err(failure(StatusCode::CONFLICT, "server_disabled"));
                }
                let w = router
                    .workers
                    .lock()
                    .ok()
                    .and_then(|w| w.iter().find(|w| w.server_id() == id).cloned())
                    .ok_or_else(|| {
                        failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable")
                    })?;
                let s = subject(router, w.as_ref(), &d)?;
                let generation = s.generation;
                w.authorize(s)
                    .map(|host| (host, generation, d.account_id))
                    .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable"))
            })
            .await;
        let (host, generation, account) = match admitted {
            Ok(Ok(h)) => h,
            Ok(Err(e)) => return e,
            _ => return failure(StatusCode::SERVICE_UNAVAILABLE, "host_busy"),
        };
        let pack = suffix == "realtime";
        let query = request
            .uri()
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        *request.uri_mut() = if pack {
            format!("/api/v2/realtime{query}")
        } else {
            format!("/api/v3/{suffix}{query}")
        }
        .parse()
        .unwrap();
        request.headers_mut().insert(
            "authorization",
            http::HeaderValue::from_static("Bearer host-trusted-subject-marker-000000000000000"),
        );
        let (stop, stopped) = watch::channel(false);
        let runtime = tokio::runtime::Handle::current();
        let downloads = Arc::new(Downloads::new(DOWNLOADS));
        let shared = Arc::new(Shared {
            host,
            storage: Default::default(),
            tls: self.tls.clone(),
            max_body: 4 * 1024 * 1024,
            bodies: self.bodies.clone(),
            control_bodies: Arc::new(Semaphore::new(CONTROL_BODY_BUDGET)),
            timeout: self.timeout,
            limiter: Mutex::new(PeerRateLimiter::new(
                RatePolicy::new(120, Duration::from_secs(60), 1024, Duration::from_secs(120))
                    .unwrap(),
            )),
            session_limiter: Mutex::new(RateLimiter::new(
                RatePolicy::new(120, Duration::from_secs(60), 4096, Duration::from_secs(120))
                    .unwrap(),
            )),
            jobs: BlockingJobs::new(runtime.clone()),
            control_jobs: BlockingJobs::new(runtime.clone()),
            realtime_jobs: BlockingJobs::new(runtime.clone()),
            install_jobs: BlockingJobs::new(runtime),
            public_downloads: downloads.global.clone(),
            downloads,
            realtime: realtime::Sockets::new(),
            stop: stopped,
        });
        let reservation = Arc::new(());
        {
            let mut streams = self.streams.lock().unwrap();
            streams.retain(|stream| stream.reservation.strong_count() != 0);
            if streams.len() >= 32 {
                return failure(StatusCode::SERVICE_UNAVAILABLE, "realtime_busy");
            }
            let peer = peer.ip().to_canonical();
            if streams
                .iter()
                .filter(|stream| stream.account == account)
                .count()
                >= 8
                || streams.iter().filter(|stream| stream.peer == peer).count() >= 12
            {
                return failure(StatusCode::SERVICE_UNAVAILABLE, "realtime_busy");
            }
            streams.push(HostStream {
                stop: stop.clone(),
                shared: shared.clone(),
                account,
                peer,
                reservation: Arc::downgrade(&reservation),
            });
        }
        if pack {
            let ticket = shared.host.with_policy(|p| {
                shared.host.sessions().issue_ticket_for(
                    "host-trusted-subject-marker-000000000000000",
                    vesta_control::pack_access::TicketPurpose::Realtime,
                    std::time::Instant::now(),
                    p,
                )
            });
            let Ok(Ok(ticket)) = ticket else {
                return failure(StatusCode::FORBIDDEN, "pack_access_denied");
            };
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {}", ticket.expose()).parse().unwrap(),
            );
        }
        let reply = realtime::upgrade(request, shared, Some(reservation)).await;
        if reply.status() != StatusCode::SWITCHING_PROTOCOLS {
            return reply;
        }
        // Host revocation closes streams; short worker subject expiry also bounds
        // them. H07 replaces this polling seam with the revocation barrier.
        let identity = self.router.identity.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            let until = tokio::time::Instant::now() + Duration::from_secs(29);
            loop {
                tick.tick().await;
                if tokio::time::Instant::now() >= until
                    || identity.authenticate(&token).is_err()
                    || !identity
                        .state
                        .snapshot()
                        .is_ok_and(|s| s.auth_generation == generation)
                {
                    let _ = stop.send(true);
                    break;
                }
                if stop.is_closed() {
                    break;
                }
            }
        });
        reply
    }
}

#[cfg(test)]
mod tests;
