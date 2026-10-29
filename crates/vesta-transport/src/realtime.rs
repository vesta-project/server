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
    let console = request.uri().path() == "/api/v3/manage/console/stream";
    let follow = request.uri().path() == "/api/v3/manage/files/follow";
    let metrics = request.uri().path() == "/api/v3/manage/metrics/stream";
    let management = console || follow || metrics;
    let error = |status, code| {
        if management {
            super::manage::error(
                status,
                match code {
                    "pack_session_invalid" => "management_session_invalid",
                    "realtime_busy" => "temporarily_unavailable",
                    "browser_origin_forbidden" => "management_forbidden",
                    other => other,
                },
            )
        } else {
            error(status, code)
        }
    };
    // Native clients only until an explicit browser-origin policy exists. CORS
    // does not protect WebSocket upgrades; never reflect or trust a caller's
    // Origin (including null/loopback), and refuse before consuming a ticket.
    if request.headers().contains_key("origin") {
        return error(StatusCode::FORBIDDEN, "browser_origin_forbidden");
    }
    let installed = request
        .uri()
        .query()
        .and_then(|q| q.strip_prefix("installed="))
        .filter(|s| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u64>().ok());
    let follow_target = if follow {
        match super::manage::files::follow_query(request.uri().query()) {
            Some(target) => Some(target),
            None => return error(StatusCode::BAD_REQUEST, "invalid_request"),
        }
    } else {
        None
    };
    if management
        && ["range", "if-range", "if-match", "if-none-match"]
            .iter()
            .any(|name| request.headers().contains_key(*name))
    {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let installed = if follow || ((console || metrics) && request.uri().query().is_none()) {
        Some(0)
    } else {
        installed
    };
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
    let console_secret = management.then(|| zeroize::Zeroizing::new(secret.as_str().to_owned()));
    let admission_target = follow_target.clone();
    let session = if management {
        let admitted = state
            .jobs
            .run(move || {
                if let Some((path, _)) = admission_target {
                    let subject = host
                        .management()
                        .authenticate(secret.as_str())
                        .map_err(|e| Box::new(super::manage::management_error(e)))?;
                    host.files()
                        .check_remote_surface(
                            &subject,
                            &[vesta_control::identity::Capability::FilesRead],
                        )
                        .map_err(|e| Box::new(super::manage::files::file_error(e)))?;
                    host.files()
                        .open_stream(&subject, &path)
                        .map_err(|e| Box::new(super::manage::files::file_error(e)))?;
                } else {
                    host.management()
                        .authorize(
                            secret.as_str(),
                            if metrics {
                                vesta_control::identity::Capability::MetricsView
                            } else {
                                vesta_control::identity::Capability::ConsoleRead
                            },
                            None,
                        )
                        .map_err(|e| Box::new(super::manage::management_error(e)))?;
                }
                host.management()
                    .pack_reference(Some(secret.as_str()))
                    .map_err(|e| Box::new(super::manage::management_error(e)))
            })
            .await;
        let reference = match admitted {
            Ok(Ok(reference)) => reference,
            Ok(Err(reply)) => return *reply,
            _ => return error(StatusCode::SERVICE_UNAVAILABLE, "realtime_busy"),
        };
        (reference, None)
    } else {
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
        (reference, Some(session.expires_at()))
    };
    let (reference, expires) = session;
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
            max_write_buffer_size: if management {
                128 * 1024
            } else {
                FRAME_BYTES * 2
            },
            max_message_size: Some(FRAME_BYTES),
            max_frame_size: Some(FRAME_BYTES),
            ..WebSocketConfig::default()
        };
        let socket =
            WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config))
                .await;
        if let Some(secret) = console_secret {
            if let Some((path, cursor)) = follow_target {
                connected_follow(socket, owned, secret, path, cursor).await;
            } else if metrics {
                connected_metrics(socket, owned, secret).await;
            } else {
                connected_console(socket, owned, secret).await;
            }
        } else {
            connected(
                socket,
                owned,
                reference,
                expires.expect("pack session"),
                installed,
            )
            .await;
        }
    }));
    reply
}

type Socket = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;
async fn send<S>(socket: &mut S, message: Message) -> Result<(), ()>
where
    S: futures_util::Sink<Message> + Unpin,
{
    tokio::time::timeout(SEND_TIMEOUT, socket.send(message))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}
/// Why a socket check could not produce a snapshot. Only an authority failure
/// is a policy close; saturation and transient state errors are retryable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Refusal {
    Stopping,
    /// No realtime worker became free within the deadline (1013 Try Again Later).
    Busy,
    /// The session expired, was revoked or the policy no longer grants it.
    Revoked,
    /// Policy or release state could not be read (1011 Internal Error).
    Unavailable,
}
impl Refusal {
    pub(super) const fn close_code(self) -> CloseCode {
        match self {
            Self::Stopping => CloseCode::Away,
            Self::Busy => CloseCode::Again,
            Self::Revoked => CloseCode::Policy,
            Self::Unavailable => CloseCode::Error,
        }
    }
}

/// Run `make()`'s action on the bounded realtime pool, waiting up to `deadline`
/// for a free worker. A broadcast wakes up to CONNECTIONS tasks at once; waiting
/// briefly avoids dropping healthy clients just because the other sockets are
/// checking the same publication. Waiters are bounded by socket admission; no
/// work queue is allocated.
pub(super) async fn admitted<T, F>(
    jobs: &super::BlockingJobs,
    stop: &tokio::sync::watch::Receiver<bool>,
    deadline: Duration,
    make: impl Fn() -> F,
) -> Result<T, Refusal>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, Refusal> + Send + 'static,
{
    tokio::time::timeout(deadline, async {
        loop {
            if *stop.borrow() {
                return Err(Refusal::Stopping);
            }
            jobs.reap(false).await;
            if let Ok(result) = jobs.run(make()).await {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .map_err(|_| Refusal::Busy)?
}

async fn snapshot(
    state: &Arc<Shared>,
    reference: RealtimeSession,
) -> Result<Option<PackRelease>, Refusal> {
    admitted(
        &state.realtime_jobs,
        &state.stop,
        SEND_TIMEOUT.min(state.timeout),
        || {
            let host = state.host.clone();
            move || {
                host.with_policy(|policy| {
                    host.sessions()
                        .recheck_realtime(reference, Instant::now(), policy)
                })
                .map_err(|_| Refusal::Unavailable)?
                .map_err(|_| Refusal::Revoked)?;
                host.releases().offered().map_err(|_| Refusal::Unavailable)
            }
        },
    )
    .await
}

/// What a client acts on: a change to either value is a `pack_update`.
fn observed(release: Option<&PackRelease>) -> (u64, u64) {
    release.map_or((0, 0), |r| (r.revision(), r.minimum_revision()))
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
        let release = match snapshot(&state, reference).await { Ok(release) => release, Err(refusal) => return refusal.close_code() };
        let mut last = observed(release.as_ref());
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
                    let release = match snapshot(&state, reference).await { Ok(release) => release, Err(refusal) => return refusal.close_code() };
                    if observed(release.as_ref()) != last {
                        if send(&mut socket, notice("pack_update", release.as_ref(), installed)).await.is_err() { return CloseCode::Away; }
                        last = observed(release.as_ref());
                    }
                }
                _ = heartbeat.tick() => {
                    // The watch only covers publications in this process. Compare
                    // the hosted release here too so another process's publish is
                    // delivered within one heartbeat.
                    let release = match snapshot(&state, reference).await { Ok(release) => release, Err(refusal) => return refusal.close_code() };
                    if observed(release.as_ref()) != last {
                        if send(&mut socket, notice("pack_update", release.as_ref(), installed)).await.is_err() { return CloseCode::Away; }
                        last = observed(release.as_ref());
                    }
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

/// Structural console notices only. No raw log content, no unbounded queue;
/// one sampled cursor and the existing codec/send/control/admission budgets.
async fn connected_console(
    mut socket: Socket,
    state: Arc<Shared>,
    secret: zeroize::Zeroizing<String>,
) {
    let mut stop = state.stop.clone();
    let close = async {
        let mut poll = tokio::time::interval(Duration::from_millis(250));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last = None;
        let mut activity = Instant::now();
        let mut controls = ControlRate::new(activity);
        loop {
            tokio::select! {
                biased;
                _ = stop.changed() => return CloseCode::Away,
                _ = tokio::time::sleep_until((activity + IDLE).into()) => return CloseCode::Policy,
                _ = poll.tick() => {
                    let latest = admitted(&state.realtime_jobs, &state.stop, SEND_TIMEOUT.min(state.timeout), || {
                        let host = state.host.clone();
                        let secret = zeroize::Zeroizing::new(secret.as_str().to_owned());
                        move || {
                            host.management().authorize(secret.as_str(), vesta_control::identity::Capability::ConsoleRead, None).map_err(|_| Refusal::Revoked)?;
                            let after = match last { Some(cursor) => cursor, None => host.runtime().console_snapshot(0,1).map_err(|_| Refusal::Unavailable)?.latest_cursor.saturating_sub(16) };
                            let page = host.runtime().console_snapshot(after,16).map_err(|_| Refusal::Unavailable)?;
                            let lines = page.lines.iter().map(|line| Ok(serde_json::json!({"type":"console.line","api_version":"3","cursor":format!("{}.console.{}",host.management().server_id(),line.cursor),"text":super::manage::redact_console(&host,&line.text).map_err(|_| Refusal::Unavailable)?,"truncated":line.truncated}))).collect::<Result<Vec<_>,Refusal>>()?;
                            Ok::<_,Refusal>((page.latest_cursor,page.next_cursor,page.gap,lines))
                        }
                    }).await;
                    let (latest,next,gap,lines) = match latest { Ok(value) => value, Err(refusal) => return refusal.close_code() };
                    if last != Some(latest) {
                        let notice = serde_json::json!({"type":"console.available", "api_version":"3", "cursor":format!("{}.console.{}", state.host.management().server_id(), latest)});
                        if send(&mut socket, Message::Text(notice.to_string())).await.is_err() { return CloseCode::Away; }
                    }
                    if gap {
                        let notice = serde_json::json!({"type":"console.gap","api_version":"3","cursor":format!("{}.console.{}",state.host.management().server_id(),next)});
                        if send(&mut socket,Message::Text(notice.to_string())).await.is_err() { return CloseCode::Away; }
                    }
                    for line in lines { if send(&mut socket,Message::Text(line.to_string())).await.is_err() { return CloseCode::Away; } }
                    last = Some(next);
                }
                message = socket.next() => match message {
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {
                        if !controls.admit(Instant::now()) { return CloseCode::Policy; }
                        activity = Instant::now();
                        if !matches!(tokio::time::timeout(SEND_TIMEOUT, socket.flush()).await, Ok(Ok(()))) { return CloseCode::Away; }
                    }
                    Some(Ok(Message::Close(_))) => {
                        let _ = tokio::time::timeout(SEND_TIMEOUT, socket.flush()).await;
                        return CloseCode::Normal;
                    }
                    None => return CloseCode::Normal,
                    _ => return CloseCode::Policy,
                }
            }
        }
    }.await;
    let _ = send(
        &mut socket,
        Message::Close(Some(CloseFrame {
            code: close,
            reason: "".into(),
        })),
    )
    .await;
}

async fn connected_follow(
    mut socket: Socket,
    state: Arc<Shared>,
    secret: zeroize::Zeroizing<String>,
    path: vesta_control::files::RelativePath,
    mut cursor: Option<u64>,
) {
    let mut stop = state.stop.clone();
    let mut identity: Option<String> = None;
    let mut previous_size = 0;
    let close = async {
        let mut poll = tokio::time::interval(Duration::from_millis(250));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + HEARTBEAT, HEARTBEAT);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut activity = Instant::now();
        let mut controls = ControlRate::new(activity);
        loop {
            tokio::select! {
                biased;
                _ = stop.changed() => return CloseCode::Away,
                _ = tokio::time::sleep_until((activity + IDLE).into()) => return CloseCode::Policy,
                _ = poll.tick() => {
                    let observed = admitted(&state.realtime_jobs, &state.stop, SEND_TIMEOUT.min(state.timeout), || {
                        let host = state.host.clone();
                        let token = zeroize::Zeroizing::new(secret.as_str().to_owned());
                        let path = path.clone();
                        let old_identity = identity.clone();
                        move || {
                            let subject = host.management().authenticate(token.as_str()).map_err(|_| Refusal::Revoked)?;
                            let mut file = host.files().open_stream(&subject, &path).map_err(|e| match e {
                                vesta_control::files::FileError::Control(_) | vesta_control::files::FileError::ProtectedPath | vesta_control::files::FileError::LinkedPath => Refusal::Revoked,
                                _ => Refusal::Unavailable,
                            })?;
                            let size = file.size();
                            let rotated = old_identity.as_ref().is_some_and(|old| old != file.identity()) || (old_identity.is_some() && size < previous_size) || cursor.is_some_and(|offset| offset > size);
                            let offset = if rotated { 0 } else { cursor.unwrap_or(size) };
                            // One 16-KiB raw frame per tick: at most 64 KiB/s, no queue.
                            let bytes = file.read_chunk(offset, 16 * 1024).map_err(|_| Refusal::Unavailable)?;
                            Ok((file.identity().to_owned(), size, file.etag().to_owned(), offset, bytes, rotated))
                        }
                    }).await;
                    let (new_identity,size,etag,offset,bytes,rotated) = match observed { Ok(value) => value, Err(refusal) => return refusal.close_code() };
                    let first = identity.is_none();
                    identity = Some(new_identity); previous_size = size;
                    if first || rotated {
                        let notice = serde_json::json!({"type":if rotated {"files.rotated"} else {"files.available"},"api_version":"3","cursor":offset.to_string(),"size":size.to_string(),"etag":etag});
                        if send(&mut socket, Message::Text(notice.to_string())).await.is_err() { return CloseCode::Away; }
                    }
                    cursor = Some(offset + bytes.len() as u64);
                    if !bytes.is_empty() {
                        let notice = serde_json::json!({"type":"files.append","api_version":"3","from_byte":offset.to_string(),"cursor":cursor.expect("cursor").to_string(),"size":size.to_string(),"etag":etag,"encoding":vesta_control::files::encoding_hint(&bytes),"bytes_base64":base64::engine::general_purpose::STANDARD.encode(&bytes)});
                        if send(&mut socket, Message::Text(notice.to_string())).await.is_err() { return CloseCode::Away; }
                    }
                }
                _ = heartbeat.tick() => { if send(&mut socket, Message::Ping(Vec::new())).await.is_err() { return CloseCode::Away; } }
                message = socket.next() => match message {
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {
                        if !controls.admit(Instant::now()) { return CloseCode::Policy; }
                        activity = Instant::now();
                        if !matches!(tokio::time::timeout(SEND_TIMEOUT, socket.flush()).await, Ok(Ok(()))) { return CloseCode::Away; }
                    }
                    Some(Ok(Message::Close(_))) => { let _ = tokio::time::timeout(SEND_TIMEOUT, socket.flush()).await; return CloseCode::Normal; }
                    None => return CloseCode::Normal,
                    _ => return CloseCode::Policy,
                }
            }
        }
    }.await;
    let _ = send(
        &mut socket,
        Message::Close(Some(CloseFrame {
            code: close,
            reason: "".into(),
        })),
    )
    .await;
}

/// Watch holds one revision, not a queue. On wake we fetch one latest snapshot
/// and at most 32 latest player events; missed revisions are explicitly marked.
async fn metrics_snapshot(
    state: &Arc<Shared>,
    secret: &str,
    after: Option<i64>,
) -> Result<vesta_control::operations::MetricsLive, Refusal> {
    admitted(
        &state.realtime_jobs,
        &state.stop,
        SEND_TIMEOUT.min(state.timeout),
        || {
            let host = state.host.clone();
            let token = zeroize::Zeroizing::new(secret.to_owned());
            move || {
                let subject = host
                    .management()
                    .authorize(
                        token.as_str(),
                        vesta_control::identity::Capability::MetricsView,
                        None,
                    )
                    .map_err(|_| Refusal::Revoked)?;
                host.runtime()
                    .operations_service()
                    .metrics_live(&subject, after)
                    .map_err(|e| {
                        if e.code() == "forbidden" {
                            Refusal::Revoked
                        } else {
                            Refusal::Unavailable
                        }
                    })
            }
        },
    )
    .await
}
/// Every individual frame obtains fresh authority, including a player event
/// after an earlier event stalled in the socket writer. PlayersView revocation
/// filters identities without granting permission merely from the first snapshot.
async fn metrics_authorized(
    state: &Arc<Shared>,
    secret: &str,
    players: bool,
) -> Result<bool, Refusal> {
    admitted(
        &state.realtime_jobs,
        &state.stop,
        SEND_TIMEOUT.min(state.timeout),
        || {
            let host = state.host.clone();
            let token = zeroize::Zeroizing::new(secret.to_owned());
            move || {
                host.management()
                    .authorize(
                        token.as_str(),
                        vesta_control::identity::Capability::MetricsView,
                        None,
                    )
                    .map_err(|_| Refusal::Revoked)?;
                Ok(!players
                    || host
                        .management()
                        .authorize(
                            token.as_str(),
                            vesta_control::identity::Capability::PlayersView,
                            None,
                        )
                        .is_ok())
            }
        },
    )
    .await
}
/// A latest-only status read, independently gated by StatusView. A metrics-only
/// subscriber receives no status, and loss of StatusView does not grant or retain it.
async fn status_snapshot(
    state: &Arc<Shared>,
    secret: &str,
) -> Result<Option<(serde_json::Value, String)>, Refusal> {
    admitted(
        &state.realtime_jobs,
        &state.stop,
        SEND_TIMEOUT.min(state.timeout),
        || {
            let host = state.host.clone();
            let token = zeroize::Zeroizing::new(secret.to_owned());
            move || {
                use vesta_control::identity::{Capability, management::ManagementError};
                host.management()
                    .authorize(token.as_str(), Capability::MetricsView, None)
                    .map_err(|_| Refusal::Revoked)?;
                match host
                    .management()
                    .authorize(token.as_str(), Capability::StatusView, None)
                {
                    Err(ManagementError::Forbidden) => return Ok(None),
                    Err(_) => return Err(Refusal::Revoked),
                    Ok(_) => {}
                }
                let data =
                    super::manage::status::projection(&host).map_err(|_| Refusal::Unavailable)?;
                let tag = super::manage::status::tag(&data);
                Ok(Some((data, tag)))
            }
        },
    )
    .await
}
async fn push_status(
    socket: &mut Socket,
    state: &Arc<Shared>,
    secret: &str,
    last: &mut Option<String>,
) -> Result<(), Refusal> {
    let Some((data, etag)) = status_snapshot(state, secret).await? else {
        *last = None;
        return Ok(());
    };
    if last.as_ref() == Some(&etag) {
        return Ok(());
    }
    let notice =
        serde_json::json!({"type":"status","api_version":"3","etag":etag,"data":data}).to_string();
    if notice.len() > 4096 {
        return Err(Refusal::Unavailable);
    }
    // As with metrics frames, recheck after projection work and immediately
    // before the bounded socket write; a cached projection grants no authority.
    if status_snapshot(state, secret).await?.is_none() {
        *last = None;
        return Ok(());
    }
    send(socket, Message::Text(notice))
        .await
        .map_err(|_| Refusal::Unavailable)?;
    *last = Some(etag);
    Ok(())
}
async fn connected_metrics(
    mut socket: Socket,
    state: Arc<Shared>,
    secret: zeroize::Zeroizing<String>,
) {
    let mut stop = state.stop.clone();
    // Subscribe before the initial read. A concurrent publication remains pending.
    let mut notifications = state.host.runtime().metrics_notifications();
    let close=async {
        let initial=match metrics_snapshot(&state,secret.as_str(),None).await {Ok(v)=>v,Err(e)=>return e.close_code()};
        let mut revision=initial.current.status.revision;
        let mut sampled=initial.current.sample.as_ref().map(|s|s.sampled_at);
        let mut player_cursor=initial.activity_cursor;
        let mut dropped_activity=false;
        if let Err(e)=metrics_authorized(&state,secret.as_str(),false).await {return e.close_code();}
        let notice=serde_json::json!({"type":"metrics.snapshot","api_version":"3","cursor":revision,"columns":initial.current.columns,"status":initial.current.status});
        if send(&mut socket,Message::Text(notice.to_string())).await.is_err() {return CloseCode::Away;}
        let mut status_tag=None;
        if let Err(e)=push_status(&mut socket,&state,secret.as_str(),&mut status_tag).await {return e.close_code();}
        // Status changes independently of telemetry (including disabled sampling).
        // A single latest projection is retained; slow consumers never queue updates.
        let mut status_tick=tokio::time::interval(Duration::from_millis(250));
        status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut heartbeat=tokio::time::interval_at(tokio::time::Instant::now()+HEARTBEAT,HEARTBEAT);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut activity=Instant::now();let mut controls=ControlRate::new(activity);
        loop {
            tokio::select! {
                _=stop.changed()=>return CloseCode::Away,
                _=tokio::time::sleep_until((activity+IDLE).into())=>return CloseCode::Policy,
                _=status_tick.tick()=>{
                    if let Err(e)=push_status(&mut socket,&state,secret.as_str(),&mut status_tag).await {return e.close_code();}
                }
                changed=notifications.changed()=>{
                    if changed.is_err() {return CloseCode::Away;}
                    // Borrow-and-update only the signal; authoritative data is read freshly.
                    let _=*notifications.borrow_and_update();
                    let latest=match metrics_snapshot(&state,secret.as_str(),Some(player_cursor)).await {Ok(v)=>v,Err(e)=>return e.close_code()};
                    let new_revision=latest.current.status.revision;
                    if new_revision>revision.saturating_add(1) || latest.activity_gap || dropped_activity {
                        if let Err(e)=metrics_authorized(&state,secret.as_str(),false).await {return e.close_code();}
                        let notice=serde_json::json!({"type":"metrics.gap","api_version":"3","cursor":new_revision,"mode":"latest_only"});
                        if send(&mut socket,Message::Text(notice.to_string())).await.is_err() {return CloseCode::Away;}
                    }
                    dropped_activity=false;
                    let at=latest.current.sample.as_ref().map(|s|s.sampled_at);
                    if at!=sampled {
                        if let Err(e)=metrics_authorized(&state,secret.as_str(),false).await {return e.close_code();}
                        let notice=serde_json::json!({"type":"metrics.sample","api_version":"3","cursor":new_revision,"columns":latest.current.columns});
                        if send(&mut socket,Message::Text(notice.to_string())).await.is_err() {return CloseCode::Away;}
                        sampled=at;
                    }
                    for event in latest.activity {
                        if notifications.has_changed().unwrap_or(false) {
                            // Never walk an old page while new samples accumulate.
                            dropped_activity=true;break;
                        }
                        match metrics_authorized(&state,secret.as_str(),true).await {Ok(true)=>{},Ok(false)=>break,Err(e)=>return e.close_code()}
                        let notice=serde_json::json!({"type":"metrics.player_activity","api_version":"3","cursor":new_revision,"event":event});
                        if send(&mut socket,Message::Text(notice.to_string())).await.is_err() {return CloseCode::Away;}
                    }
                    revision=new_revision;player_cursor=latest.activity_cursor;
                }
                _=heartbeat.tick()=>{
                    if let Err(e)=metrics_authorized(&state,secret.as_str(),false).await {return e.close_code();}
                    if send(&mut socket,Message::Ping(Vec::new())).await.is_err() {return CloseCode::Away;}
                }
                message=socket.next()=>match message {
                    Some(Ok(Message::Ping(_) | Message::Pong(_)))=>{
                        if !controls.admit(Instant::now()) {return CloseCode::Policy;}
                        activity=Instant::now();
                        if !matches!(tokio::time::timeout(SEND_TIMEOUT,socket.flush()).await,Ok(Ok(()))) {return CloseCode::Away;}
                    }
                    Some(Ok(Message::Close(_)))=>{let _=tokio::time::timeout(SEND_TIMEOUT,socket.flush()).await;return CloseCode::Normal;}
                    None=>return CloseCode::Normal,
                    _=>return CloseCode::Policy,
                }
            }
        }
    }.await;
    let _ = send(
        &mut socket,
        Message::Close(Some(CloseFrame {
            code: close,
            reason: "".into(),
        })),
    )
    .await;
}

#[cfg(test)]
mod file_stream_budget_tests {
    use super::*;
    #[test]
    fn a_slow_consumer_cannot_hold_a_follow_send_indefinitely() {
        struct Stalled;
        impl futures_util::Sink<Message> for Stalled {
            type Error = ();
            fn poll_ready(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), ()>> {
                std::task::Poll::Pending
            }
            fn start_send(self: std::pin::Pin<&mut Self>, _: Message) -> Result<(), ()> {
                unreachable!()
            }
            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), ()>> {
                std::task::Poll::Pending
            }
            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), ()>> {
                std::task::Poll::Pending
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let start = Instant::now();
        assert!(
            runtime
                .block_on(send(&mut Stalled, Message::Text("files.append".into())))
                .is_err()
        );
        assert!(start.elapsed() >= SEND_TIMEOUT);
        assert!(start.elapsed() < SEND_TIMEOUT + Duration::from_secs(2));
    }
}
