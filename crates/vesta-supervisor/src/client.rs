//! Blocking agent-side client usable from plain threads and async contexts alike.
//!
//! A request is two steps: `prepare` opens and authenticates a fresh protected
//! connection, then `send` writes the request. Callers run authorization guards between
//! the two, so no connection setup or retry happens after a guard succeeded. Work runs
//! on a private runtime; callers wait on bounded channels, never `block_on`. A failure
//! after the request was written is an unknown outcome and must never be retried.
use crate::{
    Result, native,
    wire::{self, Op, Reply},
};
use std::{path::PathBuf, sync::mpsc, time::Duration};
use vesta_state::StateDirectory;

const IO_WAIT: Duration = Duration::from_secs(5);
/// Below the supervisor's delivery wait plus margin for an admitted stdin write.
pub const DELIVERY_WAIT: Duration = Duration::from_secs(15);
const BUSY_RETRY: Duration = Duration::from_millis(20);
const BUSY_ATTEMPTS: u32 = 50;

/// Codes distinguishing "nothing was sent" from "the outcome is unknown".
pub const NOT_RUNNING: &str = "supervisor_not_running";
pub const BUSY: &str = "supervisor_busy";
pub const OUTCOME_UNKNOWN: &str = "supervisor_outcome_unknown";

/// Whether a client error proves the request was never written.
pub fn unsent(error: &str) -> bool {
    error == NOT_RUNNING || error == BUSY
}

pub struct SupervisorClient {
    runtime: Option<tokio::runtime::Runtime>,
    endpoint: PathBuf,
}
impl std::fmt::Debug for SupervisorClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SupervisorClient")
    }
}

/// An authenticated connection waiting for its single request. Dropping it closes the
/// connection without sending anything.
pub struct Prepared {
    go: tokio::sync::oneshot::Sender<(Vec<u8>, Duration)>,
    result: mpsc::Receiver<Result<Reply>>,
}
impl Prepared {
    pub fn send(self, op: Op, wait: Duration) -> Result<Reply> {
        let framed = wire::encode_request(op)?;
        if self.go.send((framed, wait)).is_err() {
            // The connection task ended before anything was written.
            return Err(NOT_RUNNING);
        }
        self.result
            .recv_timeout(wait + IO_WAIT * 2)
            .unwrap_or(Err(OUTCOME_UNKNOWN))
    }
}

impl SupervisorClient {
    pub fn new(state: &StateDirectory) -> Result<Self> {
        let endpoint = native::endpoint(
            state.path(),
            &state.path().join(vesta_state::artifacts::SUPERVISOR_DIR),
        )?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("vesta-supervisor-client")
            .enable_all()
            .build()
            .map_err(|_| "supervisor_client_unavailable")?;
        Ok(Self {
            runtime: Some(runtime),
            endpoint,
        })
    }

    /// Open and authenticate a connection. Errors here mean nothing was sent.
    pub fn prepare(&self) -> Result<Prepared> {
        let endpoint = self.endpoint.clone();
        let (ready, connected) = mpsc::sync_channel(1);
        let (go, request) = tokio::sync::oneshot::channel::<(Vec<u8>, Duration)>();
        let (sender, result) = mpsc::sync_channel(1);
        let runtime = self
            .runtime
            .as_ref()
            .ok_or("supervisor_client_unavailable")?;
        runtime.spawn(async move {
            let mut stream = match tokio::time::timeout(IO_WAIT, open(&endpoint)).await {
                Ok(Ok(stream)) => stream,
                Ok(Err(error)) => {
                    let _ = ready.send(Err(error));
                    return;
                }
                Err(_) => {
                    let _ = ready.send(Err(NOT_RUNNING));
                    return;
                }
            };
            if ready.send(Ok(())).is_err() {
                return;
            }
            // Dropped without a request: close without writing.
            let Ok((framed, wait)) = request.await else {
                return;
            };
            let _ = sender.send(exchange(&mut stream, &framed, wait).await);
        });
        connected
            .recv_timeout(IO_WAIT * 2 + BUSY_RETRY * BUSY_ATTEMPTS)
            .unwrap_or(Err(NOT_RUNNING))?;
        Ok(Prepared { go, result })
    }

    /// One unguarded request. `wait` bounds the reply after the request was written.
    pub fn call(&self, op: Op, wait: Duration) -> Result<Reply> {
        self.prepare()?.send(op, wait)
    }
}
impl Drop for SupervisorClient {
    fn drop(&mut self) {
        // Safe from async contexts; outstanding exchanges are abandoned, not retried.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

async fn open(endpoint: &std::path::Path) -> Result<native::Stream> {
    let mut attempts = 0;
    loop {
        match native::connect(endpoint).await {
            Ok(stream) => return Ok(stream),
            // Every pipe instance is momentarily connected, or the supervisor is between
            // listening instances: retry briefly. A stopped supervisor costs one second.
            Err(BUSY | NOT_RUNNING) if attempts < BUSY_ATTEMPTS => {
                attempts += 1;
                tokio::time::sleep(BUSY_RETRY).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn exchange(stream: &mut native::Stream, framed: &[u8], wait: Duration) -> Result<Reply> {
    tokio::time::timeout(IO_WAIT, wire::write_async(stream, framed))
        .await
        .map_err(|_| OUTCOME_UNKNOWN)?
        .map_err(|_| OUTCOME_UNKNOWN)?;
    let bytes = tokio::time::timeout(
        wait + IO_WAIT,
        wire::read_async(stream, wire::MAX_RESPONSE_FRAME),
    )
    .await
    .map_err(|_| OUTCOME_UNKNOWN)?
    .map_err(|_| OUTCOME_UNKNOWN)?;
    wire::decode_response(&bytes).map_err(|_| OUTCOME_UNKNOWN)
}
