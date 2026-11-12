//! Small, least-privilege adapters for launching the Vesta host at user login.

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_detach;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub fn target() -> Result<&'static str, String> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Ok("windows-x86_64");
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Ok("linux-x86_64");
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        return Ok("linux-aarch64");
    }
    #[allow(unreachable_code)]
    Err("unsupported platform".into())
}

pub fn startup_available() -> bool {
    #[cfg(windows)]
    {
        return find_executable("powershell.exe").is_some();
    }
    #[cfg(target_os = "linux")]
    {
        return bounded_status(Command::new("systemctl").args(["--user", "show-environment"]))
            .is_ok();
    }
    #[allow(unreachable_code)]
    false
}

pub fn startup_description() -> &'static str {
    #[cfg(windows)]
    {
        return "Starts Vesta when the current Windows user logs in.";
    }
    #[cfg(target_os = "linux")]
    {
        return "Starts Vesta with the current user's systemd session.";
    }
    #[allow(unreachable_code)]
    "Automatic startup is unavailable on this platform."
}

pub fn enable_startup(root: &Path, executable: &Path, id: &str) -> Result<(), String> {
    let (root, executable) = validate_installation(root, executable)?;
    let name = startup_name(id)?;
    #[cfg(windows)]
    {
        windows::enable(&root, &executable, &name)
    }
    #[cfg(target_os = "linux")]
    {
        linux::enable(&root, &executable, &name)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (root, executable, name);
        Err("automatic startup is unsupported on this platform".into())
    }
}

/// Removes this installation's current-user startup registration, if present.
/// The installation paths must still exist when this is called.
pub fn disable_startup(root: &Path, executable: &Path, id: &str) -> Result<(), String> {
    let (root, executable) = validate_installation(root, executable)?;
    let name = startup_name(id)?;
    #[cfg(windows)]
    {
        windows::disable(&root, &executable, &name)
    }
    #[cfg(target_os = "linux")]
    {
        linux::disable(&root, &executable, &name)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (root, executable, name);
        Err("automatic startup is unsupported on this platform".into())
    }
}

/// Launches `executable host --root root` as a background, non-window process.
pub fn start_host(root: &Path, executable: &Path) -> Result<(), String> {
    let (root, executable) = validate_installation(root, executable)?;
    start_verified_host(&root, Command::new(&executable))
}
/// Native updater supplies a command bound to retained signed payload handles.
pub(crate) fn start_verified_host(root: &Path, mut command: Command) -> Result<(), String> {
    command
        .arg("host")
        .arg("--root")
        .arg(root)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        // No inherited handles, including redirected CLI capture pipes.
        windows_detach::spawn(&command, root)
    }
    #[cfg(not(windows))]
    {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        command
            .spawn()
            .map(|_| ())
            .map_err(|_| "could not start host".into())
    }
}

fn startup_name(id: &str) -> Result<String, String> {
    if id.len() < 12 || !id.is_ascii() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid installation id".into());
    }
    Ok(format!("VestaHost-{}", &id[..12]))
}

fn bounded_output(command: &mut Command, capture: bool) -> Result<Output, String> {
    const LIMIT: Duration = Duration::from_secs(20);
    let mut child = command
        .stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(if capture {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdin(Stdio::null())
        .spawn()
        .map_err(|_| "could not run platform command".to_string())?;
    let deadline = Instant::now() + LIMIT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|_| "could not read platform command result".into());
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("platform command timed out".into());
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("could not wait for platform command".into());
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn bounded_status(command: &mut Command) -> Result<(), String> {
    let output = bounded_output(command, false)?;
    if output.status.success() {
        Ok(())
    } else {
        Err("platform command failed".into())
    }
}

fn validate_installation(root: &Path, executable: &Path) -> Result<(PathBuf, PathBuf), String> {
    if !root.is_absolute()
        || !executable.is_absolute()
        || has_controls(root)
        || has_controls(executable)
    {
        return Err("installation paths must be absolute and contain no control characters".into());
    }
    let root_meta =
        std::fs::symlink_metadata(root).map_err(|_| "installation root is unavailable")?;
    let exe_meta =
        std::fs::symlink_metadata(executable).map_err(|_| "setup executable is unavailable")?;
    if root_meta.file_type().is_symlink()
        || exe_meta.file_type().is_symlink()
        || !root_meta.is_dir()
        || !exe_meta.is_file()
    {
        return Err("installation paths are invalid".into());
    }
    let root = root
        .canonicalize()
        .map_err(|_| "installation root is unavailable")?;
    let executable = executable
        .canonicalize()
        .map_err(|_| "setup executable is unavailable")?;
    #[cfg(windows)]
    let expected = root.join("bin").join("vesta-setup.exe");
    #[cfg(not(windows))]
    let expected = root.join("bin").join("vesta-setup");
    if !same_path(&executable, &expected) {
        return Err("setup executable must be installed at root/bin/vesta-setup".into());
    }
    Ok((root, executable))
}

fn has_controls(path: &Path) -> bool {
    path.as_os_str()
        .to_string_lossy()
        .chars()
        .any(char::is_control)
}

#[cfg(windows)]
fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}
#[cfg(not(windows))]
fn same_path(a: &Path, b: &Path) -> bool {
    a == b
}

#[cfg(windows)]
fn find_executable(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(';')
        .map(|d| PathBuf::from(d).join(name))
        .find(|p| p.is_file())
}

#[cfg(windows)]
mod windows {
    use super::*;
    use base64::Engine;

    fn ps_quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', "''"))
    }

    // Windows command-line parsing convention used by CommandLineToArgvW/CRT.
    fn windows_arg(s: &str) -> String {
        let mut out = String::from("\"");
        let mut slashes = 0;
        for ch in s.chars() {
            if ch == '\\' {
                slashes += 1;
                continue;
            }
            if ch == '"' {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push(ch);
            } else {
                out.push_str(&"\\".repeat(slashes));
                out.push(ch);
            }
            slashes = 0;
        }
        out.push_str(&"\\".repeat(slashes * 2));
        out.push('"');
        out
    }

    pub fn enable(root: &Path, executable: &Path, name: &str) -> Result<(), String> {
        let shell = super::find_executable("powershell.exe").ok_or("PowerShell is unavailable")?;
        let root = root.to_string_lossy();
        let executable = executable.to_string_lossy();
        let task = ps_quote(name);
        let user_args = format!("host --root {}", windows_arg(&root));
        let script = format!(
            r#"
$ErrorActionPreference = 'Stop'
try {{
$taskName = {task}
$user = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$expectedArguments = {args}
$existing = @(Get-ScheduledTask -ErrorAction SilentlyContinue | Where-Object {{ $_.TaskName -eq $taskName }})
if ($existing.Count -gt 0) {{
    if ($existing.Count -ne 1) {{ exit 32 }}
    $t = $existing[0]
    $p = $t.Principal
    $actionMatches = ($t.Actions.Count -eq 1 -and $t.Actions[0].Execute -ceq {exe} -and $t.Actions[0].Arguments -ceq $expectedArguments)
    $principalMatches = (($p.UserId -ceq $sid -or $p.UserId -ceq $user) -and $p.LogonType -eq 'Interactive' -and $p.RunLevel -eq 'Limited')
    $triggerMatches = ($t.TaskPath -ceq '\' -and $t.Triggers.Count -eq 1 -and $t.Triggers[0].CimClass.CimClassName -eq 'MSFT_TaskLogonTrigger' -and $t.Triggers[0].UserId -ceq $user)
    if ($actionMatches -and $principalMatches -and $triggerMatches -and $t.Description -ceq 'Vesta host for current user') {{
        Start-ScheduledTask -TaskName $taskName
        exit 0
    }}
    exit 32
}}
$action = New-ScheduledTaskAction -Execute {exe} -Argument {args} -WorkingDirectory {root}
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -StartWhenAvailable
Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description 'Vesta host for current user' | Out-Null
Start-ScheduledTask -TaskName $taskName
}} catch {{
    exit 1
}}
exit 0
"#,
            task = task,
            exe = ps_quote(&executable),
            args = ps_quote(&user_args),
            root = ps_quote(&root)
        );
        let wide: Vec<u16> = script.encode_utf16().collect();
        let mut bytes = Vec::with_capacity(wide.len() * 2);
        for unit in wide {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        use std::os::windows::process::CommandExt;
        let result = bounded_output(
            Command::new(shell)
                .arg("-NoProfile")
                .arg("-NonInteractive")
                .arg("-EncodedCommand")
                .arg(&encoded)
                .creation_flags(0x08000000),
            true,
        )
        .map_err(|_| "could not configure Windows startup")?;
        if result.status.success() {
            Ok(())
        } else if result.status.code() == Some(32) {
            Err("startup task name is already in use".into())
        } else {
            Err("could not configure Windows startup".into())
        }
    }

    pub fn disable(root: &Path, executable: &Path, name: &str) -> Result<(), String> {
        let shell = super::find_executable("powershell.exe").ok_or("PowerShell is unavailable")?;
        let root = root.to_string_lossy();
        let executable = executable.to_string_lossy();
        let task = ps_quote(name);
        let user_args = format!("host --root {}", windows_arg(&root));
        let script = format!(
            r#"
$ErrorActionPreference = 'Stop'
try {{
$taskName = {task}
$user = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$expectedArguments = {args}
$existing = @(Get-ScheduledTask -ErrorAction SilentlyContinue | Where-Object {{ $_.TaskName -eq $taskName }})
if ($existing.Count -eq 0) {{ exit 0 }}
if ($existing.Count -ne 1) {{ exit 32 }}
$t = $existing[0]
$p = $t.Principal
$actionMatches = ($t.Actions.Count -eq 1 -and $t.Actions[0].Execute -ceq {exe} -and $t.Actions[0].Arguments -ceq $expectedArguments)
$principalMatches = (($p.UserId -ceq $sid -or $p.UserId -ceq $user) -and $p.LogonType -eq 'Interactive' -and $p.RunLevel -eq 'Limited')
$triggerMatches = ($t.TaskPath -ceq '\' -and $t.Triggers.Count -eq 1 -and $t.Triggers[0].CimClass.CimClassName -eq 'MSFT_TaskLogonTrigger' -and $t.Triggers[0].UserId -ceq $user)
if (-not ($actionMatches -and $principalMatches -and $triggerMatches -and $t.Description -ceq 'Vesta host for current user')) {{ exit 32 }}
Unregister-ScheduledTask -TaskName $taskName -TaskPath '\' -Confirm:$false
}} catch {{
    exit 1
}}
exit 0
"#,
            task = task,
            exe = ps_quote(&executable),
            args = ps_quote(&user_args)
        );
        let wide: Vec<u16> = script.encode_utf16().collect();
        let mut bytes = Vec::with_capacity(wide.len() * 2);
        for unit in wide {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        use std::os::windows::process::CommandExt;
        let result = bounded_output(
            Command::new(shell)
                .arg("-NoProfile")
                .arg("-NonInteractive")
                .arg("-EncodedCommand")
                .arg(&encoded)
                .creation_flags(0x08000000),
            false,
        )
        .map_err(|_| "could not remove Windows startup")?;
        if result.status.success() {
            Ok(())
        } else if result.status.code() == Some(32) {
            Err("startup task is not owned by this installation".into())
        } else {
            Err("could not remove Windows startup".into())
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) mod linux {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Write};
    use std::os::unix::fs::OpenOptionsExt;

    fn systemd_quote(s: &str) -> String {
        let mut out = String::from("\"");
        for ch in s.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '%' => out.push_str("%%"),
                '$' => out.push_str("$$"),
                _ => out.push(ch),
            }
        }
        out.push('"');
        out
    }

    pub(super) fn unit_contents(root: &Path, executable: &Path) -> String {
        let root_s = root.to_string_lossy();
        let exe_s = executable.to_string_lossy();
        format!(
            // The host resolves every path from its absolute --root. WorkingDirectory
            // is a path setting, not an ExecStart argument: shell quotes become literal
            // path bytes here. A fixed directory avoids path-specific parsing entirely.
            "[Unit]\nDescription=Vesta host for current user\n\n[Service]\nType=simple\nWorkingDirectory=/\nExecStart={} host --root {}\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
            systemd_quote(&exe_s),
            systemd_quote(&root_s)
        )
    }

    fn legacy_unit_contents(root: &Path, executable: &Path) -> String {
        unit_contents(root, executable).replacen(
            "WorkingDirectory=/\n",
            &format!(
                "WorkingDirectory={}\n",
                systemd_quote(&root.to_string_lossy())
            ),
            1,
        )
    }

    pub(super) fn unit_is_owned(existing: &[u8], expected: &str) -> bool {
        existing == expected.as_bytes()
    }

    fn unit_dir() -> Result<PathBuf, String> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .map(|h| h.join(".config"))
            })
            .ok_or("user configuration directory is unavailable")?;
        if has_controls(&config) {
            return Err("user configuration directory is invalid".into());
        }
        Ok(config.join("systemd").join("user"))
    }

    pub fn enable(root: &Path, executable: &Path, name: &str) -> Result<(), String> {
        enable_inner(root, executable, name, true)
    }

    fn enable_inner(
        root: &Path,
        executable: &Path,
        name: &str,
        allow_legacy_repair: bool,
    ) -> Result<(), String> {
        if bounded_status(Command::new("systemctl").args(["--user", "show-environment"])).is_err() {
            return Err("user systemd session is unavailable".into());
        }
        let dir = unit_dir()?;
        ensure_safe_dir(&dir)?;
        let path = dir.join(format!("{name}.service"));
        let contents = unit_contents(root, executable);
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > 16 * 1024 {
                    return Err("startup unit name is already in use".into());
                }
                let existing_file = OpenOptions::new()
                    .read(true)
                    .custom_flags(0x20000)
                    .open(&path)
                    .map_err(|_| "startup unit name is already in use")?; // Linux O_NOFOLLOW
                let opened_meta = existing_file
                    .metadata()
                    .map_err(|_| "startup unit name is already in use")?;
                if !opened_meta.is_file() || opened_meta.len() > 16 * 1024 {
                    return Err("startup unit name is already in use".into());
                }
                let mut existing = Vec::with_capacity(opened_meta.len() as usize);
                existing_file
                    .take(16 * 1024 + 1)
                    .read_to_end(&mut existing)
                    .map_err(|_| "startup unit name is already in use")?;
                if !unit_is_owned(&existing, &contents) {
                    // These exact legacy units cannot start because their quoted
                    // WorkingDirectory is invalid. Repair only an inactive, owned
                    // registration, without stopping any running host or game.
                    let legacy = legacy_unit_contents(root, executable);
                    if allow_legacy_repair && unit_is_owned(&existing, &legacy) {
                        if systemctl(&["is-active", &format!("{name}.service")]).is_ok() {
                            return Err("legacy startup unit is active; stop it deliberately before repairing startup".into());
                        }
                        if !read_owned_unit(&path, &legacy)? {
                            return Err("startup unit changed while repairing".into());
                        }
                        fs::remove_file(&path)
                            .map_err(|_| "could not repair owned startup unit")?;
                        systemctl(&["daemon-reload"])
                            .map_err(|_| "could not reload user startup units")?;
                        return enable_inner(root, executable, name, false);
                    }
                    return Err("startup unit name is already in use".into());
                }
                systemctl(&["enable", "--now", &format!("{name}.service")])
                    .map_err(|_| "could not enable user startup".to_string())?;
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("could not inspect startup unit".into()),
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    "startup unit name is already in use".to_owned()
                } else {
                    "could not create startup unit".to_owned()
                }
            })?;
        let write_result = file
            .write_all(contents.as_bytes())
            .and_then(|_| file.sync_all());
        drop(file);
        if write_result.is_err() {
            let _ = fs::remove_file(&path);
            return Err("could not write startup unit".into());
        }
        let reload = systemctl(&["daemon-reload"]);
        let enable = if reload.is_ok() {
            systemctl(&["enable", "--now", &format!("{name}.service")])
        } else {
            Err(())
        };
        if enable.is_err() {
            // The file was exclusively created by this call, so only its own unit is removed.
            let _ = systemctl(&["disable", "--now", &format!("{name}.service")]);
            let removed = fs::remove_file(&path).is_ok();
            if removed {
                let _ = systemctl(&["daemon-reload"]);
            }
            return Err("could not enable user startup".into());
        }
        Ok(())
    }

    pub fn disable(root: &Path, executable: &Path, name: &str) -> Result<(), String> {
        let dir = unit_dir()?;
        let path = match inspect_existing_unit_dir(&dir)? {
            Some(()) => dir.join(format!("{name}.service")),
            None => return Ok(()),
        };
        let current = unit_contents(root, executable);
        let legacy = legacy_unit_contents(root, executable);
        let expected = if read_owned_unit(&path, &legacy).is_ok_and(|owned| owned) {
            legacy
        } else {
            current
        };
        if !read_owned_unit(&path, &expected)? {
            return Ok(());
        }
        if bounded_status(Command::new("systemctl").args(["--user", "show-environment"])).is_err() {
            return Err("user systemd session is unavailable".into());
        }

        let unit = format!("{name}.service");
        systemctl(&["stop", &unit]).map_err(|_| "could not stop user startup".to_string())?;
        systemctl(&["disable", &unit]).map_err(|_| "could not disable user startup".to_string())?;

        // Recheck immediately before unlinking so a replaced or edited entry is preserved.
        if !read_owned_unit(&path, &expected)? {
            return Err("startup unit changed while disabling".into());
        }
        fs::remove_file(&path).map_err(|_| "could not remove user startup unit".to_string())?;
        systemctl(&["daemon-reload"])
            .map_err(|_| "could not reload user startup units".to_string())?;
        Ok(())
    }

    pub(crate) fn inspect_existing_unit_dir(path: &Path) -> Result<Option<()>, String> {
        let mut current = PathBuf::from("/");
        for component in path.components() {
            match component {
                std::path::Component::RootDir => continue,
                std::path::Component::Normal(part) => current.push(part),
                _ => return Err("user startup directory is invalid".into()),
            }
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                    return Err("user startup directory contains a link".into());
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => return Err("could not inspect user startup directory".into()),
            }
        }
        Ok(Some(()))
    }

    pub(crate) fn read_owned_unit(path: &Path, expected: &str) -> Result<bool, String> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err("could not inspect startup unit".into()),
        };
        if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > 16 * 1024 {
            return Err("startup unit name is already in use".into());
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(0x20000)
            .open(path)
            .map_err(|_| "startup unit name is already in use")?;
        let opened_meta = file
            .metadata()
            .map_err(|_| "startup unit name is already in use")?;
        if !opened_meta.is_file() || opened_meta.len() > 16 * 1024 {
            return Err("startup unit name is already in use".into());
        }
        let mut contents = Vec::with_capacity(opened_meta.len() as usize);
        file.take(16 * 1024 + 1)
            .read_to_end(&mut contents)
            .map_err(|_| "could not read startup unit")?;
        if contents.len() > 16 * 1024 {
            return Err("startup unit name is already in use".into());
        }
        if !unit_is_owned(&contents, expected) {
            return Err("startup unit is not owned by this installation".into());
        }
        Ok(true)
    }

    fn systemctl(args: &[&str]) -> Result<(), ()> {
        bounded_status(Command::new("systemctl").arg("--user").args(args)).map_err(|_| ())
    }

    pub(crate) fn ensure_safe_dir(path: &Path) -> Result<(), String> {
        let mut current = PathBuf::from("/");
        for component in path.components() {
            match component {
                std::path::Component::RootDir => continue,
                std::path::Component::Normal(part) => current.push(part),
                _ => return Err("user startup directory is invalid".into()),
            }
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                    return Err("user startup directory contains a link".into());
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&current)
                    .map_err(|_| "could not create user startup directory")?,
                Err(_) => return Err("could not inspect user startup directory".into()),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_name_uses_validated_installation_id_prefix() {
        assert_eq!(
            startup_name("0123456789abcdef").unwrap(),
            "VestaHost-0123456789ab"
        );
        assert!(startup_name("0123456789a").is_err());
        assert!(startup_name("0123456789ab/ef").is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires systemd-analyze; validates unit syntax without registering or starting services"]
    fn linux_generated_startup_unit_parser() {
        let temp = tempfile::tempdir().unwrap();
        let root = Path::new("/tmp/Vesta test % $ root");
        let executable = Path::new("/usr/bin/true");
        let unit = temp.path().join("vesta-parser.service");
        let fixed = linux::unit_contents(root, executable);
        std::fs::write(&unit, &fixed).unwrap();
        let output = bounded_output(
            Command::new("systemd-analyze")
                .args(["--user", "verify"])
                .arg(&unit),
            true,
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let legacy = fixed.replacen(
            "WorkingDirectory=/\n",
            "WorkingDirectory=\"/tmp/Vesta test % $ root\"\n",
            1,
        );
        std::fs::write(&unit, legacy).unwrap();
        let output = bounded_output(
            Command::new("systemd-analyze")
                .args(["--user", "verify"])
                .arg(&unit),
            true,
        )
        .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("WorkingDirectory"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_unit_ownership_requires_exact_content() {
        let root = Path::new("/opt/Vesta root");
        let executable = root.join("bin/vesta-setup");
        let expected = linux::unit_contents(root, &executable);
        assert!(expected.contains("WorkingDirectory=/\n"));
        assert!(expected.contains("--root \"/opt/Vesta root\""));
        assert!(linux::unit_is_owned(expected.as_bytes(), &expected));

        let other_root = Path::new("/opt/Other root");
        let foreign = linux::unit_contents(other_root, &other_root.join("bin/vesta-setup"));
        assert!(!linux::unit_is_owned(foreign.as_bytes(), &expected));
        assert!(!linux::unit_is_owned(b"", &expected));
        assert!(!linux::unit_is_owned(
            format!("{}# foreign edit", expected).as_bytes(),
            &expected
        ));
    }
}
