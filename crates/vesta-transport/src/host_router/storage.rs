use super::*;
use std::{path::PathBuf, sync::Weak, time::Instant};
use vesta_control::machine::storage::{self, HostStorage};

const PERIOD: Duration = Duration::from_secs(15 * 60);
const MIN_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(super) struct Measurements {
    java_cache: Option<PathBuf>,
    inner: Mutex<State>,
}
#[derive(Default)]
struct State {
    snapshot: Arc<HostStorage>,
    running: bool,
    requested: bool,
    last_started: Option<Instant>,
    last_revision: Option<u64>,
}

impl Measurements {
    pub(super) fn with_java_cache(java_cache: PathBuf) -> Self {
        Self {
            java_cache: Some(java_cache),
            ..Self::default()
        }
    }

    pub(super) fn removed_size(&self, id: Uuid) -> Option<u64> {
        self.inner
            .lock()
            .ok()?
            .snapshot
            .removed_sizes
            .get(&id)
            .copied()
    }

    pub(super) fn active_size(&self, id: Uuid) -> Option<u64> {
        self.inner.lock().ok()?.snapshot.active_size(id)
    }

    pub(super) fn refresh(&self) {
        if let Ok(mut state) = self.inner.lock() {
            state.requested = true;
        }
    }

    fn snapshot(&self) -> Reply {
        let (snapshot, measuring) = match self.inner.lock() {
            Ok(state) => (state.snapshot.clone(), state.running || state.requested),
            Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
        };
        let mut value = serde_json::to_value(&*snapshot).expect("storage wire fields");
        value["measuring"] = json!(measuring);
        routes::json(value)
    }

    fn request(&self) -> Reply {
        let Ok(mut state) = self.inner.lock() else {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
        };
        if !state.running && !state.requested {
            if let Some(started) = state.last_started {
                let wait = MIN_INTERVAL.saturating_sub(started.elapsed());
                if !wait.is_zero() {
                    return with_retry_after(
                        failure(StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
                        Duration::from_secs(wait.as_secs() + 1),
                    );
                }
            }
            state.requested = true;
        }
        let mut reply = routes::json(json!({"measuring":true}));
        *reply.status_mut() = StatusCode::ACCEPTED;
        reply
    }

    fn begin(&self, revision: Option<u64>) -> bool {
        let Ok(mut state) = self.inner.lock() else {
            return false;
        };
        let due = state.last_started.is_none_or(|last| {
            let elapsed = last.elapsed();
            elapsed >= PERIOD || (elapsed >= MIN_INTERVAL && revision != state.last_revision)
        });
        if state.running || (!due && !state.requested) {
            return false;
        }
        state.running = true;
        state.requested = false;
        state.last_started = Some(Instant::now());
        state.last_revision = revision;
        true
    }

    fn finish(&self, result: Option<HostStorage>) {
        if let Ok(mut state) = self.inner.lock() {
            if let Some(result) = result.filter(|_| !state.requested) {
                state.snapshot = Arc::new(result);
            }
            state.running = false;
        }
    }
}

pub(super) fn start(app: Weak<Application>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let Some(router) = app.upgrade().map(|app| app.router.clone()) else {
                return;
            };
            if router.is_stopping() || router.imports().is_none() {
                return;
            }
            let _ = tokio::task::spawn_blocking(move || {
                let host = &router.identity.state;
                let revision = host.snapshot().ok().map(|snapshot| snapshot.revision);
                if !router.storage.begin(revision) {
                    return;
                }
                let result = router.imports().and_then(|jobs| {
                    storage::measure(
                        host,
                        jobs.servers_root(),
                        router.storage.java_cache.as_deref(),
                    )
                    .ok()
                });
                router.storage.finish(result);
            })
            .await;
        }
    });
}

pub(super) fn dispatch(router: &HostRouter, method: &Method, path: &str, body: &[u8]) -> Reply {
    if router.imports().is_none() {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
    }
    match (method, path) {
        (&Method::GET, "/v1/host/storage") => router.storage.snapshot(),
        (&Method::POST, "/v1/host/storage/measure") => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Measure {}
            if !body.is_empty() && serde_json::from_slice::<Measure>(body).is_err() {
                return failure(StatusCode::BAD_REQUEST, "invalid_request");
            }
            router.storage.request()
        }
        _ => failure(StatusCode::NOT_FOUND, "not_found"),
    }
}
