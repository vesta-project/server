//! Owned, bounded HTTPS transport for descriptive discovery and pack-only access.
//! There is deliberately no canonical management authentication boundary here.
use http::{Request, Response, StatusCode};
use hyper::{
    body::{Body, Bytes, Frame, Incoming, SizeHint},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use rustls_pki_types::pem::PemObject;
use std::{
    convert::Infallible,
    fmt,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex, mpsc},
    task::{Context, Poll},
    thread,
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{OwnedSemaphorePermit, Semaphore, oneshot, watch},
    task::{JoinHandle, JoinSet},
};
use tokio_rustls::TlsAcceptor;
use vesta_control::{
    admission::{AdmissionDecision, PeerRateLimiter, RatePolicy},
    config::TlsListener,
    host::{NetworkHost, read_tls_document},
};

mod routes;
type Reply = Response<ReplyBody>;
const DOWNLOADS: usize = 2;
const BODY_CHUNK: usize = 64 * 1024;
const HANDSHAKES: usize = 8;

// A verified snapshot is zero-copy and globally bounded. Hold admission through
// the last transmitted chunk drop, not merely through synchronous load/body drop.
struct ReplyBody {
    bytes: Bytes,
}
impl Body for ReplyBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        if self.bytes.is_empty() {
            return Poll::Ready(None);
        }
        let count = self.bytes.len().min(BODY_CHUNK);
        Poll::Ready(Some(Ok(Frame::data(self.bytes.split_to(count)))))
    }
    fn is_end_stream(&self) -> bool {
        self.bytes.is_empty()
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.bytes.len() as u64)
    }
}
struct DownloadOwner<T> {
    data: T,
    _permit: OwnedSemaphorePermit,
}
impl<T: AsRef<[u8]>> AsRef<[u8]> for DownloadOwner<T> {
    fn as_ref(&self) -> &[u8] {
        self.data.as_ref()
    }
}
struct ArtifactOwner(vesta_control::packs::LoadedArtifact);
impl AsRef<[u8]> for ArtifactOwner {
    fn as_ref(&self) -> &[u8] {
        self.0.bytes()
    }
}
const WORKERS: usize = 2;
const HEADER_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    TlsMaterial,
    Bind,
    Runtime,
    Shutdown,
}
impl TransportError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TlsMaterial => "tls_material_unavailable",
            Self::Bind => "listener_bind_failed",
            Self::Runtime => "transport_unavailable",
            Self::Shutdown => "transport_shutdown_failed",
        }
    }
}
impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for TransportError {}

/// A timeout retains the entire runtime and its workers for another observation.
pub enum TransportShutdown {
    Complete,
    TimedOut(Transport),
}
pub struct Transport {
    address: SocketAddr,
    stop: watch::Sender<bool>,
    complete: mpsc::Receiver<()>,
    thread: Option<thread::JoinHandle<()>>,
}
impl fmt::Debug for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transport")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}
impl Transport {
    pub fn start(host: NetworkHost, listener: &TlsListener) -> Result<Self, TransportError> {
        let certificates =
            read_tls_document(listener.certificate()).map_err(|_| TransportError::TlsMaterial)?;
        let key = zeroize::Zeroizing::new(
            read_tls_document(listener.private_key()).map_err(|_| TransportError::TlsMaterial)?,
        );
        let certificates = rustls_pki_types::CertificateDer::pem_slice_iter(&certificates)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| TransportError::TlsMaterial)?;
        let key = rustls_pki_types::PrivateKeyDer::from_pem_slice(&key)
            .map_err(|_| TransportError::TlsMaterial)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| TransportError::TlsMaterial)?
            .with_no_client_auth()
            .with_single_cert(certificates, key)
            .map_err(|_| TransportError::TlsMaterial)?;
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        let socket = std::net::TcpListener::bind((listener.bind(), listener.port()))
            .map_err(|_| TransportError::Bind)?;
        socket
            .set_nonblocking(true)
            .map_err(|_| TransportError::Bind)?;
        let address = socket.local_addr().map_err(|_| TransportError::Bind)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(WORKERS)
            .enable_all()
            .thread_name("vesta-https-worker")
            .build()
            .map_err(|_| TransportError::Runtime)?;
        let refresh = host
            .start_admission_refresh()
            .map_err(|_| TransportError::Runtime)?;
        let (stop, stopped) = watch::channel(false);
        let (done, complete) = mpsc::sync_channel(1);
        let listener = listener.clone();
        let thread = thread::Builder::new()
            .name("vesta-https-owner".into())
            .spawn(move || {
                runtime.block_on(serve(
                    socket,
                    host,
                    listener,
                    TlsAcceptor::from(Arc::new(tls)),
                    stopped,
                ));
                drop(refresh);
                drop(runtime);
                let _ = done.send(());
            })
            .map_err(|_| TransportError::Runtime)?;
        Ok(Self {
            address,
            stop,
            complete,
            thread: Some(thread),
        })
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }
    pub fn shutdown(mut self, timeout: Duration) -> Result<TransportShutdown, TransportError> {
        let _ = self.stop.send(true);
        match self.complete.recv_timeout(timeout) {
            Ok(()) => {
                self.join()?;
                Ok(TransportShutdown::Complete)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(TransportShutdown::TimedOut(self)),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.join()?;
                Err(TransportError::Shutdown)
            }
        }
    }
    fn join(&mut self) -> Result<(), TransportError> {
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| TransportError::Shutdown)?;
        }
        Ok(())
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        // Explicit shutdown retains ownership on timeout; accidental Drop blocks
        // rather than detaching live synchronous work and its database handles.
        let _ = self.join();
    }
}

struct Shared {
    host: NetworkHost,
    max_body: usize,
    timeout: Duration,
    limiter: Mutex<PeerRateLimiter>,
    jobs: BlockingJobs,
    downloads: Arc<Semaphore>,
}
struct BlockingJobs {
    handles: Mutex<Vec<JoinHandle<()>>>,
}
impl BlockingJobs {
    /// Two synchronous operations maximum, including abandoned HTTP requests.
    async fn execute(&self, action: impl FnOnce() -> Reply + Send + 'static) -> Reply {
        let (tx, rx) = oneshot::channel();
        {
            let Ok(mut jobs) = self.handles.lock() else {
                return error(StatusCode::SERVICE_UNAVAILABLE, "transport_unavailable");
            };
            if jobs.len() >= WORKERS {
                return error(StatusCode::SERVICE_UNAVAILABLE, "transport_busy");
            }
            jobs.push(tokio::task::spawn_blocking(move || {
                let _ = tx.send(action());
            }));
        }
        rx.await
            .unwrap_or_else(|_| error(StatusCode::SERVICE_UNAVAILABLE, "transport_unavailable"))
    }
    async fn reap(&self, all: bool) {
        let handles = {
            let Ok(mut jobs) = self.handles.lock() else {
                return;
            };
            let mut handles = Vec::new();
            let mut index = 0;
            while index < jobs.len() {
                if all || jobs[index].is_finished() {
                    handles.push(jobs.swap_remove(index));
                } else {
                    index += 1;
                }
            }
            handles
        };
        for handle in handles {
            let _ = handle.await;
        }
    }
}

async fn serve(
    socket: std::net::TcpListener,
    host: NetworkHost,
    config: TlsListener,
    tls: TlsAcceptor,
    mut stop: watch::Receiver<bool>,
) {
    let Ok(listener) = TcpListener::from_std(socket) else {
        return;
    };
    let shared = Arc::new(Shared {
        host,
        max_body: config.max_request_bytes() as usize,
        timeout: Duration::from_secs(config.request_timeout_seconds().into()),
        limiter: Mutex::new(PeerRateLimiter::new(
            RatePolicy::new(60, Duration::from_secs(60), 1024, Duration::from_secs(120))
                .expect("fixed rate policy"),
        )),
        downloads: Arc::new(Semaphore::new(DOWNLOADS)),
        jobs: BlockingJobs {
            handles: Mutex::new(Vec::with_capacity(WORKERS)),
        },
    });
    let handshakes = Arc::new(Semaphore::new(HANDSHAKES));
    let mut connection_limiter = PeerRateLimiter::new(
        RatePolicy::new(120, Duration::from_secs(60), 1024, Duration::from_secs(120))
            .expect("fixed connection rate policy"),
    );
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = stop.changed() => break,
            _ = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                // Transient accept failures (e.g. descriptor exhaustion) must not stop serving.
                let Ok((socket, peer)) = accepted else { tokio::time::sleep(Duration::from_millis(50)).await; continue; };
                if connections.len() >= usize::from(config.max_connections()) { drop(socket); continue; }
                if !matches!(connection_limiter.try_acquire(peer.ip()), AdmissionDecision::Allowed) { drop(socket); continue; }
                let Ok(handshake) = handshakes.clone().try_acquire_owned() else { drop(socket); continue; };
                let tls = tls.clone(); let state = shared.clone();
                connections.spawn(async move {
                    let limit = state.timeout;
                    let _ = tokio::time::timeout(limit, async move {
                        let Ok(stream) = tls.accept(socket).await else { return; };
                        drop(handshake);
                        let service = service_fn(move |request| handle(request, peer, state.clone()));
                        let _ = http1::Builder::new().timer(TokioTimer::new()).header_read_timeout(limit).max_headers(64).max_buf_size(HEADER_BYTES).serve_connection(TokioIo::new(stream), service).await;
                    }).await;
                });
            }
        }
        shared.jobs.reap(false).await;
    }
    drop(listener);
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    shared.jobs.reap(true).await;
}

async fn handle(
    request: Request<Incoming>,
    peer: SocketAddr,
    state: Arc<Shared>,
) -> Result<Reply, Infallible> {
    let rate = state
        .limiter
        .lock()
        .map(|mut limiter| limiter.try_acquire(peer.ip()))
        .map_err(|_| ());
    if let Some(refusal) = routes::fixed_refusal(request.uri().path()) {
        return Ok(refusal);
    }
    let denial = match rate {
        Ok(AdmissionDecision::Allowed) => None,
        Ok(AdmissionDecision::RateLimited { .. }) => {
            Some(error(StatusCode::TOO_MANY_REQUESTS, "peer_rate_limited"))
        }
        _ => Some(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "peer_capacity_unavailable",
        )),
    };
    if let Some(reply) = denial {
        return Ok(reply);
    }
    let timeout = state.timeout;
    let result = tokio::time::timeout(timeout, routes::route(request, state)).await;
    Ok(result.unwrap_or_else(|_| error(StatusCode::REQUEST_TIMEOUT, "request_timeout")))
}
fn response(status: StatusCode, bytes: impl Into<Bytes>, content_type: &'static str) -> Reply {
    let bytes = bytes.into();
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .header("content-length", bytes.len())
        .header("cache-control", "no-store")
        .body(ReplyBody { bytes })
        .expect("fixed response headers")
}
fn error(status: StatusCode, code: &'static str) -> Reply {
    let mut reply = response(
        status,
        format!("{{\"error\":\"{code}\"}}"),
        "application/json",
    );
    reply
        .headers_mut()
        .insert("connection", http::HeaderValue::from_static("close"));
    reply
}

#[cfg(test)]
mod tests;
