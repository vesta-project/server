//! Small versioned local-only JSON protocol. No arbitrary commands, paths or stdin.
use crate::local_files::{self, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use vesta_control::operations::{LocalAction, LocalRuntime};
const MAX_FRAME: usize = 1024;
const IO_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Start,
    Stop,
    Restart,
}
impl From<Op> for LocalAction {
    fn from(op: Op) -> Self {
        match op {
            Op::Start => Self::Start,
            Op::Stop => Self::Stop,
            Op::Restart => Self::Restart,
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u8,
    op: Op,
    name: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Code {
    Complete,
    InvalidRequest,
    NameMismatch,
    EulaPending,
    RuntimeRefused,
    StopTimedOutNoForceKill,
    OutcomeUncertain,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    version: u8,
    code: Code,
}
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    let length = reader
        .read_u32()
        .await
        .map_err(|_| "local_control_invalid_frame")? as usize;
    if length == 0 || length > MAX_FRAME {
        return Err("local_control_invalid_frame");
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|_| "local_control_invalid_frame")?;
    Ok(bytes)
}
async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(writer: &mut W, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| "local_control_invalid_frame")?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err("local_control_invalid_frame");
    }
    writer
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|_| "local_control_io_failed")?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|_| "local_control_io_failed")?;
    writer.flush().await.map_err(|_| "local_control_io_failed")
}
fn decode(bytes: &[u8]) -> Result<Request> {
    let request: Request =
        serde_json::from_slice(bytes).map_err(|_| "local_control_invalid_request")?;
    if request.version != 1 || !crate::cli::valid_name(&request.name) {
        return Err("local_control_invalid_request");
    }
    Ok(request)
}
#[cfg(windows)]
fn endpoint(state: &Path) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let account = crate::pipe_security::account()?;
    let mut digest = Sha256::new();
    digest.update(account.as_bytes());
    digest.update(state.to_string_lossy().as_bytes());
    Ok(PathBuf::from(format!(
        r"\\.\pipe\vesta-agent-{:x}",
        digest.finalize()
    )))
}
#[cfg(target_os = "linux")]
fn endpoint(state: &Path) -> Result<PathBuf> {
    local_files::guard(state)?;
    Ok(state.join("local-control.sock"))
}
#[cfg(not(any(windows, target_os = "linux")))]
fn endpoint(_state: &Path) -> Result<PathBuf> {
    Err("local_platform_unsupported")
}
async fn serve_stream<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    name: &str,
    game: &Path,
    runtime: &LocalRuntime,
) -> Result<()> {
    let request = match tokio::time::timeout(IO_WAIT, read_frame(stream)).await {
        Ok(Ok(bytes)) => decode(&bytes),
        _ => Err("local_control_invalid_frame"),
    };
    let code = match request {
        Err(_) => Code::InvalidRequest,
        Ok(request) if request.name != name => Code::NameMismatch,
        Ok(request) if !matches!(request.op, Op::Stop) && !local_files::eula_accepted(game) => {
            Code::EulaPending
        }
        Ok(request) => {
            let runtime = runtime.clone();
            // Retain and await the owned blocking request; a client disconnect is
            // never cancellation and cannot cause duplicate/automatic retries.
            match tokio::task::spawn_blocking(move || runtime.request(request.op.into())).await {
                Ok(Ok(())) => Code::Complete,
                Ok(Err("local_stop_timed_out_no_force_kill")) => Code::StopTimedOutNoForceKill,
                Ok(Err("local_outcome_uncertain")) | Err(_) => Code::OutcomeUncertain,
                _ => Code::RuntimeRefused,
            }
        }
    };
    tokio::time::timeout(IO_WAIT, write_frame(stream, &Response { version: 1, code }))
        .await
        .map_err(|_| "local_control_io_failed")?
}
#[cfg(target_os = "linux")]
struct SocketCleanup {
    path: PathBuf,
    device: u64,
    inode: u64,
}
#[cfg(target_os = "linux")]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        // Remove only the socket we created, never an unknown replacement file.
        if local_files::guard(&self.path).is_ok()
            && std::fs::symlink_metadata(&self.path).is_ok_and(|m| {
                m.file_type().is_socket()
                    && m.dev() == self.device
                    && m.ino() == self.inode
                    && m.uid() == linux_uid()
            })
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
pub struct Server {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<()>>,
}
impl Server {
    pub async fn start(
        state: &Path,
        name: String,
        game: PathBuf,
        runtime: LocalRuntime,
    ) -> Result<Self> {
        if !crate::cli::valid_name(&name) {
            return Err("local_control_name_invalid");
        }
        let endpoint = endpoint(state)?;
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        #[cfg(windows)]
        let listener = crate::pipe_security::create(&endpoint, true)?;
        #[cfg(target_os = "linux")]
        let (listener, cleanup) = {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata =
                std::fs::symlink_metadata(state).map_err(|_| "local_control_acl_refused")?;
            let uid = linux_uid();
            if metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0 {
                return Err("local_control_acl_refused");
            }
            // Refuse stale endpoints rather than unlink an unknown socket.
            let listener = tokio::net::UnixListener::bind(&endpoint)
                .map_err(|_| "local_control_bind_failed")?;
            let metadata =
                std::fs::symlink_metadata(&endpoint).map_err(|_| "local_control_acl_refused")?;
            let cleanup = SocketCleanup {
                path: endpoint.clone(),
                device: metadata.dev(),
                inode: metadata.ino(),
            };
            std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| "local_control_acl_refused")?;
            (listener, cleanup)
        };
        #[cfg(any(windows, target_os = "linux"))]
        let task = tokio::spawn(async move {
            #[cfg(windows)]
            let mut listener = listener;
            let result = async {
                loop {
                    #[cfg(windows)]
                    let mut stream = {
                        tokio::select! { _ = &mut stopped => break, result = listener.connect() => result.map_err(|_| "local_control_accept_failed")? }
                        // Keep a protected instance alive continuously, before dropping the connected one.
                        let next = crate::pipe_security::create(&endpoint, false)?;
                        std::mem::replace(&mut listener, next)
                    };
                    #[cfg(target_os = "linux")]
                    let mut stream = {
                        let (stream, _) = tokio::select! { _ = &mut stopped => break, result = listener.accept() => result.map_err(|_| "local_control_accept_failed")? };
                        if stream.peer_cred().map_err(|_| "local_control_acl_refused")?.uid() != linux_uid() { continue; }
                        stream
                    };
                    let _ = serve_stream(&mut stream, &name, &game, &runtime).await;
                }
                Ok(())
            }.await;
            #[cfg(target_os = "linux")]
            {
                drop(listener);
                drop(cleanup);
            }
            result
        });
        #[cfg(not(any(windows, target_os = "linux")))]
        let task = {
            let _ = (endpoint, stop, stopped, name, game, runtime);
            return Err("local_platform_unsupported");
        };
        Ok(Self {
            stop: Some(stop),
            task,
        })
    }
    pub async fn shutdown(mut self) -> Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.await.map_err(|_| "local_control_worker_failed")?
    }
}
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn linux_uid() -> u32 {
    // SAFETY: geteuid has no pointers and observes only the current process identity.
    unsafe { libc::geteuid() }
}
pub async fn request(state: &Path, name: String, op: Op) -> Result<()> {
    let endpoint = endpoint(state)?;
    #[cfg(windows)]
    let mut stream = {
        let client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&endpoint)
            .map_err(|_| "local_agent_not_running_start_serve_first")?;
        crate::pipe_security::verify_server(&client)?;
        client
    };
    #[cfg(target_os = "linux")]
    let mut stream = {
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
        let parent = std::fs::symlink_metadata(state)
            .map_err(|_| "local_agent_not_running_start_serve_first")?;
        let meta = std::fs::symlink_metadata(&endpoint)
            .map_err(|_| "local_agent_not_running_start_serve_first")?;
        if !meta.file_type().is_socket()
            || meta.uid() != linux_uid()
            || meta.permissions().mode() & 0o077 != 0
            || parent.uid() != linux_uid()
            || parent.permissions().mode() & 0o077 != 0
        {
            return Err("local_control_acl_refused");
        }
        let client = tokio::net::UnixStream::connect(endpoint)
            .await
            .map_err(|_| "local_agent_not_running_start_serve_first")?;
        if client
            .peer_cred()
            .map_err(|_| "local_control_acl_refused")?
            .uid()
            != linux_uid()
        {
            return Err("local_control_acl_refused");
        }
        client
    };
    #[cfg(any(windows, target_os = "linux"))]
    {
        tokio::time::timeout(
            IO_WAIT,
            write_frame(
                &mut stream,
                &Request {
                    version: 1,
                    op,
                    name,
                },
            ),
        )
        .await
        .map_err(|_| "local_control_io_failed")??;
        // Native stop may take ten seconds; never retry a possibly admitted request.
        let bytes =
            tokio::time::timeout(std::time::Duration::from_secs(30), read_frame(&mut stream))
                .await
                .map_err(|_| "local_control_outcome_unknown_do_not_retry")?
                .map_err(|_| "local_control_outcome_unknown_do_not_retry")?;
        let response: Response =
            serde_json::from_slice(&bytes).map_err(|_| "local_control_invalid_response")?;
        if response.version != 1 {
            return Err("local_control_invalid_response");
        }
        match response.code {
            Code::Complete => Ok(()),
            Code::EulaPending => Err("local_eula_pending_owner_must_accept_themselves"),
            Code::StopTimedOutNoForceKill => Err("local_stop_timed_out_no_force_kill"),
            Code::OutcomeUncertain => Err("local_control_outcome_unknown_do_not_retry"),
            _ => Err("local_control_request_refused"),
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (endpoint, name, op);
        Err("local_platform_unsupported")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_versioned_json_rejects_unknown_ops_and_fields() {
        for bytes in [
            br#"{"version":1,"op":"kill","name":"owned"}"#.as_slice(),
            br#"{"version":2,"op":"stop","name":"owned"}"#,
            br#"{"version":1,"op":"start","name":"../owned"}"#,
            br#"{"version":1,"op":"stop","name":"owned","command":"arbitrary"}"#,
        ] {
            assert!(decode(bytes).is_err());
        }
        assert!(decode(br#"{"version":1,"op":"restart","name":"owned"}"#).is_ok());
    }
    #[test]
    fn frames_truncated_oversized_empty_and_round_trip() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            for bytes in [
                vec![0, 0],
                vec![0, 0, 0, 4, b'x'],
                vec![0, 0, 0, 0],
                vec![0, 0, 4, 1],
            ] {
                assert!(read_frame(&mut bytes.as_slice()).await.is_err());
            }
            let mut bytes = Vec::new();
            write_frame(
                &mut bytes,
                &Request {
                    version: 1,
                    op: Op::Stop,
                    name: "owned".into(),
                },
            )
            .await
            .unwrap();
            let read = read_frame(&mut bytes.as_slice()).await.unwrap();
            assert!(matches!(decode(&read).unwrap().op, Op::Stop));
        });
    }
}
