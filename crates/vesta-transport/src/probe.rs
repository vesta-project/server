//! Native updater health probe: fixed loopback TLS response, no runtime/auth/
//! setup/files/mutation routes. No caller-provided path or secret is served.
use super::*;
use http::Method;
/// Observations only. Native updater composition validates these against its
/// own child, held signed payload, TLS pin and exclusive maintenance fences.
pub struct LocalUpdateProbe {
    pub protocol: u16,
    pub nonce: String,
    pub manifest_sha256: String,
    pub tls_sha256: String,
    pub observation: vesta_control::onboarding::consumer::Observation,
}
struct Application {
    tls: Arc<tls::Material>,
    timeout: Duration,
    payload: Vec<u8>,
}
impl HttpApplication for Application {
    fn tls(&self) -> &Arc<tls::Material> {
        &self.tls
    }
    fn timeout(&self) -> Duration {
        self.timeout
    }
    async fn reap(&self, _: bool) {}
    async fn dispatch(
        self: Arc<Self>,
        request: Request<Incoming>,
        _: SocketAddr,
    ) -> Result<Reply, Infallible> {
        Ok(
            if request.method() != Method::GET || request.uri().path() != "/api/v3/update-health" {
                error(StatusCode::NOT_FOUND, "not_found")
            } else if request.uri().query().is_some() || !request.body().is_end_stream() {
                error(StatusCode::BAD_REQUEST, "invalid_request")
            } else {
                response(StatusCode::OK, self.payload.clone(), "application/json")
            },
        )
    }
}
impl Transport {
    /// Only trusted local composition may supply this small, nonsecret response.
    /// The listener must be loopback; it can use a kernel-assigned trial port.
    pub fn start_local_probe(
        data: LocalUpdateProbe,
        listener: &TlsListener,
        expected_fingerprint: &str,
    ) -> Result<Self, TransportError> {
        let hex = |s: &str, length| s.len() == length && s.bytes().all(|b| b.is_ascii_hexdigit());
        if !listener.bind().is_loopback()
            || data.protocol == 0
            || !hex(&data.nonce, 32)
            || !hex(&data.manifest_sha256, 64)
            || !hex(&data.tls_sha256, 64)
            || data.tls_sha256 != expected_fingerprint
        {
            return Err(TransportError::Runtime);
        }
        let mut scope =
            serde_json::to_value(data.observation).map_err(|_| TransportError::Runtime)?;
        scope["tls_sha256"] = serde_json::json!(data.tls_sha256);
        let payload=serde_json::to_vec(&serde_json::json!({"protocol":data.protocol,"nonce":data.nonce,"health":{"manifest_sha256":data.manifest_sha256,"scope":scope}})).map_err(|_|TransportError::Runtime)?;
        if payload.len() > CONTROL_BODY_MAXIMUM {
            return Err(TransportError::Runtime);
        }
        pending::start_small_application(
            listener,
            Some(expected_fingerprint),
            move |tls, config| {
                Arc::new(Application {
                    tls,
                    timeout: Duration::from_secs(config.request_timeout_seconds().into()),
                    payload,
                })
            },
        )
    }
}
