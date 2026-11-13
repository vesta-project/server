//! Separate protected host-local control. H10 starts Server with the SAME
//! HostIdentity used by HTTPS; H11 tightens the service/worker/game OS boundary.
use crate::local_files::{self, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;
use vesta_control::machine::recovery::{LocalPrivilege, RecoveryRequest};
use vesta_transport::host_identity::HostIdentity;
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum LocalRequest {
    SetupCode,
    Status,
    Link,
    InspectPending,
    Inspect { link_id: Uuid },
    Approve { link_id: Uuid, request_hash: String },
    Recover { request: RecoveryRequest },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    data: Option<serde_json::Value>,
    error: Option<String>,
}
fn dispatch(identity: &HostIdentity, request: LocalRequest) -> Response {
    let local = LocalPrivilege::from_privileged_local_channel();
    let result = match request {
        LocalRequest::Status => identity
            .state
            .snapshot()
            .map(|s| serde_json::json!({"hostId":s.host_id,"claimed":s.primary_account_id.is_some()}))
            .map_err(|_| vesta_control::machine::sessions::IdentityFailure("host_local_failed")),
        LocalRequest::SetupCode => identity
            .setup_code(local)
            .map(|code| serde_json::json!({"code":code})),
        LocalRequest::Link => identity.local_link_code(local).and_then(|v| {
            serde_json::to_value(v)
                .map_err(|_| vesta_control::machine::sessions::IdentityFailure("host_local_failed"))
        }),
        LocalRequest::InspectPending => identity
            .local_pending_link(local)
            .and_then(local_link_review),
        LocalRequest::Inspect { link_id } => identity
            .local_link_status(local, link_id)
            .and_then(local_link_review),
        LocalRequest::Approve {
            link_id,
            request_hash,
        } => identity
            .local_approve_link(local, link_id, &request_hash)
            .map(|device| serde_json::json!({"deviceId":device,"state":"approved"})),
        LocalRequest::Recover { request } => identity
            .local_recover(local, &request)
            .map(|device| serde_json::json!({"deviceId":device,"recovered":true})),
    };
    match result {
        Ok(data) => Response {
            data: Some(data),
            error: None,
        },
        Err(e) => Response {
            data: None,
            error: Some(e.0.into()),
        },
    }
}
fn local_link_review(
    status: vesta_control::machine::link::LinkStatus,
) -> vesta_control::machine::sessions::Result<serde_json::Value> {
    let mut value = serde_json::to_value(&status)
        .map_err(|_| vesta_control::machine::sessions::IdentityFailure("host_local_failed"))?;
    value["linkId"] = serde_json::json!(status.link_id);
    Ok(value)
}
async fn read<R: AsyncRead + Unpin>(stream: &mut R) -> Result<Vec<u8>> {
    let n = stream
        .read_u32()
        .await
        .map_err(|_| "host_local_io_failed")? as usize;
    if n == 0 || n > 16384 {
        return Err("host_local_invalid_frame");
    }
    let mut bytes = vec![0; n];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| "host_local_io_failed")?;
    Ok(bytes)
}
async fn write<W: AsyncWrite + Unpin>(stream: &mut W, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| "host_local_invalid_frame")?;
    if bytes.len() > 16384 {
        return Err("host_local_invalid_frame");
    }
    stream
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|_| "host_local_io_failed")?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|_| "host_local_io_failed")?;
    stream.flush().await.map_err(|_| "host_local_io_failed")
}
fn endpoint(state: &Path) -> Result<PathBuf> {
    local_files::guard(state)?;
    #[cfg(windows)]
    {
        use sha2::{Digest, Sha256};
        Ok(PathBuf::from(format!(
            r"\\.\pipe\vesta-host-{:x}",
            Sha256::digest(state.to_string_lossy().to_lowercase().as_bytes())
        )))
    }
    #[cfg(not(windows))]
    {
        Ok(state.join("host-local.sock"))
    }
}
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn uid() -> u32 {
    // SAFETY: geteuid observes only the current process identity.
    unsafe { libc::geteuid() }
}
#[cfg(target_os = "linux")]
fn allowed_peer(peer: u32, service: u32) -> bool {
    peer == 0 || peer == service
}
pub struct Server {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<()>>,
}
impl Server {
    pub async fn start(state: &Path, identity: Arc<HostIdentity>) -> Result<Self> {
        if std::fs::canonicalize(state).map_err(|_| "host_local_acl_refused")?
            != std::fs::canonicalize(identity.state.database().directory().path())
                .map_err(|_| "host_local_acl_refused")?
        {
            return Err("host_local_acl_refused");
        }
        let endpoint = endpoint(state)?;
        #[cfg(windows)]
        let mut listener = {
            if crate::host_local_windows::directory_owner(state)?
                != crate::host_local_windows::account()?
            {
                return Err("host_local_acl_refused");
            }
            crate::host_local_windows::create(&endpoint, true, 2)?
        };
        #[cfg(target_os = "linux")]
        let (listener, cleanup) = {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let m = std::fs::symlink_metadata(state).map_err(|_| "host_local_acl_refused")?;
            if m.uid() != uid() || m.permissions().mode() & 0o077 != 0 {
                return Err("host_local_acl_refused");
            }
            crate::control::bind_control(&endpoint)?
        };
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        #[cfg(any(windows, target_os = "linux"))]
        let task = tokio::spawn(async move {
            loop {
                #[cfg(windows)]
                let mut stream = {
                    tokio::select! {_= &mut stopped=>break,result=listener.connect()=>result.map_err(|_|"host_local_accept_failed")?}
                    let next = crate::host_local_windows::create(&endpoint, false, 2)?;
                    std::mem::replace(&mut listener, next)
                };
                #[cfg(target_os = "linux")]
                let mut stream = {
                    let (stream, _) = tokio::select! {_= &mut stopped=>break,result=listener.accept()=>result.map_err(|_|"host_local_accept_failed")?};
                    if !allowed_peer(
                        stream
                            .peer_cred()
                            .map_err(|_| "host_local_acl_refused")?
                            .uid(),
                        uid(),
                    ) {
                        continue;
                    }
                    stream
                };
                let bytes =
                    match tokio::time::timeout(Duration::from_secs(2), read(&mut stream)).await {
                        Ok(Ok(bytes)) => bytes,
                        _ => continue,
                    };
                let request = match serde_json::from_slice::<LocalRequest>(&bytes) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let response = dispatch(&identity, request);
                let _ = tokio::time::timeout(Duration::from_secs(2), write(&mut stream, &response))
                    .await;
            }
            #[cfg(target_os = "linux")]
            {
                drop(listener);
                drop(cleanup);
            }
            Ok(())
        });
        #[cfg(not(any(windows, target_os = "linux")))]
        let task = {
            let _ = (stop, stopped, identity);
            return Err("host_local_platform_unsupported");
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
        self.task.await.map_err(|_| "host_local_failed")?
    }
}
/// Preserve the supervisor's same-uid rule; this separate host-admin channel
/// additionally admits root while retaining an fd-pinned, owner-checked directory.
#[cfg(target_os = "linux")]
fn privileged_socket_address(endpoint: &Path, owner: u32) -> Result<(std::fs::File, PathBuf)> {
    use std::os::{fd::AsRawFd, unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt}};
    let directory = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(endpoint.parent().ok_or("host_local_acl_refused")?)
        .map_err(|_| "host_local_acl_refused")?;
    let metadata = directory.metadata().map_err(|_| "host_local_acl_refused")?;
    if metadata.uid() != owner || !allowed_peer(uid(), owner) || metadata.permissions().mode() & 0o077 != 0 {
        return Err("host_local_acl_refused");
    }
    let address = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
        .join(endpoint.file_name().ok_or("host_local_acl_refused")?);
    Ok((directory, address))
}
pub async fn request(state: &Path, request: LocalRequest) -> Result<serde_json::Value> {
    let endpoint = endpoint(state)?;
    #[cfg(windows)]
    let mut stream = {
        let client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&endpoint)
            .map_err(|_| "host_local_unavailable")?;
        crate::host_local_windows::verify_server(&client, state)?;
        client
    };
    #[cfg(target_os = "linux")]
    let mut stream = {
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
        let parent = std::fs::symlink_metadata(state).map_err(|_| "host_local_acl_refused")?;
        let socket = std::fs::symlink_metadata(&endpoint).map_err(|_| "host_local_unavailable")?;
        if !parent.is_dir()
            || parent.permissions().mode() & 0o077 != 0
            || !allowed_peer(uid(), parent.uid())
            || !socket.file_type().is_socket()
            || socket.uid() != parent.uid()
            || socket.permissions().mode() & 0o077 != 0
        {
            return Err("host_local_acl_refused");
        }
        let (_dir, address) = privileged_socket_address(&endpoint, parent.uid())
            .map_err(|_| "host_local_acl_refused")?;
        let stream = tokio::net::UnixStream::connect(address)
            .await
            .map_err(|_| "host_local_unavailable")?;
        if stream
            .peer_cred()
            .map_err(|_| "host_local_acl_refused")?
            .uid()
            != parent.uid()
        {
            return Err("host_local_acl_refused");
        }
        stream
    };
    #[cfg(any(windows, target_os = "linux"))]
    {
        tokio::time::timeout(Duration::from_secs(2), write(&mut stream, &request))
            .await
            .map_err(|_| "host_local_io_failed")??;
        let bytes = tokio::time::timeout(Duration::from_secs(10), read(&mut stream))
            .await
            .map_err(|_| "host_local_outcome_unknown")??;
        let response: Response =
            serde_json::from_slice(&bytes).map_err(|_| "host_local_invalid_response")?;
        if response.error.is_some() {
            return Err("host_local_request_refused");
        }
        response.data.ok_or("host_local_invalid_response")
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (endpoint, request);
        Err("host_local_platform_unsupported")
    }
}
pub const USAGE: &str = "Vesta Server\n  vesta host-status [--state <host-state>]\n  vesta setup-code [--state <host-state>]\n  vesta link [--state <host-state>]\n  vesta link --approve [<link-id> [<request-hash>]] [--state <host-state>]\n  vesta link --recover <account-uuid> <device-key> <device-name> [--revoke-all] [--state <host-state>]\nOmit the approval hash to inspect the exact signed request first. Recovery keeps other devices unless --revoke-all is supplied.";
fn parse(mut args: Vec<String>) -> Result<(PathBuf, LocalRequest)> {
    let mut state = std::env::var_os("VESTA_HOST_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            #[cfg(windows)]
            {
                PathBuf::from(
                    std::env::var_os("ProgramData").unwrap_or_else(|| "C:/ProgramData".into()),
                )
                .join("Vesta/host")
            }
            #[cfg(not(windows))]
            {
                PathBuf::from("/var/lib/vesta/host")
            }
        });
    if let Some(i) = args.iter().position(|a| a == "--state") {
        if i + 1 >= args.len() {
            return Err("host_local_invalid_arguments");
        }
        state = PathBuf::from(args.remove(i + 1));
        args.remove(i);
    }
    let r = match args.as_slice() {
        [c] if c == "setup-code" => LocalRequest::SetupCode,
        [c] if c == "host-status" => LocalRequest::Status,
        [c] if c == "link" => LocalRequest::Link,
        [c, a] if c == "link" && a == "--approve" => LocalRequest::InspectPending,
        [c, a, id] if c == "link" && a == "--approve" => LocalRequest::Inspect {
            link_id: Uuid::parse_str(id).map_err(|_| "host_local_invalid_arguments")?,
        },
        [c, a, id, hash]
            if c == "link"
                && a == "--approve"
                && hash.len() == 64
                && hash.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            LocalRequest::Approve {
                link_id: Uuid::parse_str(id).map_err(|_| "host_local_invalid_arguments")?,
                request_hash: hash.clone(),
            }
        }
        _ => {
            let revoke = args.last().is_some_and(|a| a == "--revoke-all");
            if revoke {
                args.pop();
            }
            match args.as_slice() {
                [c, r, account, key, name] if c == "link" && r == "--recover" => {
                    LocalRequest::Recover {
                        request: RecoveryRequest {
                            request_id: Uuid::new_v4(),
                            account_id: Uuid::parse_str(account)
                                .map_err(|_| "host_local_invalid_arguments")?,
                            device_key: key.clone(),
                            device_name: name.clone(),
                            revoke_all: revoke,
                        },
                    }
                }
                _ => return Err("host_local_invalid_arguments"),
            }
        }
    };
    Ok((state, r))
}
pub async fn run(args: Vec<std::ffi::OsString>) -> std::process::ExitCode {
    let result = async {
        let args = args
            .into_iter()
            .map(|a| a.into_string().map_err(|_| "host_local_invalid_arguments"))
            .collect::<Result<Vec<_>>>()?;
        let (state, r) = parse(args)?;
        let state = local_files::absolute(
            &state,
            &std::env::current_dir().map_err(|_| "host_local_invalid_arguments")?,
        )?;
        let interactive_approval=matches!(r,LocalRequest::InspectPending);
        let value=request(&state, r).await?;
        if interactive_approval {
            use std::io::{IsTerminal,Write};
            let id=value["linkId"].as_str().and_then(|s|Uuid::parse_str(s).ok()).ok_or("host_local_invalid_response")?;
            let hash=value["requestHash"].as_str().ok_or("host_local_no_request_to_approve")?.to_owned();
            if !std::io::stdin().is_terminal(){
                eprintln!("Review this request, then approve with: vesta link --approve {id} {hash}. Use the same --state value.");
                return Ok(value);
            }
            println!("{}",serde_json::to_string_pretty(&value).map_err(|_|"host_local_invalid_response")?);
            eprint!("Approve this exact device request? [y/N] ");std::io::stderr().flush().map_err(|_|"host_local_io_failed")?;
            let mut answer=String::new();std::io::stdin().read_line(&mut answer).map_err(|_|"host_local_io_failed")?;
            if !matches!(answer.trim(),"y"|"Y"|"yes"|"YES"){return Ok(serde_json::json!({"state":"not_approved"}));}
            return request(&state,LocalRequest::Approve{link_id:id,request_hash:hash}).await;
        }
        Ok(value)
    }
    .await;
    match result {
        Ok(value) => {
            println!("{value}");
            std::process::ExitCode::SUCCESS
        }
        Err(code) => {
            eprintln!("{code}\n{USAGE}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_local_cli_grammar_requires_exact_approval_and_explicit_revoke_all() {
        let parse_args = |a: &[&str]| parse(a.iter().map(|s| s.to_string()).collect());
        let id = Uuid::new_v4().to_string();
        assert!(matches!(
            parse_args(&["setup-code"]).unwrap().1,
            LocalRequest::SetupCode
        ));
        assert!(matches!(
            parse_args(&["link"]).unwrap().1,
            LocalRequest::Link
        ));
        assert!(matches!(
            parse_args(&["link", "--approve"]).unwrap().1,
            LocalRequest::InspectPending
        ));
        assert!(matches!(
            parse_args(&["link", "--approve", &id]).unwrap().1,
            LocalRequest::Inspect { .. }
        ));
        assert!(parse_args(&["link", "--approve", &id, "bad-hash"]).is_err());
        assert!(matches!(
            parse_args(&["link", "--approve", &id, &"a".repeat(64)])
                .unwrap()
                .1,
            LocalRequest::Approve { .. }
        ));
        for revoke in [false, true] {
            let mut a = vec!["link", "--recover", &id, "public-key", "Laptop"];
            if revoke {
                a.push("--revoke-all")
            }
            match parse_args(&a).unwrap().1 {
                LocalRequest::Recover { request } => assert_eq!(request.revoke_all, revoke),
                _ => panic!("expected recovery"),
            }
        }
        assert!(parse_args(&["setup-code", "--remote", "http://example.com"]).is_err());
    }
    #[cfg(any(windows, target_os = "linux"))]
    #[tokio::test]
    async fn host_local_protected_channel_rotates_setup_without_http() {
        let dir = tempfile::tempdir().unwrap();
        let state =
            vesta_control::machine::HostState::open(dir.path(), Arc::new(vesta_state::SystemClock))
                .unwrap();
        let identity = Arc::new(HostIdentity::new(state.clone()).unwrap());
        let server = Server::start(dir.path(), identity).await.unwrap();
        let first = request(dir.path(), LocalRequest::SetupCode).await.unwrap();
        let second = request(dir.path(), LocalRequest::SetupCode).await.unwrap();
        assert_ne!(first["code"], second["code"]);
        assert_eq!(first["code"].as_str().unwrap().len(), 14);
        assert!(request(dir.path(), LocalRequest::Link).await.is_err());
        state.claim(Uuid::new_v4(), "Owner", [5; 32]).unwrap();
        let link = request(dir.path(), LocalRequest::Link).await.unwrap();
        let pending = request(dir.path(), LocalRequest::InspectPending)
            .await
            .unwrap();
        assert_eq!(link["linkId"], pending["linkId"]);
        assert_eq!(pending["state"], "awaiting_request");
        server.shutdown().await.unwrap();
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn host_local_peer_admission_rejects_other_os_users() {
        assert!(allowed_peer(0, 1000));
        assert!(allowed_peer(1000, 1000));
        assert!(!allowed_peer(1001, 1000));
    }
}
