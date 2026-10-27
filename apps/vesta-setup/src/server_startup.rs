//! Per-server agent boot registration. Independent of Minecraft autostart.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub server_id: String,
    pub server_dir: PathBuf,
    pub executable: PathBuf,
    pub unit_directory: PathBuf,
    pub principal: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Enabled,
    Disabled,
}

fn name(id: &str) -> Result<String, String> {
    if id.len() != 36
        || !id.starts_with("srv_")
        || !id[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(
            "invalid managed server ID (expected srv_ followed by 32 lowercase hex digits)".into(),
        );
    }
    Ok(format!("vesta-agent-{id}.service"))
}
/// Resolve an existing installation. Persist this descriptor before enabling it;
/// uninstall must reuse it instead of substituting the uninstalling CLI path.
pub fn prepare(id: &str, directory: &Path, executable: &Path) -> Result<Registration, String> {
    name(id)?;
    if !directory.is_absolute() || !executable.is_absolute() {
        return Err("agent boot paths must be absolute".into());
    }
    let directory = directory
        .canonicalize()
        .map_err(|e| format!("server directory: {e}"))?;
    directory_identity(id, &directory)?;
    let executable = executable
        .canonicalize()
        .map_err(|e| format!("agent executable: {e}"))?;
    if !directory.join("host.toml").is_file() || !executable.is_file() {
        return Err("existing host.toml and agent executable required".into());
    }
    let (unit_directory, principal) = identity()?;
    Ok(Registration {
        server_id: id.into(),
        server_dir: directory,
        executable,
        unit_directory,
        principal,
    })
}
#[cfg(not(target_os = "linux"))]
fn unsupported() -> String {
    "Per-server machine-boot startup is unsupported on this platform. Windows boot-trigger registration requires an elevated administrator and a verified current-user S4U task; no login-start fallback is used.".into()
}
#[cfg(not(target_os = "linux"))]
fn identity() -> Result<(PathBuf, String), String> {
    Err(unsupported())
}
#[cfg(target_os = "linux")]
fn identity() -> Result<(PathBuf, String), String> {
    use std::os::unix::fs::MetadataExt;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME unavailable")?;
    if !home.is_absolute() {
        return Err("HOME must be absolute".into());
    }
    let uid = std::fs::metadata("/proc/self")
        .map_err(|e| e.to_string())?
        .uid()
        .to_string();
    let config = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home.join(".config"),
    };
    if !config.is_absolute() {
        return Err("XDG_CONFIG_HOME must be absolute".into());
    }
    Ok((config.join("systemd/user"), uid))
}
fn directory_identity(id: &str, directory: &Path) -> Result<(), String> {
    name(id)?;
    if directory.file_name().and_then(|s| s.to_str()) != Some(id) {
        return Err("server directory basename does not match managed server ID".into());
    }
    Ok(())
}
#[cfg(target_os = "linux")]
fn validate(r: &Registration) -> Result<String, String> {
    let name = name(&r.server_id)?;
    directory_identity(&r.server_id, &r.server_dir)?;
    if !r.server_dir.is_absolute() || !r.executable.is_absolute() {
        return Err("receipt paths must be absolute".into());
    }
    if identity()? != (r.unit_directory.clone(), r.principal.clone()) {
        return Err("boot receipt belongs to another account or unit directory".into());
    }
    Ok(name)
}
/// Read-only capability probe. Permission to enable linger is checked by enable.
pub fn available() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        linux::available()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(unsupported())
    }
}
/// Start an exact owned enabled agent after publishing the server registry.
pub fn start(r: &Registration) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        linux::start(r)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = r;
        Err(unsupported())
    }
}
/// Enables boot only; does not launch the agent now or start Minecraft.
pub fn enable(r: &Registration) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        linux::enable(r)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = r;
        Err(unsupported())
    }
}
/// Caller must establish game/agent quiescence first. Never changes account linger.
pub fn disable(r: &Registration) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        linux::disable(r)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = r;
        Err(unsupported())
    }
}
pub fn status(r: &Registration) -> Result<Status, String> {
    #[cfg(target_os = "linux")]
    {
        linux::status(r)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = r;
        Err(unsupported())
    }
}

#[cfg(any(target_os = "linux", test))]
fn quote(path: &Path) -> Result<String, String> {
    let s = path.to_str().ok_or("boot paths must be UTF-8")?;
    if s.chars().any(char::is_control) {
        return Err("control characters in boot path".into());
    }
    Ok(format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    ))
}
#[cfg(test)]
fn verify_contents(r: &Registration, contents: &[u8]) -> Result<(), String> {
    if contents != unit(r)?.as_bytes() {
        return Err("boot unit ownership mismatch; foreign edits preserved".into());
    }
    Ok(())
}
#[cfg(any(target_os = "linux", test))]
fn unit(r: &Registration) -> Result<String, String> {
    name(&r.server_id)?;
    Ok(format!(
        "# Vesta managed agent {} principal={}\n[Unit]\nDescription=Vesta managed server agent\nStartLimitIntervalSec=60\nStartLimitBurst=3\n\n[Service]\nType=simple\nWorkingDirectory=/\nExecStart={} agent serve {} --allow-local\nRestart=on-failure\nRestartSec=5\nKillSignal=SIGINT\nKillMode=process\nSendSIGKILL=no\nTimeoutStopSec=30\n\n[Install]\nWantedBy=default.target\n",
        r.server_id,
        r.principal,
        quote(&r.executable)?,
        quote(&r.server_dir.join("host.toml"))?
    ))
}
#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{
        fs,
        io::{Read, Write},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };
    fn run(program: &str, args: &[&str]) -> Result<String, String> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{program}: {e}"))?;
        let stdout = child.stdout.take().ok_or("command output unavailable")?;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.take(8193).read_to_end(&mut bytes);
            let _ = sender.send(bytes);
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match child.try_wait() {
                Ok(Some(result)) => {
                    let bytes = receiver
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .map_err(|_| "command output exceeded deadline")?;
                    if bytes.len() > 8192 {
                        return Err(format!("{program}: output limit exceeded"));
                    }
                    if !result.success() {
                        return Err(format!(
                            "{program} failed ({result}); check current-user systemd session and permission to enable linger"
                        ));
                    }
                    return String::from_utf8(bytes).map_err(|_| "non-UTF-8 command output".into());
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{program}: command failed or exceeded 20 seconds"));
                }
            }
        }
    }
    pub fn available() -> Result<(), String> {
        let (directory, uid) = identity()?;
        crate::platform::linux::inspect_existing_unit_dir(&directory)?;
        run("systemctl", &["--user", "show-environment"])?;
        run(
            "loginctl",
            &["show-user", &uid, "--property=Linger", "--value"],
        )?;
        Ok(())
    }
    pub fn start(r: &Registration) -> Result<(), String> {
        if status(r)? != Status::Enabled {
            return Err("agent boot registration is not enabled".into());
        }
        owned(r)?;
        let n = name(&r.server_id)?;
        let fragment = run(
            "systemctl",
            &["--user", "show", &n, "--property=FragmentPath", "--value"],
        )?;
        if Path::new(fragment.trim()) != r.unit_directory.join(&n) {
            return Err("loaded agent unit is not the owned registration".into());
        }
        let dropins = run(
            "systemctl",
            &["--user", "show", &n, "--property=DropInPaths", "--value"],
        )?;
        if !dropins.trim().is_empty() {
            return Err("loaded agent unit has operator drop-ins; start refused".into());
        }
        run("systemctl", &["--user", "start", &name(&r.server_id)?])?;
        Ok(())
    }
    fn owned(r: &Registration) -> Result<Option<PathBuf>, String> {
        let path = r.unit_directory.join(validate(r)?);
        if crate::platform::linux::inspect_existing_unit_dir(&r.unit_directory)?.is_none() {
            return Ok(None);
        }
        crate::platform::linux::inspect_existing_unit_dir(
            &r.unit_directory.join("default.target.wants"),
        )?;
        for extra in [
            r.unit_directory.join(format!("{}.d", name(&r.server_id)?)),
            r.unit_directory.join("service.d"),
        ] {
            if extra.exists() {
                return Err(
                    "systemd drop-ins present; boot registration requires manual review".into(),
                );
            }
        }
        let link = r
            .unit_directory
            .join("default.target.wants")
            .join(name(&r.server_id)?);
        if fs::symlink_metadata(&link).is_ok()
            && fs::read_link(&link).map_err(|_| "foreign enablement link preserved")? != path
        {
            return Err("foreign enablement link preserved".into());
        }
        if crate::platform::linux::read_owned_unit(&path, &unit(r)?)? {
            Ok(Some(path))
        } else {
            Ok(None)
        }
    }
    pub fn status(r: &Registration) -> Result<Status, String> {
        if owned(r)?.is_none() {
            return Ok(Status::Disabled);
        }
        let n = name(&r.server_id)?;
        let enabled = r
            .unit_directory
            .join("default.target.wants")
            .join(&n)
            .is_symlink();
        let linger = run(
            "loginctl",
            &["show-user", &r.principal, "--property=Linger", "--value"],
        )?;
        if enabled && linger.trim() == "yes" {
            Ok(Status::Enabled)
        } else {
            Ok(Status::Disabled)
        }
    }
    pub fn enable(r: &Registration) -> Result<(), String> {
        let existing = owned(r)?;
        if !r.executable.is_file() || !r.server_dir.join("host.toml").is_file() {
            return Err("agent installation missing".into());
        }
        run("systemctl", &["--user", "show-environment"])?;
        if existing.is_none() {
            crate::platform::linux::ensure_safe_dir(&r.unit_directory)?;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(r.unit_directory.join(name(&r.server_id)?))
                .map_err(|e| e.to_string())?;
            file.write_all(unit(r)?.as_bytes())
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
        }
        owned(r)?;
        // Establish the exact owned unit before changing the account-wide boot
        // prerequisite. Failed registration keeps its receipt for explicit retry.
        run("loginctl", &["enable-linger", &r.principal])?;
        if run(
            "loginctl",
            &["show-user", &r.principal, "--property=Linger", "--value"],
        )?
        .trim()
            != "yes"
        {
            return Err("linger was not enabled; boot startup unavailable".into());
        }
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", &name(&r.server_id)?])?;
        Ok(())
    }
    pub fn disable(r: &Registration) -> Result<(), String> {
        let Some(path) = owned(r)? else {
            return Ok(());
        };
        run("systemctl", &["--user", "disable", &name(&r.server_id)?])?;
        owned(r)?;
        fs::remove_file(path).map_err(|e| e.to_string())?;
        run("systemctl", &["--user", "daemon-reload"])?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn registration() -> Registration {
        Registration {
            server_id: "srv_0123456789abcdef0123456789abcdef".into(),
            server_dir: "/tmp/server space%$".into(),
            executable: "/tmp/agent\"\\%$".into(),
            unit_directory: "/tmp/units".into(),
            principal: "1000".into(),
        }
    }
    #[test]
    fn full_identity() {
        let r = registration();
        assert_eq!(
            name(&r.server_id).unwrap(),
            "vesta-agent-srv_0123456789abcdef0123456789abcdef.service"
        );
        for bad in ["srv_0123", "srv_0123456789abcdef0123456789abcdeF", "../bad"] {
            assert!(name(bad).is_err());
        }
    }
    #[test]
    fn directory_identity_is_bound() {
        let id = "srv_0123456789abcdef0123456789abcdef";
        assert!(directory_identity(id, &PathBuf::from("servers").join(id)).is_ok());
        assert!(directory_identity(id, Path::new("servers/another-server")).is_err());
    }
    #[test]
    fn generation_is_safe_and_agent_only() {
        let text = unit(&registration()).unwrap();
        assert!(text.contains(&format!(
            "agent serve {} --allow-local",
            quote(&registration().server_dir.join("host.toml")).unwrap()
        )));
        assert!(text.contains("WorkingDirectory=/\n"));
        assert!(text.contains("KillMode=process\nSendSIGKILL=no"));
        assert!(!text.contains("--local-only"));
        assert!(!text.contains("autostart"));
        assert!(quote(Path::new("/tmp/bad\npath")).is_err());
    }
    #[test]
    fn foreign_contents_are_refused() {
        let r = registration();
        let expected = unit(&r).unwrap();
        assert!(verify_contents(&r, expected.as_bytes()).is_ok());
        for edited in [
            expected.replace("--allow-local", "--local-only"),
            expected.replace("principal=1000", "principal=0"),
            expected.replace("KillMode=process", "KillMode=control-group"),
            expected.replace(" agent serve ", " host "),
        ] {
            assert!(verify_contents(&r, edited.as_bytes()).is_err());
        }
    }
    #[test]
    fn receipt_roundtrip() {
        let r = registration();
        assert_eq!(
            r,
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap()
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn foreign_receipt_is_rejected_without_commands() {
        assert!(validate(&registration()).is_err());
    }
}
