//! Restricted first-server context. Shares account dispatch and the managed
//! transport's TLS/connection/deadline machinery without a fake game runtime.
use super::*;
use http::Method;
use http_body_util::BodyExt;
use vesta_control::{
    identity::{
        management::{ManagementAccess, permissions::PermissionError},
        minecraft::{MinecraftChallenges, ServerUuid},
    },
    onboarding::FirstServerSetup,
};

#[derive(Clone)]
pub struct PendingHost {
    setup: FirstServerSetup,
    minecraft: Arc<MinecraftChallenges>,
    installation_id: String,
    provisioning: Option<vesta_control::onboarding::provisioning::Provisioning>,
}
impl PendingHost {
    /// OS-local composition before binding the pending listener.
    pub fn new(setup: FirstServerSetup) -> Result<Self, TransportError> {
        let installation_id = setup.installation_id().to_owned();
        let server = ServerUuid::parse(setup.management().server_id())
            .map_err(|_| TransportError::Runtime)?;
        let minecraft =
            Arc::new(MinecraftChallenges::new(server).map_err(|_| TransportError::Runtime)?);
        Ok(Self {
            setup,
            minecraft,
            installation_id,
            provisioning: None,
        })
    }
    pub fn with_provisioning(
        mut self,
        service: vesta_control::onboarding::provisioning::Provisioning,
    ) -> Self {
        self.provisioning = Some(service);
        self
    }
    pub fn management(&self) -> &ManagementAccess {
        self.setup.management()
    }
}

struct PendingApplication {
    host: PendingHost,
    tls: Arc<tls::Material>,
    timeout: Duration,
    maximum: usize,
    bodies: Arc<Semaphore>,
    limiter: Mutex<PeerRateLimiter>,
    jobs: BlockingJobs,
}
impl HttpApplication for PendingApplication {
    fn tls(&self) -> &Arc<tls::Material> {
        &self.tls
    }
    fn timeout(&self) -> Duration {
        self.timeout
    }
    async fn reap(&self, all: bool) {
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
impl PendingApplication {
    async fn route(&self, request: Request<Incoming>, peer: SocketAddr) -> Reply {
        let allowed = self
            .limiter
            .lock()
            .map(|mut l| matches!(l.try_acquire(peer.ip()), AdmissionDecision::Allowed))
            .unwrap_or(false);
        if !allowed {
            return with_retry_after(
                error(StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
                Duration::from_secs(1),
            );
        }
        if request.uri().query().is_some() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let method = request.method().clone();
        let original = request.uri().path();
        let path = if matches!(
            original,
            "/api/v3/auth/device/enroll"
                | "/api/v3/auth/minecraft/challenge"
                | "/api/v3/auth/minecraft/complete"
                | "/api/v3/auth/session/challenge"
                | "/api/v3/auth/session"
        ) {
            original.replacen("/api/v3/auth/", "/api/v3/manage/", 1)
        } else {
            original.to_owned()
        };
        let auth = matches!(
            path.as_str(),
            "/api/v3/manage/minecraft/challenge"
                | "/api/v3/manage/minecraft/complete"
                | "/api/v3/manage/session/challenge"
                | "/api/v3/manage/session"
                | "/api/v3/manage/pair"
                | "/api/v3/manage/device/enroll"
        );
        let job_id = path
            .strip_prefix("/api/v3/setup/jobs/")
            .filter(|id| id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'));
        let provisioning = method == Method::POST
            && matches!(path.as_str(), "/api/v3/setup/plan" | "/api/v3/setup/apply")
            || method == Method::GET && job_id.is_some();
        let recognized = provisioning
            || (method == Method::POST && (auth || path == "/api/v3/setup/claim"))
            || (method == Method::GET
                && matches!(path.as_str(), "/api/v3/setup" | "/api/v3/setup/status"));
        if !recognized {
            return error(StatusCode::NOT_FOUND, "not_found");
        }
        let authorization =
            routes::bearer(request.headers()).map(|s| zeroize::Zeroizing::new(s.to_owned()));
        if request
            .headers()
            .get("content-length")
            .and_then(|s| s.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|v| v > self.maximum as u64)
        {
            return error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
        }
        let reserve = if request.body().is_end_stream() {
            0
        } else {
            self.maximum
        };
        let Ok(permit) = self.bodies.clone().try_acquire_many_owned(reserve as u32) else {
            return error(StatusCode::SERVICE_UNAVAILABLE, "transport_busy");
        };
        let mut body = request.into_body();
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        while let Some(frame) = body.frame().await {
            let Ok(frame) = frame else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let Ok(data) = frame.into_data() else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            if data.len() > self.maximum.saturating_sub(bytes.len()) {
                return error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
            }
            bytes.extend_from_slice(&data);
        }
        if (method == Method::GET || path == "/api/v3/setup/claim") && !bytes.is_empty() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let host = self.host.clone();
        self.jobs.execute_for(false,move||{
            let _permit=permit;
            if auth{return routes::account_dispatch(host.management(),&host.minecraft,&path,true,authorization.as_deref().map(|s|s.as_str()),&bytes);}
            if path=="/api/v3/setup" {
                return routes::json(serde_json::json!({"api_version":"3","installation_id":host.installation_id,"server_id":host.management().server_id(),"status":"awaiting_app_onboarding","management_available":false,"provisioning_available":host.provisioning.is_some()}));
            }
            let Some(token)=authorization else{return routes::management_error(vesta_control::identity::management::ManagementError::SessionInvalid);};
            if provisioning {
                let Some(service)=host.provisioning else{return error(StatusCode::SERVICE_UNAVAILABLE,"setup_local_configuration_required");};
                // Authenticate before parsing to avoid exposing installed state or
                // validation differences to an unauthenticated caller.
                if let Err(e)=host.setup.claimed_status(&token){return setup_error(e);}
                let result=match path.as_str(){
                    "/api/v3/setup/plan"=>match serde_json::from_slice(&bytes){Ok(input)=>service.prepare(&token,input),Err(_)=>return error(StatusCode::BAD_REQUEST,"invalid_request")},
                    "/api/v3/setup/apply"=>match serde_json::from_slice(&bytes){Ok(input)=>service.apply(&token,input),Err(_)=>return error(StatusCode::BAD_REQUEST,"invalid_request")},
                    _=>service.status(&token,path.trim_start_matches("/api/v3/setup/jobs/")),
                };
                return match result {Ok(value)=>{let mut reply=routes::json(serde_json::to_value(value).expect("fixed setup job"));if method==Method::POST{*reply.status_mut()=StatusCode::ACCEPTED;}reply},Err(e)=>setup_error(e)};
            }
            let result=if method==Method::POST {host.setup.claim(&token)}else{host.setup.claimed_status(&token)};
            match result {
                Ok(value)=>routes::json(serde_json::to_value(value).expect("fixed setup status")),
                Err(e)=>setup_error(e),
            }
        }).await
    }
}

fn setup_error(e: PermissionError) -> Reply {
    error(
        match e {
            PermissionError::SessionInvalid => StatusCode::UNAUTHORIZED,
            PermissionError::Forbidden => StatusCode::FORBIDDEN,
            PermissionError::Conflict | PermissionError::IdempotencyConflict => {
                StatusCode::CONFLICT
            }
            PermissionError::NotFound => StatusCode::NOT_FOUND,
            PermissionError::InvalidRequest => StatusCode::BAD_REQUEST,
            PermissionError::PreconditionFailed => StatusCode::PRECONDITION_FAILED,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        },
        "setup_access_refused",
    )
}

impl Transport {
    pub fn start_pending(
        host: PendingHost,
        listener: &TlsListener,
    ) -> Result<Self, TransportError> {
        Self::start_pending_checked(host, listener, None)
    }
    pub fn start_pending_pinned(
        host: PendingHost,
        listener: &TlsListener,
        expected_fingerprint: &str,
    ) -> Result<Self, TransportError> {
        Self::start_pending_checked(host, listener, Some(expected_fingerprint))
    }
    fn start_pending_checked(
        host: PendingHost,
        listener: &TlsListener,
        expected: Option<&str>,
    ) -> Result<Self, TransportError> {
        start_small_application(listener, expected, move |serving_tls, config| {
            Arc::new(PendingApplication {
                host,
                tls: serving_tls,
                timeout: Duration::from_secs(config.request_timeout_seconds().into()),
                maximum: (config.max_request_bytes() as usize).min(CONTROL_BODY_MAXIMUM),
                bodies: Arc::new(Semaphore::new(REQUEST_BODY_BUDGET)),
                limiter: Mutex::new(PeerRateLimiter::new(
                    RatePolicy::new(
                        config.peer_requests_per_minute(),
                        Duration::from_secs(60),
                        1024,
                        Duration::from_secs(120),
                    )
                    .expect("validated rate policy"),
                )),
                jobs: BlockingJobs::with_workers(tokio::runtime::Handle::current(), 1),
            })
        })
    }
}

/// Shared native composition for small pending/probe contexts; same accept, TLS,
/// connection and request deadline loop as the managed host.
pub(crate) fn start_small_application<A: HttpApplication, F>(
    listener: &TlsListener,
    expected: Option<&str>,
    make: F,
) -> Result<Transport, TransportError>
where
    F: FnOnce(Arc<tls::Material>, &TlsListener) -> Arc<A> + Send + 'static,
{
    let tls = Arc::new(tls::Material::new(listener)?);
    if let Some(expected) = expected {
        if tls.fingerprint()?.as_str() != expected {
            return Err(TransportError::Runtime);
        }
    }
    let socket = std::net::TcpListener::bind((listener.bind(), listener.port()))
        .map_err(|_| TransportError::Bind)?;
    socket
        .set_nonblocking(true)
        .map_err(|_| TransportError::Bind)?;
    let address = socket.local_addr().map_err(|_| TransportError::Bind)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .thread_name("vesta-pending-https")
        .build()
        .map_err(|_| TransportError::Runtime)?;
    let (stop, stopped) = watch::channel(false);
    let (done, complete) = mpsc::sync_channel(1);
    let serving_tls = tls.clone();
    let config = listener.clone();
    let thread = thread::Builder::new()
        .name("vesta-pending-owner".into())
        .spawn(move || {
            runtime.block_on(async move {
                let Ok(socket) = TcpListener::from_std(socket) else {
                    return;
                };
                let app = make(serving_tls, &config);
                serve_application(socket, config, app, stopped).await;
            });
            let _ = done.send(());
        })
        .map_err(|_| TransportError::Runtime)?;
    Ok(Transport {
        address,
        tls,
        stop,
        complete,
        thread: Some(thread),
    })
}
