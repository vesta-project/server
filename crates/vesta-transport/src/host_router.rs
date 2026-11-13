//! Host authority and HTTP routing. No legacy bearer is accepted here.
use super::*;
use crate::host_identity::HostIdentity;
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;
use vesta_control::{
    identity::BuiltinRole,
    machine::{
        imports::{
            FolderBrowser, OwnerBinding,
            jobs::{ImportJobs, JobError},
        },
        owners::{Actor, Device, EffectiveRole, ServerRole},
    },
    worker::{ServerSubject, ServerWorker, Worker, WorkerGrant, WorkerHandle, WorkerPolicy},
};
mod imports;
mod owners;

/// Builds the in-process worker for a registered imported server.
pub type WorkerFactory = dyn Fn(Uuid) -> Result<Worker, JobError> + Send + Sync;
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
        }
    }
    /// Enable the import job routes. `create` builds a registered server's
    /// worker (normally `ImportJobs::create_worker` with the host clock).
    pub fn with_imports(
        mut self,
        jobs: ImportJobs,
        create: impl Fn(Uuid) -> Result<Worker, JobError> + Send + Sync + 'static,
    ) -> Self {
        self.imports = Some(Arc::new(ImportService {
            jobs,
            create: Box::new(create),
        }));
        self
    }
    pub fn imports(&self) -> Option<&ImportJobs> {
        self.imports.as_deref().map(|i| &i.jobs)
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
        let imports = self.imports.as_ref().ok_or(JobError::UNAVAILABLE)?;
        let mut owned = self.owned.lock().map_err(|_| JobError::UNAVAILABLE)?;
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
        let worker = (imports.create)(server_id)?;
        let handle = worker.handle();
        self.add_worker(Arc::new(handle.clone()));
        owned.push(worker);
        Ok(handle)
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
    bodies: Arc<Semaphore>,
    jobs: BlockingJobs,
    limiter: Mutex<PeerRateLimiter>,
    tickets: Mutex<
        std::collections::HashMap<
            String,
            (zeroize::Zeroizing<String>, Uuid, u64, std::time::Instant),
        >,
    >,
    downloads: Arc<Downloads<vesta_control::pack_access::RealtimeSession>>,
    ownership: owners::Challenges,
    streams: Mutex<Vec<(watch::Sender<bool>, Arc<Shared>)>>,
}
impl Transport {
    pub fn start_host(router: HostRouter, listener: &TlsListener) -> Result<Self, TransportError> {
        pending::start_small_application(listener, None, move |tls, config| {
            let app = Arc::new(Application {
                downloads: Arc::new(Downloads::new(DOWNLOADS)),
                tickets: Default::default(),
                streams: Default::default(),
                ownership: Default::default(),
                router,
                tls,
                timeout: Duration::from_secs(config.request_timeout_seconds().into()),
                bodies: Arc::new(Semaphore::new(REQUEST_BODY_BUDGET)),
                jobs: BlockingJobs::with_workers(tokio::runtime::Handle::current(), 2),
                limiter: Mutex::new(PeerRateLimiter::new(
                    RatePolicy::new(
                        config.peer_requests_per_minute(),
                        Duration::from_secs(60),
                        1024,
                        Duration::from_secs(120),
                    )
                    .expect("validated policy"),
                )),
            });
            // Registry state follows observed worker state while the host runs.
            let weak = Arc::downgrade(&app);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(2));
                loop {
                    tick.tick().await;
                    let Some(router) = weak.upgrade().map(|a| a.router.clone()) else {
                        break;
                    };
                    let _ = tokio::task::spawn_blocking(move || router.refresh_server_states())
                        .await;
                }
            });
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
    async fn reap(&self, all: bool) {
        let streams = {
            let mut streams = self.streams.lock().unwrap();
            if all {
                for (stop, _) in streams.iter() {
                    let _ = stop.send(true);
                }
                std::mem::take(&mut *streams)
            } else {
                Vec::new()
            }
        };
        for (_, s) in streams {
            s.reap(true).await;
        }
        self.jobs.reap(all).await;
    }
    async fn dispatch(
        self: Arc<Self>,
        request: Request<Incoming>,
        peer: SocketAddr,
    ) -> Result<Reply, Infallible> {
        Ok(self.route(request, peer).await)
    }
}
fn failure(status: StatusCode, code: &str) -> Reply {
    let mut reply = routes::json(json!({"error":{"code":code,"requestId":Uuid::new_v4()}}));
    *reply.status_mut() = status;
    reply
}
fn output<T: Serialize>(result: vesta_control::machine::sessions::Result<T>) -> Reply {
    match result {
        Ok(value) => routes::json(serde_json::to_value(value).expect("wire response")),
        Err(e) => failure(
            if e.0.ends_with("unavailable") {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::UNAUTHORIZED
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
fn role_name(role: EffectiveRole) -> &'static str {
    match role {
        EffectiveRole::PrimaryOwner => "primary_owner",
        EffectiveRole::Owner => "owner",
        EffectiveRole::Server(ServerRole::Admin) => "admin",
        EffectiveRole::Server(ServerRole::Moderator) => "moderator",
        EffectiveRole::Server(ServerRole::Player) => "player",
    }
}
fn grant_role(role: EffectiveRole) -> (vesta_control::worker::ServerRole, BuiltinRole) {
    use vesta_control::worker::ServerRole as W;
    match role {
        EffectiveRole::PrimaryOwner | EffectiveRole::Owner => (W::Owner, BuiltinRole::Owner),
        EffectiveRole::Server(ServerRole::Admin) => (W::Admin, BuiltinRole::Administrator),
        EffectiveRole::Server(ServerRole::Moderator) => (W::Moderator, BuiltinRole::Moderator),
        EffectiveRole::Server(ServerRole::Player) => (W::Player, BuiltinRole::PackUser),
    }
}
/// Build the complete snapshot, including every active device, in stable order.
fn subject(
    router: &HostRouter,
    worker: &dyn ServerWorker,
    device: &Device,
) -> Result<ServerSubject, Reply> {
    let state = &router.identity.state;
    let unavailable = || failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable");
    let snap = state.snapshot().map_err(|_| unavailable())?;
    let role = state
        .effective_role(device.account_id, worker.server_id())
        .map_err(|_| unavailable())?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "not_found"))?;
    let devices = state
        .devices(Actor::LocalRecovery)
        .map_err(|_| unavailable())?;
    let mut grants = Vec::new();
    for d in devices.into_iter().filter(|d| d.revoked_at_ms.is_none()) {
        if let Some(role) = state
            .effective_role(d.account_id, worker.server_id())
            .map_err(|_| unavailable())?
        {
            let (role, builtin) = grant_role(role);
            grants.push(WorkerGrant {
                account_id: d.account_id,
                device_id: d.device_id,
                role,
                capabilities: builtin
                    .capabilities()
                    .iter()
                    .filter(|c| !c.local_only())
                    .map(|c| c.id().into())
                    .collect(),
            });
        }
    }
    worker
        .sync_policy(WorkerPolicy {
            host_id: snap.host_id,
            server_id: worker.server_id(),
            generation: snap.auth_generation,
            grants,
        })
        .map_err(|_| unavailable())?;
    let now = state.now_ms();
    Ok(ServerSubject {
        host_id: snap.host_id,
        server_id: worker.server_id(),
        account_id: device.account_id,
        device_id: device.device_id,
        generation: snap.auth_generation,
        issued_at_ms: now,
        expires_at_ms: now + 30_000,
        capabilities: grant_role(role)
            .1
            .capabilities()
            .iter()
            .filter(|c| !c.local_only())
            .map(|c| c.id().into())
            .collect(),
    })
}
impl Application {
    async fn route(self: &Arc<Self>, request: Request<Incoming>, peer: SocketAddr) -> Reply {
        if !self
            .limiter
            .lock()
            .is_ok_and(|mut l| matches!(l.try_acquire(peer.ip()), AdmissionDecision::Allowed))
        {
            return with_retry_after(
                failure(StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
                Duration::from_secs(1),
            );
        }
        if request.headers().contains_key("cookie") {
            return failure(StatusCode::BAD_REQUEST, "invalid_request");
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
        let query = request.uri().query().map(str::to_owned);
        let headers = request.headers().clone();
        let connection = request
            .extensions()
            .get::<Arc<host_pake::Connection>>()
            .cloned();
        let maximum = if path.starts_with("/v1/servers/") {
            4 * 1024 * 1024
        } else {
            64 * 1024
        };
        let reserve = if request.body().is_end_stream() {
            0
        } else {
            maximum
        };
        let Ok(permit) = self.bodies.clone().try_acquire_many_owned(reserve) else {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "transport_busy");
        };
        let mut body = request.into_body();
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        while let Some(frame) = body.frame().await {
            let Ok(frame) = frame else {
                return failure(StatusCode::BAD_REQUEST, "invalid_request");
            };
            if let Ok(data) = frame.into_data() {
                if data.len() > maximum as usize - bytes.len() {
                    return failure(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
                }
                bytes.extend_from_slice(&data);
            } else {
                return failure(StatusCode::BAD_REQUEST, "invalid_request");
            }
        }
        let app = self.clone();
        self.jobs
            .execute_for(false, move || {
                let _permit = permit;
                app.dispatch_sync(
                    &method,
                    &path,
                    query.as_deref(),
                    &headers,
                    connection,
                    peer,
                    &bytes,
                )
            })
            .await
    }
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
        if query.is_some() && !path.starts_with("/v1/servers/") {
            return failure(StatusCode::BAD_REQUEST, "invalid_request");
        }
        if matches!(*method, Method::GET | Method::HEAD) && !bytes.is_empty() {
            return failure(StatusCode::BAD_REQUEST, "invalid_request");
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
                value["name"] = json!(self.router.name);
                value["claimState"] = json!(if s.primary_account_id.is_none() {
                    "unclaimed"
                } else {
                    "claimed"
                });
            }
            return routes::json(value);
        }
        if matches!(*method, Method::GET | Method::HEAD) {
            if let Some((id, asset)) = path
                .strip_prefix("/v1/servers/")
                .and_then(|r| r.split_once('/'))
                .filter(|(_, s)| s.starts_with("resource-packs/"))
            {
                let worker = Uuid::parse_str(id)
                    .ok()
                    .filter(|id| {
                        state.server(*id).ok().flatten().is_some_and(|e| {
                            e.state != vesta_control::machine::registry::ServerState::Deleted
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
                        Challenge::Id(r) => output(identity.session_challenge(r)),
                        Challenge::Key(r) => output(identity.session_key_challenge(r)),
                    };
                }
                "/v1/host/sessions/key-challenge" => {
                    return output(identity.session_key_challenge(input!()));
                }
                "/v1/host/sessions" => return output(identity.session_complete(input!())),
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
            ) {
                let Some(c) = connection.as_ref() else {
                    return failure(StatusCode::UNAUTHORIZED, "pairing_failed");
                };
                return match path {
                    "/v1/host/pake/start" => output(identity.pake_start(c, peer.ip(), input!())),
                    "/v1/host/pake/finish" => output(identity.pake_finish(c, input!())),
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
        if method == Method::GET && path.starts_with("/v1/host/links/") && pake.is_some() {
            let Some(c) = connection else {
                return failure(StatusCode::UNAUTHORIZED, "pairing_failed");
            };
            let Ok(id) = Uuid::parse_str(path.trim_start_matches("/v1/host/links/")) else {
                return failure(StatusCode::BAD_REQUEST, "invalid_request");
            };
            return output(identity.link_status(&c, pake.unwrap(), id));
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
                    json!({"servers":entries.into_iter().map(|(e,r)|json!({"serverId":e.server_id,"name":e.name,"gamePort":e.game_port,"state":format!("{:?}",e.state).to_lowercase(),"role":role_name(r)})).collect::<Vec<_>>(),"nextCursor":null}),
                ),
                Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "host_unavailable"),
            };
        }
        if let Some(rest) = path.strip_prefix("/v1/servers/") {
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
            let worker = self
                .router
                .workers
                .lock()
                .ok()
                .and_then(|w| w.iter().find(|w| w.server_id() == id).cloned());
            let Some(worker) = worker else {
                return failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable");
            };
            let subject = match subject(&self.router, worker.as_ref(), &device) {
                Ok(s) => s,
                Err(e) => return e,
            };
            if suffix == "pack/ticket" && *method == Method::POST {
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
            return crate::worker::dispatch_host(
                worker.as_ref(),
                subject,
                crate::worker::WorkerRequest {
                    method,
                    path: suffix,
                    query,
                    body: bytes,
                    idempotency_key: h("idempotency-key"),
                    if_match: h("if-match"),
                    if_none_match: h("if-none-match"),
                    range: h("range"),
                    if_range: h("if-range"),
                },
                self.tls.clone(),
                peer.ip(),
                self.downloads.clone(),
            );
        }
        let owner = state.host_role(device.account_id).ok().flatten();
        if method == Method::GET && path == "/v1/host" {
            let Ok(s) = state.snapshot() else {
                return failure(StatusCode::SERVICE_UNAVAILABLE, "host_unavailable");
            };
            let mut capabilities = vec!["folders", "import-inspect", "resource-plan"];
            if self.router.imports.is_some() {
                capabilities.push("import-jobs");
            }
            return routes::json(
                json!({"hostId":s.host_id,"name":self.router.name,"fingerprint":self.tls.fingerprint().unwrap_or_default(),"endpoints":[],"authGeneration":s.auth_generation.to_string(),"revision":s.revision.to_string(),"role":owner.map(|r|if r==vesta_control::machine::owners::HostRole::PrimaryOwner{"primary_owner"}else{"owner"}),"capabilities":capabilities}),
            );
        }
        if owner.is_none() {
            return failure(StatusCode::FORBIDDEN, "forbidden");
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
                Ok(_) => no_content(),
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
    async fn stream(self: &Arc<Self>, mut request: Request<Incoming>, _peer: SocketAddr) -> Reply {
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
        let router = self.router.clone();
        let session = token.clone();
        let admitted = self
            .jobs
            .run(move || {
                let d = router
                    .identity
                    .authenticate(&session)
                    .map_err(|_| failure(StatusCode::UNAUTHORIZED, "invalid_session"))?;
                if !router
                    .identity
                    .state
                    .effective_role(d.account_id, id)
                    .is_ok_and(|r| r.is_some())
                {
                    return Err(failure(StatusCode::NOT_FOUND, "not_found"));
                }
                let w = router
                    .workers
                    .lock()
                    .ok()
                    .and_then(|w| w.iter().find(|w| w.server_id() == id).cloned())
                    .ok_or_else(|| {
                        failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable")
                    })?;
                let s = subject(&router, w.as_ref(), &d)?;
                w.authorize(s)
                    .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable"))
            })
            .await;
        let host = match admitted {
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
        {
            let mut streams = self.streams.lock().unwrap();
            streams.retain(|(_, s)| s.realtime.active() > 0);
            if streams.len() >= 32 {
                return failure(StatusCode::SERVICE_UNAVAILABLE, "realtime_busy");
            }
            streams.push((stop.clone(), shared.clone()));
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
        let reply = realtime::upgrade(request, shared).await;
        // Host revocation closes streams; short worker subject expiry also bounds
        // them. H07 replaces this polling seam with the revocation barrier.
        let identity = self.router.identity.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            let until = tokio::time::Instant::now() + Duration::from_secs(29);
            loop {
                tick.tick().await;
                if tokio::time::Instant::now() >= until || identity.authenticate(&token).is_err() {
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
