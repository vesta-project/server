//! Pack notices only. Each socket owns a bounded codec, a session reference and
//! admission permits. There is no client command dispatch or management authority.
use super::{Reply, ReplyBody, Shared, error};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use http::{Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        self, Message,
        protocol::{CloseFrame, Role, WebSocketConfig, frame::coding::CloseCode},
    },
};
use vesta_control::{pack_access::RealtimeSession, packs::PackRelease};

const CONNECTIONS: usize = 32;
const PER_SESSION: usize = 2;
const FRAME_BYTES: usize = 1024;
const HEARTBEAT: Duration = Duration::from_secs(10);
const IDLE: Duration = Duration::from_secs(30);
const SEND_TIMEOUT: Duration = Duration::from_secs(2);
const CONTROL_BURST: u32 = 8;

// One control frame per second sustained, with an eight-frame initial burst.
// Count both Ping and Pong so unsolicited Pong floods cannot bypass the budget.
pub(super) struct ControlRate {
    tokens: u32,
    updated: Instant,
}
impl ControlRate {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            tokens: CONTROL_BURST,
            updated: now,
        }
    }
    pub(super) fn admit(&mut self, now: Instant) -> bool {
        let seconds = now.saturating_duration_since(self.updated).as_secs();
        if seconds != 0 {
            self.tokens =
                (self.tokens + seconds.min(u64::from(CONTROL_BURST)) as u32).min(CONTROL_BURST);
            self.updated += Duration::from_secs(seconds);
        }
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
}

pub(super) struct Sockets {
    budget: Arc<Semaphore>,
    sessions: Arc<Mutex<HashMap<RealtimeSession, usize>>>,
    handles: Mutex<Vec<JoinHandle<()>>>,
}
struct Permit {
    _global: OwnedSemaphorePermit,
    sessions: Arc<Mutex<HashMap<RealtimeSession, usize>>>,
    session: RealtimeSession,
}
impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(count) = sessions.get_mut(&self.session)
        {
            *count -= 1;
            if *count == 0 {
                sessions.remove(&self.session);
            }
        }
    }
}
impl Sockets {
    pub(super) fn new() -> Self {
        Self {
            budget: Arc::new(Semaphore::new(CONNECTIONS)),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            handles: Mutex::new(Vec::with_capacity(CONNECTIONS)),
        }
    }
    pub(super) fn active(&self) -> usize {
        CONNECTIONS - self.budget.available_permits()
    }
    fn acquire(&self, session: RealtimeSession) -> Result<Permit, ()> {
        let global = self.budget.clone().try_acquire_owned().map_err(|_| ())?;
        let mut sessions = self.sessions.lock().map_err(|_| ())?;
        let count = sessions.entry(session).or_default();
        if *count >= PER_SESSION {
            return Err(());
        }
        *count += 1;
        Ok(Permit {
            _global: global,
            sessions: self.sessions.clone(),
            session,
        })
    }
    pub(super) async fn reap(&self, all: bool) {
        let handles = {
            let Ok(mut tasks) = self.handles.lock() else {
                return;
            };
            let mut handles = Vec::new();
            let mut index = 0;
            while index < tasks.len() {
                if all || tasks[index].is_finished() {
                    handles.push(tasks.swap_remove(index));
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

pub(super) async fn upgrade(mut request: Request<Incoming>, state: Arc<Shared>) -> Reply {
    let installed = request
        .uri()
        .query()
        .and_then(|q| q.strip_prefix("installed="))
        .filter(|s| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u64>().ok());
    let Some(installed) = installed else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    if request.version() != http::Version::HTTP_11
        || request.headers().contains_key("cookie")
        || request.headers().contains_key("transfer-encoding")
        || request
            .headers()
            .get("content-length")
            .is_some_and(|v| v != "0")
    {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let secret = if request.headers().get_all("authorization").iter().count() == 1 {
        request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .filter(|v| {
                v.len() == 43
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            .map(|v| zeroize::Zeroizing::new(v.to_owned()))
    } else {
        None
    };
    let Some(secret) = secret else {
        return error(StatusCode::UNAUTHORIZED, "pack_session_invalid");
    };
    // Tungstenite supplies the RFC handshake validation and accept calculation.
    // Its cached version uses http 0.2; copy observed headers into that API.
    let mut handshake = tungstenite::http::Request::builder()
        .method("GET")
        .uri("/api/v2/realtime");
    for name in [
        "connection",
        "upgrade",
        "sec-websocket-version",
        "sec-websocket-key",
    ] {
        if request.headers().get_all(name).iter().count() != 1 {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let value = request.headers().get(name).expect("one header");
        handshake = handshake.header(name, value.as_bytes());
    }
    let key = request
        .headers()
        .get("sec-websocket-key")
        .expect("one header");
    if base64::engine::general_purpose::STANDARD
        .decode(key.as_bytes())
        .map_or(true, |key| key.len() != 16)
    {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let Ok(handshake) = handshake.body(()) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let Ok(accept) = tungstenite::handshake::server::create_response(&handshake) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    state.jobs.reap(false).await;
    state.realtime.reap(false).await;
    let host = state.host.clone();
    let redeemed = state
        .jobs
        .run(move || {
            host.with_policy(|p| {
                host.sessions()
                    .redeem_realtime(secret.as_str(), Instant::now(), p)
            })
        })
        .await;
    let Ok(Ok(Ok((reference, session)))) = redeemed else {
        return error(StatusCode::UNAUTHORIZED, "pack_session_invalid");
    };
    let Ok(permit) = state.realtime.acquire(reference) else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "realtime_busy");
    };
    let on_upgrade = hyper::upgrade::on(&mut request);
    let mut reply = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header(
            "sec-websocket-accept",
            accept
                .headers()
                .get("sec-websocket-accept")
                .expect("validated handshake")
                .as_bytes(),
        )
        .header("cache-control", "no-store")
        .body(ReplyBody::Bytes(hyper::body::Bytes::new()))
        .expect("fixed upgrade headers");
    reply.headers_mut().remove("content-length");
    // Accepted HTTP tasks are aborted/joined first on shutdown, so no socket can
    // be appended after the owner begins joining this list.
    let Ok(mut handles) = state.realtime.handles.lock() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "realtime_busy");
    };
    if handles.len() >= CONNECTIONS || *state.stop.borrow() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "realtime_busy");
    }
    let owned = state.clone();
    handles.push(tokio::spawn(async move {
        let _permit = permit;
        let mut stop = owned.stop.clone();
        let upgraded = tokio::select! {
            biased;
            _ = stop.changed() => return,
            result = tokio::time::timeout(owned.timeout, on_upgrade) => {
                let Ok(Ok(stream)) = result else { return; };
                stream
            }
        };
        let config = WebSocketConfig {
            write_buffer_size: 0,
            max_write_buffer_size: FRAME_BYTES * 2,
            max_message_size: Some(FRAME_BYTES),
            max_frame_size: Some(FRAME_BYTES),
            ..WebSocketConfig::default()
        };
        let socket =
            WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config))
                .await;
        connected(socket, owned, reference, session.expires_at(), installed).await;
    }));
    reply
}

type Socket = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;
async fn send(socket: &mut Socket, message: Message) -> Result<(), ()> {
    tokio::time::timeout(SEND_TIMEOUT, socket.send(message))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}
async fn snapshot(
    state: &Arc<Shared>,
    reference: RealtimeSession,
) -> Result<Option<PackRelease>, ()> {
    // A broadcast wakes up to CONNECTIONS tasks at once. Wait within a fixed
    // deadline for the shared two-worker budget instead of dropping healthy
    // clients just because the other sockets are checking the same publication.
    // These waiters are bounded by socket admission; no work queue is allocated.
    tokio::time::timeout(SEND_TIMEOUT.min(state.timeout), async {
        loop {
            if *state.stop.borrow() {
                return Err(());
            }
            state.realtime_jobs.reap(false).await;
            let host = state.host.clone();
            if let Ok(result) = state
                .realtime_jobs
                .run(move || {
                    host.with_policy(|policy| {
                        host.sessions()
                            .recheck_realtime(reference, Instant::now(), policy)
                    })
                    .map_err(|_| ())?
                    .map_err(|_| ())?;
                    host.releases().current().map_err(|_| ())
                })
                .await
            {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .map_err(|_| ())?
}
fn notice(kind: &str, release: Option<&PackRelease>, installed: u64) -> Message {
    // No hosted release has revision zero; the null status explicitly represents
    // unpublished state rather than fabricating an update decision.
    let value = match release {
        Some(release) => {
            serde_json::json!({"type": kind, "revision": release.revision(), "minimum_revision": release.minimum_revision(), "status": release.status_for(installed).wire()})
        }
        None => {
            serde_json::json!({"type": kind, "revision": 0, "minimum_revision": 0, "status": null})
        }
    };
    Message::Text(value.to_string())
}
async fn connected(
    mut socket: Socket,
    state: Arc<Shared>,
    reference: RealtimeSession,
    expires: Instant,
    installed: u64,
) {
    let mut changes = state.host.releases().subscribe();
    let mut stop = state.stop.clone();
    let close = async {
        if *stop.borrow() { return CloseCode::Away; }
        let Ok(release) = snapshot(&state, reference).await else { return CloseCode::Policy; };
        let mut last_revision = release.as_ref().map_or(0, PackRelease::revision);
        if send(&mut socket, notice("hello", release.as_ref(), installed)).await.is_err() { return CloseCode::Away; }
        let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + HEARTBEAT, HEARTBEAT);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut activity = Instant::now();
        let mut controls = ControlRate::new(activity);
        loop {
            tokio::select! {
                biased;
                _ = stop.changed() => return CloseCode::Away,
                _ = tokio::time::sleep_until(expires.into()) => return CloseCode::Policy,
                _ = tokio::time::sleep_until((activity + IDLE).into()) => return CloseCode::Policy,
                change = changes.changed() => {
                    if change.is_err() { return CloseCode::Away; }
                    let Ok(release) = snapshot(&state, reference).await else { return CloseCode::Policy; };
                    let revision = release.as_ref().map_or(0, PackRelease::revision);
                    if revision != last_revision {
                        if send(&mut socket, notice("pack_update", release.as_ref(), installed)).await.is_err() { return CloseCode::Away; }
                        last_revision = revision;
                    }
                }
                _ = heartbeat.tick() => {
                    if snapshot(&state, reference).await.is_err() { return CloseCode::Policy; }
                    if send(&mut socket, Message::Text("{\"type\":\"heartbeat\"}".to_owned())).await.is_err()
                        || send(&mut socket, Message::Ping(Vec::new())).await.is_err() { return CloseCode::Away; }
                }
                message = socket.next() => match message {
                    Some(Ok(Message::Ping(_))) => {
                        if !controls.admit(Instant::now()) { return CloseCode::Policy; }
                        activity = Instant::now();
                        // Tungstenite queues the matching Pong; flush it with the same
                        // bounded send deadline rather than buffering indefinitely.
                        if !matches!(tokio::time::timeout(SEND_TIMEOUT, socket.flush()).await, Ok(Ok(()))) { return CloseCode::Away; }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        if !controls.admit(Instant::now()) { return CloseCode::Policy; }
                        activity = Instant::now();
                    },
                    Some(Ok(Message::Close(_))) => {
                        // The codec has queued the peer close reply. A second
                        // Close write is rejected while closing; flush instead.
                        let _ = tokio::time::timeout(SEND_TIMEOUT, socket.flush()).await;
                        return CloseCode::Normal;
                    }
                    None => return CloseCode::Normal,
                    Some(Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed)) => return CloseCode::Normal,
                    _ => return CloseCode::Policy,
                }
            }
        }
    }.await;
    // Close is best effort and bounded; do not wait for an uncooperative peer's
    // reply. The owned task and permits finish after the send deadline at latest.
    let _ = send(
        &mut socket,
        Message::Close(Some(CloseFrame {
            code: close,
            reason: "".into(),
        })),
    )
    .await;
}
