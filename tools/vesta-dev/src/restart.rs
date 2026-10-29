//! Local development restart only. Never kills processes or clears recovery evidence.
use super::executable;
use fs2::FileExt;
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use vesta_control::config::{HostConfiguration, Supervision};
use vesta_state::{StateDirectory, artifacts::ArtifactName};

type Result<T> = std::result::Result<T, String>;
const WAIT: Duration = Duration::from_secs(90);
#[cfg(test)]
mod live_tests;

fn guarded(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("restart requires absolute paths without parent traversal".into());
    }
    for ancestor in path.ancestors() {
        let meta = fs::symlink_metadata(ancestor).map_err(|_| "restart path is unavailable")?;
        if meta.file_type().is_symlink() {
            return Err("restart refuses linked paths".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err("restart refuses reparse points".into());
            }
        }
    }
    Ok(())
}

fn read_document(path: &Path) -> Result<Vec<u8>> {
    guarded(path)?;
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|f| f.take(65_537).read_to_end(&mut bytes))
        .map_err(|_| "restart configuration unavailable")?;
    if bytes.len() > 65_536 {
        return Err("restart configuration exceeds limit".into());
    }
    Ok(bytes)
}

struct Agent {
    binary: PathBuf,
    root: PathBuf,
    id: String,
    base: PathBuf,
}
impl Agent {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command.current_dir(&self.base).stdin(Stdio::null());
        command
    }
    fn manage(&self, op: &str) -> Result<Value> {
        let output = self
            .command()
            .args(["manage", &self.id, op, "--json", "--install-root"])
            .arg(&self.root)
            .output()
            .map_err(|_| "cannot run staged management command")?;
        if !output.status.success() {
            return Err(format!(
                "{op} refused or outcome uncertain; no automatic retry or force-kill. Check the server with vesta manage {} status",
                self.id
            ));
        }
        // Only status emits structured data; lifecycle commands acknowledge completion.
        if op == "status" {
            serde_json::from_slice(&output.stdout)
                .map_err(|_| "invalid local status response".into())
        } else {
            Ok(Value::Null)
        }
    }
}

fn selected_id(list: &Value, target: &str) -> Result<String> {
    let list = list
        .as_array()
        .filter(|rows| rows.len() <= 4096)
        .ok_or("invalid server inventory")?;
    let mut matches = list
        .iter()
        .filter(|row| row["id"] == target || row["name"] == target);
    let id = matches
        .next()
        .and_then(|row| row["id"].as_str())
        .ok_or("server not found in selected registry")?;
    if matches.next().is_some()
        || id.len() != 36
        || !id.starts_with("srv_")
        || !id[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("ambiguous or invalid server identity".into());
    }
    Ok(id.into())
}

fn was_running(status: &Value) -> Result<bool> {
    match (
        status["agent"].as_str(),
        status["state"].as_str(),
        &status["generation"],
    ) {
        (Some("online"), Some("stopped"), Value::Null) => Ok(false),
        (Some("online"), Some("running"), Value::String(generation)) if !generation.is_empty() => {
            Ok(true)
        }
        _ => Err(
            "restart requires a stable running or stopped game; wait for current work to finish"
                .into(),
        ),
    }
}

fn listener_mode(status: &Value, selected: Option<bool>) -> Result<bool> {
    match (status["localOnly"].as_bool(), selected) {
        (Some(actual), None) => Ok(actual),
        (Some(actual), Some(selected)) if actual == selected => Ok(actual),
        (Some(_), Some(_)) => Err("listener override differs from the running agent; restart preserves its mode".into()),
        (None, Some(selected)) => Ok(selected),
        (None, None) => Err("this older agent does not report its listener mode. Repeat with --configured-listener for normal TLS serving, or --local-only for local management only; nothing was stopped".into()),
    }
}

/// Wait for the managed agent lease, runtime owner and supervisor owner to release.
/// Hold all available locks together while checking recovery state; never unlink them.
fn quiescent(source: &Path, config: &HostConfiguration) -> Result<bool> {
    let directory = source.parent().ok_or("invalid server source")?;
    let lease = directory.with_extension("operator-lock");
    guarded(&lease)?;
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .open(lease)
        .map_err(|_| "agent ownership lease unavailable")?;
    match FileExt::try_lock_exclusive(&lease) {
        Ok(()) => {}
        Err(e)
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
        {
            return Ok(false);
        }
        Err(_) => return Err("agent ownership cannot be checked".into()),
    }
    match fs::symlink_metadata(directory.join("operator-session-pending")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        _ => {
            return Err(
                "shutdown was not proven; recovery marker preserved, restart refused".into(),
            );
        }
    }
    let state = StateDirectory::new(config.state(), Some(config.game()))
        .map_err(|_| "invalid state directory")?;
    let _runner = match state.acquire_runner_lock() {
        Ok(lock) => lock,
        Err(e) if e.code() == "state_runner_lock_held" => return Ok(false),
        Err(_) => return Err("runtime ownership cannot be checked".into()),
    };
    if config.supervision() == Supervision::Separate {
        let directory = state
            .supervisor()
            .map_err(|_| "supervisor state unavailable")?;
        let name = ArtifactName::new("supervisor.lock").map_err(|_| "invalid supervisor lock")?;
        let lock = directory
            .open(&name)
            .map_err(|_| "supervisor ownership unavailable")?;
        if let Some(lock) = lock.as_ref() {
            match FileExt::try_lock_exclusive(lock) {
                Ok(()) => {}
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
                {
                    return Ok(false);
                }
                Err(_) => return Err("supervisor ownership cannot be checked".into()),
            }
        }
        if vesta_supervisor::instance_busy(&state)
            .map_err(|_| "supervisor observation unavailable")?
        {
            return Err("supervisor or recorded game remains live; restart refused".into());
        }
    }
    Ok(true)
}

fn spawn(agent: &Agent, source: &Path, local_only: bool, logs: &Path) -> Result<Child> {
    let directory = StateDirectory::new(logs, None).map_err(|_| "invalid restart log directory")?;
    directory
        .prepare()
        .map_err(|_| "cannot prepare private restart logs")?;
    let log_path = logs.join("agent.log");
    let log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&log_path)
        .map_err(|_| "cannot create restart log")?;
    directory
        .tighten_file(&log_path)
        .map_err(|_| "cannot protect restart log")?;
    let mut command = agent.command();
    command
        .args(["agent", "serve"])
        .arg(source)
        .stdout(log.try_clone().map_err(|_| "cannot open restart log")?)
        .stderr(log);
    if local_only {
        command.arg("--local-only");
    }
    spawn_independent(&mut command)
}

fn spawn_independent(command: &mut Command) -> Result<Child> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED: u32 = 0x0000_0008 | 0x0000_0200;
        command.creation_flags(DETACHED | 0x0100_0000);
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(_) if vesta_supervisor::pipe_security::job_allows_independent_lifetime() => {
                command.creation_flags(DETACHED);
            }
            Err(error) => {
                return Err(format!(
                    "cannot launch independent agent ({error}); Windows job containment prevents a proven independent lifetime. Exit this dev session and run scripts/dev-shell.ps1 from a standalone PowerShell terminal, then retry vesta-rebuild. Nothing was stopped or force-killed"
                ));
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .spawn()
        .map_err(|_| "cannot launch rebuilt agent".into())
}

/// Reject a terminal that cannot keep agents alive before building or opening a shell.
#[cfg(windows)]
pub(super) fn check_shell_lifetime() -> Result<()> {
    let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    command.arg("--help").stdin(Stdio::null());
    check_launch(&mut command)
}

fn check_independent_launch(agent: &Agent) -> Result<()> {
    // Check the exact replacement binary and process containment before stopping
    // a working host. `version` opens no host, listener or game runtime.
    let mut command = agent.command();
    command.arg("version");
    check_launch(&mut command)
}

fn check_launch(command: &mut Command) -> Result<()> {
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = spawn_independent(command)?;
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| "cannot observe launch preflight")?
        {
            return if status.success() {
                Ok(())
            } else {
                Err("replacement launch preflight failed; nothing was stopped".into())
            };
        }
        if Instant::now() >= until {
            return Err("replacement launch preflight timed out; nothing was stopped".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn run(
    bin: &Path,
    session: &Path,
    root: &Path,
    target: &str,
    selected_mode: Option<bool>,
) -> Result<()> {
    let root = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|_| "working directory unavailable")?
            .join(root)
    };
    guarded(&root)?;
    let binary = bin.join(executable("vesta"));
    let output = Command::new(&binary)
        .args(["list", "--json", "--install-root"])
        .arg(&root)
        .stdin(Stdio::null())
        .output()
        .map_err(|_| "cannot list selected registry")?;
    if !output.status.success() {
        return Err("cannot inspect selected registry; nothing was stopped".into());
    }
    let id = selected_id(
        &serde_json::from_slice(&output.stdout).map_err(|_| "invalid registry response")?,
        target,
    )?;
    let source = root.join("instances").join(&id).join("host.toml");
    let document = read_document(&source)?;
    let mut agent = Agent {
        binary,
        root,
        id,
        base: source.parent().unwrap().to_owned(),
    };
    let observed = agent.manage("status");
    let offline = observed.is_err();
    let status = match observed {
        Ok(status) => Some(status),
        Err(error) => {
            // A missing endpoint alone proves nothing. Agent-only recovery
            // requires an explicit listener choice and every ownership fence.
            let config = HostConfiguration::load_file(&source, &agent.base)
                .map_err(|_| "invalid host configuration")?;
            config
                .validate_host_paths()
                .map_err(|_| "invalid server paths")?;
            if selected_mode.is_none() || !quiescent(&source, &config)? {
                return Err(error);
            }
            None
        }
    };
    let running = status
        .as_ref()
        .map(was_running)
        .transpose()?
        .unwrap_or(false);
    let local_only = listener_mode(status.as_ref().unwrap_or(&Value::Null), selected_mode)?;
    if let Some(base) = status.as_ref().and_then(|s| s["launchBase"].as_str()) {
        agent.base = base.into();
    }
    guarded(&agent.base)?;
    let config = HostConfiguration::load_file(&source, &agent.base)
        .map_err(|_| "invalid host configuration")?;
    config
        .validate_host_paths()
        .map_err(|_| "invalid server paths")?;
    let launch = read_document(config.launch_definition())?;
    // Validate the complete staged build and launch before interrupting anything.
    let checked = agent
        .command()
        .args(["agent", "check-config"])
        .arg(&source)
        .output()
        .map_err(|_| "cannot validate rebuilt agent")?;
    if !checked.status.success() {
        return Err("rebuilt agent refused configuration; nothing was stopped".into());
    }
    check_independent_launch(&agent)?;
    let logs = session
        .join("restarts")
        .join(uuid::Uuid::new_v4().to_string());
    println!(
        "Restarting {}{}; waiting for graceful shutdown. Log: {}",
        agent.id,
        if running {
            " and its running Minecraft process"
        } else {
            " (Minecraft stays stopped)"
        },
        logs.join("agent.log").display()
    );
    if !offline {
        let stopped = agent
            .command()
            .args(["agent", "shutdown"])
            .arg(&source)
            .output()
            .map_err(|_| "cannot request graceful shutdown")?;
        if !stopped.status.success() {
            if String::from_utf8_lossy(&stopped.stderr)
                .contains("local_control_unsupported_request")
            {
                return Err("this older agent does not support shutdown requests. Stop its terminal once with Ctrl+C, then repeat --restart with --configured-listener or --local-only. Nothing was force-killed".into());
            }
            return Err(
                "shutdown refused or uncertain; no restart or automatic retry attempted".into(),
            );
        }
    }
    let until = Instant::now() + WAIT;
    while !quiescent(&source, &config)? {
        if Instant::now() >= until {
            return Err("shutdown did not finish; old processes retained, no force-kill".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if read_document(&source)? != document || read_document(config.launch_definition())? != launch {
        return Err("configuration changed during restart; left stopped for review".into());
    }
    let mut child = spawn(&agent, &source, local_only, &logs)?;
    let until = Instant::now() + WAIT;
    let ready = loop {
        if child
            .try_wait()
            .map_err(|_| "cannot observe rebuilt agent")?
            .is_some()
        {
            return Err(format!(
                "rebuilt agent exited; preserved log: {}",
                logs.join("agent.log").display()
            ));
        }
        if let Ok(status) = agent.manage("status")
            && status["agentPid"].as_u64() == Some(u64::from(child.id()))
        {
            break status;
        }
        if Instant::now() >= until {
            return Err(format!(
                "rebuilt agent has not answered; process left running, inspect {}",
                logs.join("agent.log").display()
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    if was_running(&ready)? {
        return Err("game unexpectedly running before restoration; no start sent".into());
    }
    if running {
        agent.manage("start")?;
        let observed = agent.manage("status")?;
        if observed["agentPid"].as_u64() != Some(u64::from(child.id())) || !was_running(&observed)?
        {
            return Err(
                "start outcome not confirmed; inspect server status, no retry attempted".into(),
            );
        }
    }
    println!(
        "Rebuilt agent is online (PID {}). Minecraft {}. Log: {}",
        child.id(),
        if running {
            "started; game readiness may still be loading"
        } else {
            "remains stopped"
        },
        logs.join("agent.log").display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ownership_locks_and_recovery_evidence_fence_relaunch() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp
            .path()
            .join("instances/srv_0123456789abcdef0123456789abcdef");
        fs::create_dir_all(directory.join("game/config")).unwrap();
        let source = directory.join("host.toml");
        let document = "schema-version=1\n[paths]\nstate='state'\ngame='game'\nscan-roots=['game/config']\nlaunch-definition='launch.toml'\n[pack]\nname='Fixture'\nversion='1'\nrevision=1\nminimum-revision=1\nminecraft-version='1.21.1'\n[pack-access]\nmode='minecraft'\n[listener]\nmode='disabled'\n";
        fs::write(&source, document).unwrap();
        let config = HostConfiguration::load_file(&source, &directory).unwrap();
        let state = StateDirectory::new(config.state(), Some(config.game())).unwrap();
        state.prepare().unwrap();
        let lease = File::create(directory.with_extension("operator-lock")).unwrap();
        FileExt::try_lock_exclusive(&lease).unwrap();
        assert!(!quiescent(&source, &config).unwrap());
        drop(lease);
        let owner = state.acquire_runner_lock().unwrap();
        assert!(!quiescent(&source, &config).unwrap());
        drop(owner);
        assert!(quiescent(&source, &config).unwrap());
        let marker = directory.join("operator-session-pending");
        fs::write(&marker, b"unproven shutdown").unwrap();
        assert!(quiescent(&source, &config).is_err());
        assert_eq!(fs::read(marker).unwrap(), b"unproven shutdown");
    }
    #[test]
    fn only_stable_observations_decide_game_restoration() {
        assert!(
            !was_running(
                &serde_json::json!({"agent":"online","state":"stopped","generation":null})
            )
            .unwrap()
        );
        assert!(
            was_running(
                &serde_json::json!({"agent":"online","state":"running","generation":"generation"})
            )
            .unwrap()
        );
        for state in ["stopping", "draining", "unknown"] {
            assert!(
                was_running(
                    &serde_json::json!({"agent":"online","state":state,"generation":"generation"})
                )
                .is_err()
            );
        }
        assert!(
            was_running(&serde_json::json!({"agent":"online","state":"running","generation":null}))
                .is_err()
        );
    }
    #[test]
    fn preserve_listener_mode_and_require_legacy_choice() {
        for actual in [true, false] {
            let status = serde_json::json!({"localOnly":actual});
            assert_eq!(listener_mode(&status, None).unwrap(), actual);
            assert!(listener_mode(&status, Some(!actual)).is_err());
        }
        assert!(listener_mode(&Value::Null, None).is_err());
        assert!(listener_mode(&Value::Null, Some(true)).unwrap());
    }
    #[test]
    fn registry_selection_never_interprets_target_as_a_path() {
        let id = "srv_0123456789abcdef0123456789abcdef";
        let list = serde_json::json!([{"id":id,"name":"example"}]);
        assert_eq!(selected_id(&list, "example").unwrap(), id);
        assert_eq!(selected_id(&list, id).unwrap(), id);
        assert!(selected_id(&list, "../example").is_err());
        assert!(
            selected_id(
                &serde_json::json!([{"id":"../../escape","name":"example"}]),
                "example"
            )
            .is_err()
        );
    }
}
