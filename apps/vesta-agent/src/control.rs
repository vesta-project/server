//! Small versioned local-only JSON protocol. No arbitrary commands, paths or stdin.
use crate::local_files::{self, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use vesta_control::operations::{LocalAction, LocalRuntime};
const MAX_FRAME: usize = 1024;
const MAX_RESPONSE_FRAME: usize = 1024 * 1024;
const IO_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Op {
    Start,
    Stop,
    Restart,
    Pair { role: String, minutes: u16 },
    DevicesList,
    OwnerLink { uuid: String, role: String },
    OwnerUnlink { uuid: String, revoke_devices: bool },
    OwnerList,
    Superadmin { user: String, grant: bool },
    AccessShow,
    AccessSet { fields: Vec<(String, String)> },
    DevicesRevoke { device: String },
}
impl From<Op> for LocalAction {
    fn from(op: Op) -> Self {
        match op {
            Op::Start => Self::Start,
            Op::Stop => Self::Stop,
            Op::Restart => Self::Restart,
            _ => unreachable!("management ops never reach lifecycle driver"),
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
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    version: u8,
    code: Code,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    read_frame_limit(reader, MAX_FRAME).await
}
async fn read_frame_limit<R: AsyncRead + Unpin>(reader: &mut R, maximum: usize) -> Result<Vec<u8>> {
    let length = reader
        .read_u32()
        .await
        .map_err(|_| "local_control_invalid_frame")? as usize;
    if length == 0 || length > maximum {
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
    if bytes.is_empty() || bytes.len() > MAX_RESPONSE_FRAME {
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
    management: &vesta_control::identity::management::ManagementAccess,
    fingerprint: Option<&vesta_transport::TlsFingerprint>,
    public_endpoint: Option<&str>,
) -> Result<()> {
    let request = match tokio::time::timeout(IO_WAIT, read_frame(stream)).await {
        Ok(Ok(bytes)) => decode(&bytes),
        _ => Err("local_control_invalid_frame"),
    };
    let mut data = None;
    let code = match request {
        Err(_) => Code::InvalidRequest,
        Ok(request) if request.name != name => Code::NameMismatch,
        Ok(request)
            if matches!(
                request.op,
                Op::Pair { .. }
                    | Op::DevicesList
                    | Op::DevicesRevoke { .. }
                    | Op::OwnerLink { .. }
                    | Op::OwnerUnlink { .. }
                    | Op::OwnerList
                    | Op::Superadmin { .. }
                    | Op::AccessShow
                    | Op::AccessSet { .. }
            ) =>
        {
            let management = management.clone();
            let fingerprint = fingerprint.cloned();
            let public_endpoint = public_endpoint.map(str::to_owned);
            match tokio::task::spawn_blocking(move || {
                management_op(
                    &management,
                    request.op,
                    fingerprint.as_ref(),
                    public_endpoint.as_deref(),
                )
            })
            .await
            {
                Ok(Ok(value)) => {
                    data = Some(value);
                    Code::Complete
                }
                _ => Code::RuntimeRefused,
            }
        }
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
    tokio::time::timeout(
        IO_WAIT,
        write_frame(
            stream,
            &Response {
                version: 1,
                code,
                data,
            },
        ),
    )
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
        management: vesta_control::identity::management::ManagementAccess,
        fingerprint: Option<vesta_transport::TlsFingerprint>,
        public_endpoint: Option<String>,
    ) -> Result<Self> {
        if !crate::cli::valid_name(&name) {
            return Err("local_control_name_invalid");
        }
        let endpoint = endpoint(state)?;
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        #[cfg(windows)]
        let listener = crate::pipe_security::create(&endpoint, true, 2)?;
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
                        let next = crate::pipe_security::create(&endpoint, false, 2)?;
                        std::mem::replace(&mut listener, next)
                    };
                    #[cfg(target_os = "linux")]
                    let mut stream = {
                        let (stream, _) = tokio::select! { _ = &mut stopped => break, result = listener.accept() => result.map_err(|_| "local_control_accept_failed")? };
                        if stream.peer_cred().map_err(|_| "local_control_acl_refused")?.uid() != linux_uid() { continue; }
                        stream
                    };
                    let _ = serve_stream(&mut stream, &name, &game, &runtime, &management, fingerprint.as_ref(), public_endpoint.as_deref()).await;
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
            let _ = (
                endpoint,
                stop,
                stopped,
                name,
                game,
                runtime,
                management,
                fingerprint,
                public_endpoint,
            );
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
    request_data(state, name, op).await.map(|_| ())
}
pub async fn request_data(state: &Path, name: String, op: Op) -> Result<Option<serde_json::Value>> {
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
        let bytes = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            read_frame_limit(&mut stream, MAX_RESPONSE_FRAME),
        )
        .await
        .map_err(|_| "local_control_outcome_unknown_do_not_retry")?
        .map_err(|_| "local_control_outcome_unknown_do_not_retry")?;
        let response: Response =
            serde_json::from_slice(&bytes).map_err(|_| "local_control_invalid_response")?;
        if response.version != 1 {
            return Err("local_control_invalid_response");
        }
        match response.code {
            Code::Complete => Ok(response.data),
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
/// This is reached only after native pipe/socket admission and server-name binding.
fn management_op(
    service: &vesta_control::identity::management::ManagementAccess,
    op: Op,
    fingerprint: Option<&vesta_transport::TlsFingerprint>,
    public_endpoint: Option<&str>,
) -> Result<serde_json::Value> {
    match op {
        Op::Pair { role, minutes } => {
            let role = crate::cli::pair_role(&role).ok_or("local_control_invalid_request")?;
            let fingerprint = fingerprint
                .map(|f| f.current().map_err(|_| "local_tls_unavailable"))
                .transpose()?;
            let code = service
                .issue_local(role, minutes)
                .map_err(|_| "local_management_refused")?;
            let pairing_link = pairing_link(
                public_endpoint,
                fingerprint.as_deref(),
                code.secret.expose(),
                service.server_id(),
            );
            Ok(
                serde_json::json!({"code": code.secret.expose(), "expires_at_ms": code.expires_at_ms, "server_id": service.server_id(), "role": role.id(), "tls_sha256_fingerprint": fingerprint, "pairing_link": pairing_link}),
            )
        }
        Op::OwnerLink { uuid, role } => {
            let uuid = vesta_control::identity::minecraft::MinecraftUuid::parse(&uuid)
                .map_err(|_| "local_control_invalid_request")?;
            let role = crate::cli::pair_role(&role).ok_or("local_control_invalid_request")?;
            // Resolve fingerprint BEFORE mutation; a failure cannot hide an admitted link.
            let fingerprint = fingerprint
                .map(|f| f.current().map_err(|_| "local_tls_unavailable"))
                .transpose()?;
            service
                .link_local(uuid, role)
                .map_err(|_| "local_management_refused")?;
            Ok(
                serde_json::json!({"linked": true, "minecraft_uuid": uuid.as_uuid().to_string(), "role": role.id(), "server_id": service.server_id(), "tls_sha256_fingerprint": fingerprint}),
            )
        }
        Op::OwnerUnlink {
            uuid,
            revoke_devices,
        } => {
            let uuid = vesta_control::identity::minecraft::MinecraftUuid::parse(&uuid)
                .map_err(|_| "local_control_invalid_request")?;
            service
                .unlink_local(uuid, revoke_devices)
                .map_err(|_| "local_management_refused")?;
            Ok(serde_json::json!({"unlinked": true, "devices_revoked": revoke_devices}))
        }
        Op::AccessShow => serde_json::to_value(
            service
                .access_local()
                .map_err(|_| "local_management_refused")?,
        )
        .map_err(|_| "local_management_refused"),
        Op::AccessSet { fields } => serde_json::to_value(
            service
                .patch_access_local(&fields)
                .map_err(|_| "local_management_refused")?,
        )
        .map_err(|_| "local_management_refused"),
        Op::Superadmin { user, grant } => {
            service
                .superadmin_local(&user, grant)
                .map_err(|_| "local_management_refused")?;
            Ok(serde_json::json!({"user": user, "superadmin": grant}))
        }
        Op::OwnerList => serde_json::to_value(
            service
                .links_local()
                .map_err(|_| "local_management_refused")?,
        )
        .map_err(|_| "local_management_refused"),
        Op::DevicesList => serde_json::to_value(
            service
                .list_local()
                .map_err(|_| "local_management_refused")?,
        )
        .map_err(|_| "local_management_refused"),
        Op::DevicesRevoke { device } => {
            service
                .revoke_local(&device)
                .map_err(|_| "local_management_refused")?;
            Ok(serde_json::json!({"revoked": true}))
        }
        _ => Err("local_control_invalid_request"),
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
            br#"{"version":1,"op":{"pair":{"role":"owner","minutes":10,"command":"arbitrary"}},"name":"owned"}"#,
            br#"{"version":1,"op":{"accessset":{"fields":[],"role":"owner"}},"name":"owned"}"#,
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

/// Serialize all query values, including nested endpoint delimiters, exactly once.
fn pairing_link(
    endpoint: Option<&str>,
    fingerprint: Option<&str>,
    code: &str,
    server: &str,
) -> Option<String> {
    let (endpoint, fingerprint) = (endpoint?, fingerprint?);
    let mut link = url::Url::parse("vesta://pair").expect("fixed pairing URL");
    link.query_pairs_mut()
        .append_pair("endpoint", endpoint)
        .append_pair("code", code)
        .append_pair("fp", fingerprint)
        .append_pair("sid", server);
    Some(link.into())
}

#[cfg(test)]
mod pairing_tests {
    use super::pairing_link;
    #[test]
    fn link_encodes_endpoint_and_requires_both_trust_and_address() {
        let fp = "a".repeat(64);
        let endpoint = "https://[2001:db8::1]:8443/a%20b/";
        let link = pairing_link(Some(endpoint), Some(&fp), "code", "server").unwrap();
        assert!(link.starts_with("vesta://pair?endpoint=https%3A%2F%2F%5B2001%3Adb8%3A%3A1%5D%3A8443%2Fa%2520b%2F&code=code&fp="));
        let url = url::Url::parse(&link).unwrap();
        let pairs: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["endpoint"], endpoint);
        assert_eq!(pairs["fp"], fp);
        assert_eq!(pairs["sid"], "server");
        assert!(pairing_link(None, Some(&fp), "code", "server").is_none());
        assert!(pairing_link(Some(endpoint), None, "code", "server").is_none());
    }
}
