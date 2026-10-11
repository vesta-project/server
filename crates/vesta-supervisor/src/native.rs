//! Platform adapters: protected endpoint naming, peer checks and process identity.
//!
//! Windows uses a current-account-only named pipe; Linux an owner-only Unix socket in
//! the owner-only supervisor directory with peer uid checks in both directions. Other
//! platforms refuse. Same-account processes remain trusted; this is not OS isolation.
use crate::Result;
use std::path::{Path, PathBuf};

#[cfg(windows)]
pub type Stream = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(target_os = "linux")]
pub type Stream = tokio::net::UnixStream;

/// The supervisor endpoint for one state directory and the current OS account.
#[cfg(windows)]
pub fn endpoint(state: &Path, _directory: &Path) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let account = crate::pipe_security::account()?;
    let mut digest = Sha256::new();
    digest.update(b"vesta-supervisor-v1\0");
    digest.update(account.as_bytes());
    digest.update(b"\0");
    digest.update(state.to_string_lossy().as_bytes());
    Ok(PathBuf::from(format!(
        r"\\.\pipe\vesta-supervisor-{:x}",
        digest.finalize()
    )))
}
#[cfg(target_os = "linux")]
pub fn endpoint(_state: &Path, directory: &Path) -> Result<PathBuf> {
    Ok(directory.join("supervisor.sock"))
}
#[cfg(not(any(windows, target_os = "linux")))]
pub fn endpoint(_state: &Path, _directory: &Path) -> Result<PathBuf> {
    Err("supervisor_platform_unsupported")
}

/// One native observation of a PID. `Unknown` is a failed query, never "exited".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessState {
    Running(u64),
    Gone,
    Unknown,
}

/// Liveness of one exact recorded process. Callers must treat `Unknown` as alive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Liveness {
    Alive,
    Gone,
    Unknown,
}

#[cfg(windows)]
pub fn process_state(pid: u32) -> ProcessState {
    crate::pipe_security::process_state(pid)
}
#[cfg(target_os = "linux")]
pub fn process_state(pid: u32) -> ProcessState {
    // Field 22 (starttime) after the parenthesised command name; zombies have exited.
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return ProcessState::Gone,
        Err(_) => return ProcessState::Unknown,
    };
    let parsed = stat.rfind(')').and_then(|end| {
        let mut fields = stat[end + 1..].split_whitespace();
        let state = fields.next()?;
        let start = fields.nth(18)?.parse::<u64>().ok()?;
        Some((state == "Z", start))
    });
    match parsed {
        Some((true, _)) => ProcessState::Gone,
        Some((false, start)) => ProcessState::Running(start),
        None => ProcessState::Unknown,
    }
}
#[cfg(not(any(windows, target_os = "linux")))]
pub fn process_state(_pid: u32) -> ProcessState {
    ProcessState::Unknown
}

/// Native start time of a running process, for writing a new record.
pub fn process_start(pid: u32) -> Option<u64> {
    match process_state(pid) {
        ProcessState::Running(start) => Some(start),
        _ => None,
    }
}

/// Linux start times count from boot, so records also carry the kernel boot ID.
/// Windows creation times are absolute; no boot identity is needed there.
#[cfg(target_os = "linux")]
pub fn boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty() && id.len() <= 64)
}
#[cfg(not(target_os = "linux"))]
pub fn boot_id() -> Option<String> {
    None
}

/// Whether the exact process (PID, native start time, boot) is still running.
pub fn liveness(pid: u32, start: u64, boot: Option<&str>) -> Liveness {
    match boot_id() {
        Some(current) if boot.is_some_and(|boot| boot != current) => return Liveness::Gone,
        None if boot.is_some() => return Liveness::Unknown,
        _ => {}
    }
    match process_state(pid) {
        ProcessState::Running(observed) if observed == start => Liveness::Alive,
        ProcessState::Running(_) | ProcessState::Gone => Liveness::Gone,
        ProcessState::Unknown => Liveness::Unknown,
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub fn linux_uid() -> u32 {
    // SAFETY: geteuid has no pointers and observes only the current process identity.
    unsafe { libc::geteuid() }
}

/// Connect to a running supervisor and verify it runs as the current account.
#[cfg(windows)]
pub async fn connect(endpoint: &Path) -> Result<Stream> {
    let client = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(endpoint)
        .map_err(|error| {
            const ERROR_PIPE_BUSY: i32 = 231;
            if error.raw_os_error() == Some(ERROR_PIPE_BUSY) {
                "supervisor_busy"
            } else {
                "supervisor_not_running"
            }
        })?;
    crate::pipe_security::verify_server(&client)?;
    Ok(client)
}
#[cfg(target_os = "linux")]
pub async fn connect(endpoint: &Path) -> Result<Stream> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    let parent = endpoint.parent().ok_or("supervisor_acl_refused")?;
    let directory = std::fs::symlink_metadata(parent).map_err(|_| "supervisor_not_running")?;
    let socket = std::fs::symlink_metadata(endpoint).map_err(|_| "supervisor_not_running")?;
    if !socket.file_type().is_socket()
        || socket.uid() != linux_uid()
        || socket.permissions().mode() & 0o077 != 0
        || !directory.is_dir()
        || directory.uid() != linux_uid()
        || directory.permissions().mode() & 0o077 != 0
    {
        return Err("supervisor_acl_refused");
    }
    let client = tokio::net::UnixStream::connect(endpoint)
        .await
        .map_err(|_| "supervisor_not_running")?;
    if client
        .peer_cred()
        .map_err(|_| "supervisor_acl_refused")?
        .uid()
        != linux_uid()
    {
        return Err("supervisor_acl_refused");
    }
    Ok(client)
}
#[cfg(not(any(windows, target_os = "linux")))]
pub async fn connect(_endpoint: &Path) -> Result<std::convert::Infallible> {
    Err("supervisor_platform_unsupported")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive_and_a_wrong_start_time_is_not() {
        let pid = std::process::id();
        let start = process_start(pid).expect("own process start time");
        assert_eq!(liveness(pid, start, boot_id().as_deref()), Liveness::Alive);
        assert_eq!(liveness(pid, start.wrapping_add(1), None), Liveness::Gone);
        if cfg!(target_os = "linux") {
            assert_eq!(liveness(pid, start, Some("another-boot")), Liveness::Gone);
        }
    }

    #[test]
    fn exited_child_is_not_alive() {
        let mut child = std::process::Command::new(env!("CARGO"))
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let start = process_start(pid);
        child.wait().unwrap();
        assert!(start.is_none_or(|start| liveness(pid, start, None) == Liveness::Gone));
        assert_ne!(process_state(pid), ProcessState::Unknown);
    }
}
