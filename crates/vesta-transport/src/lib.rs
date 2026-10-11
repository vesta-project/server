//! Owned, bounded HTTPS transport for descriptive discovery and pack-only access.
//! There is deliberately no canonical management authentication boundary here.
//!
//! A session bearer can mint a single-use realtime ticket with an empty
//! `POST /api/v2/realtime/ticket`. Upgrade `GET /api/v2/realtime?installed=N`
//! with that ticket in exactly one `Authorization: Bearer` header. Credentials
//! are never accepted in cookies or URLs. Application messages use a `type` tag:
//! `hello` and `pack_update` carry revision, minimum_revision and status;
//! `heartbeat` has no payload. Unpublished hello has zero revisions and null
//! status. Clients may send only WebSocket Ping, Pong and Close control frames.
//! Download tickets from `POST /api/v2/pack/ticket` cannot upgrade realtime;
//! realtime tickets cannot authenticate a session or download an artifact.
//!
//! TLS rotation is explicit via [`Transport::reload_tls`]. `vesta-agent serve`
//! invokes it every five seconds, using protected reads capped at 256 KiB per file
//! and comparing mtime, size and SHA-256 (including content with unchanged metadata).
//! Install a complete PEM chain and matching key at the original configured paths.
//! Invalid replacements retain the previous identity and record a fixed error;
//! there is no plaintext/generated-certificate fallback. Accepted connections retain
//! their snapshot, while subsequently accepted handshakes use the replacement.
//! Discovery returns the active leaf DER SHA-256 in the lowercase-hex HTTP header
//! `X-Vesta-TLS-SHA256-Fingerprint`, leaving the strict JSON manifest unchanged.
//! That header describes the current listener, even on a pre-rotation connection;
//! it does not grant trust. Existing dependencies expose no standalone validity
//! window accessor, so expiry/not-before and issuer-chain validation remain the
//! client's responsibility. Reads are byte-bounded, not filesystem-time-bounded.
use http::{Request, Response, StatusCode};
use hyper::{
    body::{Body, Bytes, Frame, Incoming, SizeHint},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use sha2::{Digest, Sha256};
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
use std::{
    fs::File,
    future::Future,
    io::{self, Read},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpListener,
    sync::{Notify, OwnedSemaphorePermit, Semaphore, oneshot, watch},
    task::{JoinHandle, JoinSet},
};
use tokio_rustls::TlsAcceptor;
use vesta_control::{
    admission::{AdmissionDecision, PeerRateLimiter, RatePolicy},
    config::TlsListener,
    host::NetworkHost,
};

mod file_body;
mod manage;
mod realtime;
mod routes;
mod tls;
type Reply = Response<ReplyBody>;
const DOWNLOADS: usize = 2;
const BODY_CHUNK: usize = 64 * 1024;
const HANDSHAKES: usize = 8;
/// Bytes per second a streaming artifact body must sustain, measured over each
/// `request_timeout` window. A stalled or trickling peer loses its connection and
/// with it every download permit held by its body and buffered chunks.
const MIN_DOWNLOAD_RATE: u64 = 8 * 1024;
/// After the final chunk is handed to Hyper, up to two chunks may still be queued
/// for the peer; allow them to drain at the floor rate before the flush deadline.
const DRAIN: Duration = Duration::from_secs(2 * BODY_CHUNK as u64 / MIN_DOWNLOAD_RATE);

/// Bound reuse even when every request and idle interval stays within its limit.
const MAX_REQUESTS: usize = 32;

/// Separate handshake/first-head, request, idle and artifact throughput budgets.
/// Each uses `request_timeout`; downloads have no total-duration cap. Later
/// requests start at their first read, not at head completion: partial reads never
/// refresh that budget. Responses stay timed until their final socket flush.
struct ConnectionDeadline {
    limit: Duration,
    phase: Mutex<Phase>,
    changed: Notify,
}
enum Phase {
    FirstHead {
        deadline: std::time::Instant,
    },
    Idle {
        deadline: std::time::Instant,
    },
    Request {
        deadline: std::time::Instant,
        complete: bool,
    },
    Drain {
        deadline: std::time::Instant,
    },
    Streaming {
        window: std::time::Instant,
        bytes: u64,
    },
}
impl ConnectionDeadline {
    fn new(limit: Duration) -> Self {
        Self {
            limit,
            phase: Mutex::new(Phase::FirstHead {
                deadline: std::time::Instant::now() + limit,
            }),
            changed: Notify::new(),
        }
    }
    fn read(&self) {
        if let Ok(mut phase) = self.phase.lock()
            && let Phase::Idle { deadline } = *phase
            && std::time::Instant::now() < deadline
        {
            *phase = Phase::Request {
                deadline: std::time::Instant::now() + self.limit,
                complete: false,
            };
            self.changed.notify_one();
        }
    }
    fn request(&self) {
        if let Ok(mut phase) = self.phase.lock() {
            // Hyper can already have the next head buffered (pipelining). That
            // request starts when dispatched; a head read from the socket keeps
            // the earlier read deadline. Never revive an expired head or idle slot.
            let now = std::time::Instant::now();
            if matches!(*phase, Phase::FirstHead { deadline } | Phase::Idle { deadline } if now < deadline)
            {
                *phase = Phase::Request {
                    deadline: now + self.limit,
                    complete: false,
                };
                self.changed.notify_one();
            }
        }
    }
    fn response_complete(&self) {
        if let Ok(mut phase) = self.phase.lock()
            && let Phase::Request { complete, .. } = &mut *phase
        {
            *complete = true;
        }
    }
    fn flushed(&self) {
        if let Ok(mut phase) = self.phase.lock() {
            let now = std::time::Instant::now();
            if matches!(*phase, Phase::Request { deadline, complete: true } | Phase::Drain { deadline } if now < deadline)
            {
                *phase = Phase::Idle {
                    deadline: now + self.limit,
                };
                self.changed.notify_one();
            }
        }
    }
    fn stream(&self) {
        if let Ok(mut phase) = self.phase.lock() {
            let now = std::time::Instant::now();
            if matches!(*phase, Phase::FirstHead { deadline } | Phase::Request { deadline, .. } if now < deadline)
            {
                *phase = Phase::Streaming {
                    window: now,
                    bytes: 0,
                };
                self.changed.notify_one();
            }
        }
    }
    fn progress(&self, count: usize) {
        if let Ok(mut phase) = self.phase.lock()
            && let Phase::Streaming { bytes, .. } = &mut *phase
        {
            *bytes = bytes.saturating_add(count as u64);
        }
    }
    fn finish(&self) {
        if let Ok(mut phase) = self.phase.lock()
            && matches!(*phase, Phase::Streaming { .. })
        {
            *phase = Phase::Drain {
                deadline: std::time::Instant::now() + self.limit + DRAIN,
            };
            self.changed.notify_one();
        }
    }
    fn floor(&self) -> u64 {
        let millis = u64::try_from(self.limit.as_millis()).unwrap_or(u64::MAX);
        MIN_DOWNLOAD_RATE.saturating_mul(millis) / 1000
    }
    /// The next instant to re-check, or `None` once the connection must close.
    fn check(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        let floor = self.floor();
        let mut phase = self.phase.lock().ok()?;
        match &mut *phase {
            Phase::FirstHead { deadline }
            | Phase::Idle { deadline }
            | Phase::Request { deadline, .. }
            | Phase::Drain { deadline } => (now < *deadline).then_some(*deadline),
            Phase::Streaming { window, bytes } => {
                let end = *window + self.limit;
                if now < end {
                    return Some(end);
                }
                if *bytes < floor {
                    return None;
                }
                *window = now;
                *bytes = 0;
                Some(now + self.limit)
            }
        }
    }
    async fn expired(&self) {
        while let Some(wake) = self.check(std::time::Instant::now()) {
            tokio::select! {
                () = tokio::time::sleep_until(wake.into()) => {},
                () = self.changed.notified() => {},
            }
        }
    }
}

/// Observe plaintext reads and final response flushes without parsing HTTP here.
/// Hyper still owns head parsing, buffering, backpressure and protocol upgrades.
struct DeadlineIo<T> {
    inner: T,
    deadline: Arc<ConnectionDeadline>,
}
impl<T: AsyncRead + Unpin> AsyncRead for DeadlineIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.deadline.read();
        }
        result
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for DeadlineIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_flush(cx);
        if matches!(result, Poll::Ready(Ok(()))) {
            self.deadline.flushed();
        }
        result
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Concurrent artifact downloads: a global cap plus one per pack session and one
/// per peer IP, so a single player or address cannot hold the whole budget. The
/// holder sets only contain entries backed by a live global permit, so they are
/// bounded by `DOWNLOADS` and cleaned up when the permit drops.
struct Downloads<S> {
    global: Arc<Semaphore>,
    holders: Arc<Mutex<Holders<S>>>,
}
struct Holders<S> {
    sessions: std::collections::HashSet<S>,
    peers: std::collections::HashSet<std::net::IpAddr>,
}
struct Holder<S: Eq + std::hash::Hash> {
    holders: Arc<Mutex<Holders<S>>>,
    session: S,
    peer: std::net::IpAddr,
}
impl<S: Eq + std::hash::Hash> Drop for Holder<S> {
    fn drop(&mut self) {
        if let Ok(mut holders) = self.holders.lock() {
            holders.sessions.remove(&self.session);
            holders.peers.remove(&self.peer);
        }
    }
}
/// Held by the body and every chunk still owned by Hyper or the socket buffer.
struct DownloadPermit {
    // Declared first: holder slots are released before the global permit.
    _holder: Option<Box<dyn Send + Sync>>,
    _global: OwnedSemaphorePermit,
}
impl From<OwnedSemaphorePermit> for DownloadPermit {
    fn from(global: OwnedSemaphorePermit) -> Self {
        Self {
            _holder: None,
            _global: global,
        }
    }
}
impl<S: Clone + Eq + std::hash::Hash + Send + Sync + 'static> Downloads<S> {
    fn new(global: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global)),
            holders: Arc::new(Mutex::new(Holders {
                sessions: std::collections::HashSet::with_capacity(global),
                peers: std::collections::HashSet::with_capacity(global),
            })),
        }
    }
    fn acquire(&self, session: S, peer: std::net::IpAddr) -> Option<DownloadPermit> {
        let peer = peer.to_canonical();
        let mut holders = self.holders.lock().ok()?;
        if holders.sessions.contains(&session) || holders.peers.contains(&peer) {
            return None;
        }
        let global = self.global.clone().try_acquire_owned().ok()?;
        holders.sessions.insert(session.clone());
        holders.peers.insert(peer);
        drop(holders);
        Some(DownloadPermit {
            _holder: Some(Box::new(Holder {
                holders: self.holders.clone(),
                session,
                peer,
            })),
            _global: global,
        })
    }
}

enum ReplyBody {
    Bytes(Bytes),
    TimedBytes {
        bytes: Bytes,
        deadline: Arc<ConnectionDeadline>,
    },
    Artifact(Box<ArtifactBody>),
    File(Box<file_body::FileBody>),
}
impl ReplyBody {
    fn watch(&mut self, deadline: Arc<ConnectionDeadline>) {
        match self {
            Self::Bytes(bytes) => {
                if bytes.is_empty() {
                    deadline.response_complete();
                }
                *self = Self::TimedBytes {
                    bytes: std::mem::take(bytes),
                    deadline,
                };
            }
            Self::Artifact(body) => body.watch(deadline),
            Self::File(body) => body.watch(deadline),
            Self::TimedBytes { .. } => unreachable!("response is watched once"),
        }
    }
}
struct ArtifactBody {
    stream: Option<ArtifactStream>,
    pending: Option<JoinHandle<(ArtifactStream, io::Result<Bytes>)>>,
    deadline: Option<Arc<ConnectionDeadline>>,
}
impl ArtifactBody {
    /// Switch the connection from the request deadline to the throughput floor.
    fn watch(&mut self, deadline: Arc<ConnectionDeadline>) {
        deadline.stream();
        self.deadline = Some(deadline);
    }
}
impl Drop for ArtifactBody {
    fn drop(&mut self) {
        if let Some(deadline) = &self.deadline {
            deadline.finish();
        }
    }
}
struct ArtifactStream {
    file: File,
    hash: Sha256,
    expected_hash: [u8; 32],
    size: u64,
    remaining: u64,
    permit: Arc<DownloadPermit>,
}
impl ArtifactBody {
    fn new(
        file: File,
        artifact: &vesta_control::packs::PublishedArtifact,
        permit: DownloadPermit,
    ) -> io::Result<Self> {
        // Hyper may never poll a zero-length body. Validate that identity now.
        if artifact.size() == 0 && Sha256::digest([])[..] != artifact.sha256().as_bytes()[..] {
            return Err(io::Error::other("artifact identity mismatch"));
        }
        Ok(Self {
            stream: Some(ArtifactStream {
                file,
                hash: Sha256::new(),
                expected_hash: *artifact.sha256().as_bytes(),
                size: artifact.size(),
                remaining: artifact.size(),
                permit: Arc::new(permit),
            }),
            pending: None,
            deadline: None,
        })
    }
}
impl ArtifactStream {
    fn read_chunk(&mut self) -> io::Result<Bytes> {
        // Only this fixed-size allocation is made, never an artifact-sized buffer.
        let count = self.remaining.min(BODY_CHUNK as u64) as usize;
        let mut data = vec![0; count];
        self.file.read_exact(&mut data)?;
        self.hash.update(&data);
        self.remaining -= count as u64;
        if self.remaining == 0 {
            let mut extra = [0; 1];
            if self.file.read(&mut extra)? != 0
                || self.file.metadata()?.len() != self.size
                || self.hash.clone().finalize()[..] != self.expected_hash[..]
            {
                return Err(io::Error::other("artifact identity mismatch"));
            }
            // Withhold the final bytes until verification: Content-Length must not
            // allow Hyper/the peer to consider a mismatched response complete.
        }
        Ok(Bytes::from_owner(DownloadOwner {
            data,
            _permit: self.permit.clone(),
        }))
    }
}
impl Body for ReplyBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        match &mut *self {
            Self::Bytes(bytes) => {
                if bytes.is_empty() {
                    return Poll::Ready(None);
                }
                let count = bytes.len().min(BODY_CHUNK);
                Poll::Ready(Some(Ok(Frame::data(bytes.split_to(count)))))
            }
            Self::TimedBytes { bytes, deadline } => {
                let frame = (!bytes.is_empty()).then(|| {
                    let count = bytes.len().min(BODY_CHUNK);
                    Frame::data(bytes.split_to(count))
                });
                if bytes.is_empty() {
                    deadline.response_complete();
                }
                Poll::Ready(frame.map(Ok))
            }
            Self::File(body) => Pin::new(body.as_mut()).poll_frame(cx),
            Self::Artifact(body) => {
                if body.pending.is_none() {
                    let Some(mut stream) = body.stream.take() else {
                        return Poll::Ready(None);
                    };
                    if stream.remaining == 0 {
                        return Poll::Ready(None);
                    }
                    // A cancelled body cannot cancel synchronous IO. The task owns
                    // its file and permit until that one bounded read finishes.
                    body.pending = Some(tokio::task::spawn_blocking(move || {
                        let data = stream.read_chunk();
                        (stream, data)
                    }));
                }
                let result = match Pin::new(body.pending.as_mut().expect("pending read")).poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(result) => result,
                };
                body.pending = None;
                match result {
                    Ok((stream, Ok(bytes))) => {
                        if stream.remaining != 0 {
                            body.stream = Some(stream);
                        }
                        // Hyper polls for another chunk only once its bounded write
                        // buffer has room, so hand-off approximates peer progress.
                        if let Some(deadline) = &body.deadline {
                            deadline.progress(bytes.len());
                        }
                        Poll::Ready(Some(Ok(Frame::data(bytes))))
                    }
                    Ok((_, Err(error))) => Poll::Ready(Some(Err(error))),
                    Err(_) => Poll::Ready(Some(Err(io::Error::other("artifact read failed")))),
                }
            }
        }
    }
    fn is_end_stream(&self) -> bool {
        match self {
            Self::Bytes(bytes) | Self::TimedBytes { bytes, .. } => bytes.is_empty(),
            Self::File(body) => body.is_end_stream(),
            Self::Artifact(body) => {
                body.pending.is_none() && body.stream.as_ref().is_none_or(|s| s.remaining == 0)
            }
        }
    }
    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Bytes(bytes) | Self::TimedBytes { bytes, .. } => {
                SizeHint::with_exact(bytes.len() as u64)
            }
            // Content-Length is set from the checked published size. Do not claim
            // completion while an unverified read is in flight.
            Self::Artifact(_) | Self::File(_) => SizeHint::default(),
        }
    }
}
struct DownloadOwner<T> {
    data: T,
    _permit: Arc<DownloadPermit>,
}
impl<T: AsRef<[u8]>> AsRef<[u8]> for DownloadOwner<T> {
    fn as_ref(&self) -> &[u8] {
        self.data.as_ref()
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
/// Read-only live TLS identity handle for the protected local owner channel.
#[derive(Clone)]
pub struct TlsFingerprint(Arc<tls::Material>);
impl TlsFingerprint {
    pub fn current(&self) -> Result<String, TransportError> {
        self.0.fingerprint()
    }
}
pub struct Transport {
    address: SocketAddr,
    tls: Arc<tls::Material>,
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
        let tls = Arc::new(tls::Material::new(listener)?);
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
        // Realtime policy/release reads must never occupy HTTP blocking workers.
        // This owned runtime supplies only a separate two-thread blocking pool.
        let realtime_runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(WORKERS)
            .thread_name("vesta-realtime-check")
            .build()
            .map_err(|_| TransportError::Runtime)?;
        let install_runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(WORKERS)
            .thread_name("vesta-content-install")
            .build()
            .map_err(|_| TransportError::Runtime)?;
        let refresh = host
            .start_admission_refresh()
            .map_err(|_| TransportError::Runtime)?;
        let (stop, stopped) = watch::channel(false);
        let (done, complete) = mpsc::sync_channel(1);
        let listener = listener.clone();
        let serving_tls = tls.clone();
        let thread = thread::Builder::new()
            .name("vesta-https-owner".into())
            .spawn(move || {
                runtime.block_on(serve(
                    socket,
                    host,
                    listener,
                    serving_tls,
                    stopped,
                    realtime_runtime.handle().clone(),
                    install_runtime.handle().clone(),
                ));
                drop(refresh);
                drop(realtime_runtime);
                drop(install_runtime);
                drop(runtime);
                let _ = done.send(());
            })
            .map_err(|_| TransportError::Runtime)?;
        Ok(Self {
            address,
            tls,
            stop,
            complete,
            thread: Some(thread),
        })
    }
    /// Re-read configured paths through the protected bounded reader. New accepted
    /// handshakes use the replacement; established streams retain their identity.
    /// Returns false when mtime, size and SHA-256 are unchanged. A failed reload
    /// retains the old configuration and records only `tls_material_unavailable`.
    pub fn reload_tls(&self) -> Result<bool, TransportError> {
        self.tls.reload()
    }
    pub fn fingerprint_handle(&self) -> TlsFingerprint {
        TlsFingerprint(self.tls.clone())
    }
    pub fn tls_fingerprint(&self) -> Result<String, TransportError> {
        self.tls.fingerprint()
    }
    pub fn last_tls_reload_error(&self) -> Option<TransportError> {
        self.tls.last_error()
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
    tls: Arc<tls::Material>,
    host: NetworkHost,
    max_body: usize,
    timeout: Duration,
    limiter: Mutex<PeerRateLimiter>,
    jobs: BlockingJobs,
    realtime_jobs: BlockingJobs,
    install_jobs: BlockingJobs,
    downloads: Arc<Downloads<vesta_control::pack_access::RealtimeSession>>,
    realtime: realtime::Sockets,
    stop: watch::Receiver<bool>,
}
struct BlockingJobs {
    runtime: tokio::runtime::Handle,
    handles: Mutex<Vec<JoinHandle<()>>>,
}
impl BlockingJobs {
    fn new(runtime: tokio::runtime::Handle) -> Self {
        Self {
            runtime,
            handles: Mutex::new(Vec::with_capacity(WORKERS)),
        }
    }
    /// Two synchronous operations maximum, including abandoned HTTP requests.
    #[cfg(test)]
    async fn execute(&self, action: impl FnOnce() -> Reply + Send + 'static) -> Reply {
        self.execute_for(false, action).await
    }
    async fn execute_for(
        &self,
        operational: bool,
        action: impl FnOnce() -> Reply + Send + 'static,
    ) -> Reply {
        self.run(action).await.unwrap_or_else(|_| {
            if operational {
                manage::error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable")
            } else {
                error(StatusCode::SERVICE_UNAVAILABLE, "transport_busy")
            }
        })
    }
    async fn run<T: Send + 'static>(
        &self,
        action: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, ()> {
        let (tx, rx) = oneshot::channel();
        {
            let Ok(mut jobs) = self.handles.lock() else {
                return Err(());
            };
            if jobs.len() >= WORKERS {
                return Err(());
            }
            jobs.push(self.runtime.spawn_blocking(move || {
                let _ = tx.send(action());
            }));
        }
        rx.await.map_err(|_| ())
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
    tls: Arc<tls::Material>,
    mut stop: watch::Receiver<bool>,
    realtime_runtime: tokio::runtime::Handle,
    install_runtime: tokio::runtime::Handle,
) {
    let Ok(listener) = TcpListener::from_std(socket) else {
        return;
    };
    let shared = Arc::new(Shared {
        tls,
        host,
        max_body: config.max_request_bytes() as usize,
        timeout: Duration::from_secs(config.request_timeout_seconds().into()),
        limiter: Mutex::new(PeerRateLimiter::new(
            RatePolicy::new(60, Duration::from_secs(60), 1024, Duration::from_secs(120))
                .expect("fixed rate policy"),
        )),
        downloads: Arc::new(Downloads::new(DOWNLOADS)),
        realtime: realtime::Sockets::new(),
        stop: stop.clone(),
        jobs: BlockingJobs::new(tokio::runtime::Handle::current()),
        realtime_jobs: BlockingJobs::new(realtime_runtime),
        install_jobs: BlockingJobs::new(install_runtime),
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
                if connections.len() + shared.realtime.active() >= usize::from(config.max_connections()) { drop(socket); continue; }
                if !matches!(connection_limiter.try_acquire(peer.ip()), AdmissionDecision::Allowed) { drop(socket); continue; }
                let Ok(handshake) = handshakes.clone().try_acquire_owned() else { drop(socket); continue; };
                let Ok(config) = shared.tls.config() else { drop(socket); continue; };
                let tls = TlsAcceptor::from(config); let state = shared.clone();
                connections.spawn(async move {
                    let limit = state.timeout;
                    // Handshake, heads and routing get `limit`; streamed artifact
                    // bodies are bounded by the throughput floor instead. Expiry drops
                    // the connection, its body and every download permit it holds.
                    let deadline = Arc::new(ConnectionDeadline::new(limit));
                    let watchdog = deadline.clone();
                    let work = async move {
                        let Ok(stream) = tls.accept(socket).await else { return; };
                        drop(handshake);
                        let stream = DeadlineIo { inner: stream, deadline: deadline.clone() };
                        let requests = std::sync::atomic::AtomicUsize::new(0);
                        let service = service_fn(move |request| {
                            let last = requests.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1 >= MAX_REQUESTS;
                            let state = state.clone();
                            let deadline = deadline.clone();
                            async move {
                                deadline.request();
                                let mut reply = handle(request, peer, state).await?;
                                reply.body_mut().watch(deadline);
                                // An upgrade hands ownership to the separately bounded
                                // realtime transport; do not replace Connection: upgrade.
                                if last && reply.status() != StatusCode::SWITCHING_PROTOCOLS {
                                    reply.headers_mut().insert(http::header::CONNECTION, http::HeaderValue::from_static("close"));
                                }
                                Ok::<_, Infallible>(reply)
                            }
                        });
                        // Hyper's head timer includes idle time before the first
                        // byte; our watchdog bounds those phases independently.
                        let _ = http1::Builder::new().header_read_timeout(None).max_headers(64).max_buf_size(HEADER_BYTES).serve_connection(TokioIo::new(stream), service).with_upgrades().await;
                    };
                    tokio::select! {
                        () = work => {}
                        () = watchdog.expired() => {}
                    }
                });
            }
        }
        shared.jobs.reap(false).await;
        shared.install_jobs.reap(false).await;
        shared.realtime.reap(false).await;
        shared.realtime_jobs.reap(false).await;
    }
    drop(listener);
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    shared.realtime.reap(true).await;
    shared.realtime_jobs.reap(true).await;
    shared.jobs.reap(true).await;
    shared.install_jobs.reap(true).await;
}

async fn handle(
    request: Request<Incoming>,
    peer: SocketAddr,
    state: Arc<Shared>,
) -> Result<Reply, Infallible> {
    let operational = manage::implemented(request.uri().path());
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
        Ok(AdmissionDecision::RateLimited { retry_after }) => Some(if operational {
            manage::error(StatusCode::TOO_MANY_REQUESTS, "rate_limited")
        } else {
            rate_limited("peer_rate_limited", retry_after)
        }),
        _ => Some(if operational {
            manage::error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable")
        } else {
            error(StatusCode::SERVICE_UNAVAILABLE, "peer_capacity_unavailable")
        }),
    };
    if let Some(reply) = denial {
        return Ok(reply);
    }
    let timeout = state.timeout;
    let result = tokio::time::timeout(timeout, routes::route(request, state, peer)).await;
    Ok(result.unwrap_or_else(|_| {
        if operational {
            manage::error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable")
        } else {
            error(StatusCode::REQUEST_TIMEOUT, "request_timeout")
        }
    }))
}
fn response(status: StatusCode, bytes: impl Into<Bytes>, content_type: &'static str) -> Reply {
    let bytes = bytes.into();
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .header("content-length", bytes.len())
        .header("cache-control", "no-store")
        .body(ReplyBody::Bytes(bytes))
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

/// Delta-seconds must round up: a fractional refill interval must not tell a
/// caller to retry before the next token. Other clients may consume that token.
fn rate_limited(code: &'static str, retry_after: Duration) -> Reply {
    let seconds = retry_after
        .as_secs()
        .saturating_add(u64::from(retry_after.subsec_nanos() != 0))
        .max(1);
    let mut reply = error(StatusCode::TOO_MANY_REQUESTS, code);
    reply.headers_mut().insert(
        http::header::RETRY_AFTER,
        seconds.to_string().parse().expect("decimal retry duration"),
    );
    reply
}

#[cfg(test)]
mod tests;
