//! OS-local managed-agent startup policy. Never changes Minecraft startup or EULA.
use crate::{
    cli::AgentStartupChoice,
    local_files::{self, Result},
    servers,
};
use std::{
    fs,
    io::{self, BufRead, IsTerminal, Read, Write},
    path::Path,
};
use vesta_setup::server_startup::{self, Registration, Status};

const RECEIPT: &str = "agent-startup.json";

pub fn choose(selected: Option<AgentStartupChoice>) -> Result<AgentStartupChoice> {
    if let Some(choice) = selected {
        return Ok(choice);
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "Choose --agent-startup manual or --agent-startup boot. This controls the agent, not Minecraft.",
        );
    }
    println!("The agent stays available for management while Minecraft is stopped.");
    #[cfg(target_os = "linux")]
    println!("Boot startup keeps your Linux user services running without signing in.");
    print!("Start this server's agent at machine boot? [y/N] ");
    io::stdout()
        .flush()
        .map_err(|_| "local_output_unavailable")?;
    let mut line = String::new();
    let read = io::stdin()
        .lock()
        .take(65)
        .read_line(&mut line)
        .map_err(|_| "local_input_unavailable")?;
    if read == 0 {
        return Err("local_input_cancelled");
    }
    if line.len() >= 65 {
        return Err("local_input_too_long");
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(AgentStartupChoice::Boot),
        "" | "n" | "no" => Ok(AgentStartupChoice::Manual),
        _ => Err("Enter yes or no, or use --agent-startup manual|boot."),
    }
}

pub fn preflight(choice: AgentStartupChoice) -> Result<()> {
    if choice == AgentStartupChoice::Boot {
        server_startup::available().map_err(|error| {
            eprintln!("{error}");
            "local_agent_boot_startup_unavailable"
        })?;
    }
    Ok(())
}

fn load(directory: &Path) -> Result<Option<Registration>> {
    let path = directory.join(RECEIPT);
    local_files::guard(&path)?;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("local_agent_startup_receipt_unavailable"),
        Ok(_) => (),
    }
    let registration: Registration = serde_json::from_slice(&local_files::read(&path, 16_384)?)
        .map_err(|_| "local_agent_startup_receipt_invalid")?;
    if fs::canonicalize(directory).map_err(|_| "local_path_refused")? != registration.server_dir
        || directory.file_name().and_then(|id| id.to_str()) != Some(registration.server_id.as_str())
    {
        return Err("local_agent_startup_receipt_identity_changed");
    }
    Ok(Some(registration))
}

pub fn enable(directory: &Path) -> Result<()> {
    let registration = match load(directory)? {
        Some(registration) => registration,
        None => {
            let executable = std::env::current_exe()
                .map_err(|_| "local_executable_unavailable")?
                .with_file_name(format!("vesta-agent{}", std::env::consts::EXE_SUFFIX));
            local_files::guard(&executable)?;
            let id = directory
                .file_name()
                .and_then(|id| id.to_str())
                .ok_or("local_record_invalid")?;
            let registration =
                server_startup::prepare(id, directory, &executable).map_err(|error| {
                    eprintln!("{error}");
                    "local_agent_startup_prepare_failed"
                })?;
            let bytes = serde_json::to_vec(&registration)
                .map_err(|_| "local_agent_startup_receipt_invalid")?;
            // Preserve the original command binding even if OS registration is partial.
            local_files::write_new(&directory.join(RECEIPT), &bytes)?;
            registration
        }
    };
    server_startup::enable(&registration).map_err(|error| {
        eprintln!("{error}");
        "local_agent_startup_enable_failed_server_preserved"
    })
}

pub fn start_registered(directory: &Path) -> Result<()> {
    let registration = load(directory)?.ok_or("local_agent_startup_not_configured")?;
    server_startup::start(&registration).map_err(|error| {
        eprintln!("{error}");
        "local_agent_startup_start_failed_server_preserved"
    })
}

pub fn start(root: &Path, name: &str) -> Result<()> {
    let registry = local_files::lock(&root.join("registry.lock"))?;
    let server = servers::resolve(root, name)?;
    let registered = if let Some(registration) = load(&server.directory)? {
        server_startup::status(&registration).map_err(|error| {
            eprintln!("{error}");
            "local_agent_startup_status_failed"
        })? == Status::Enabled
    } else {
        false
    };
    if registered {
        drop(registry);
        start_registered(&server.directory)?;
        println!(
            "Agent start requested. Use status to check readiness. Minecraft startup is unchanged."
        );
        return Ok(());
    }
    let lease = local_files::lock(
        &servers::managed_lease(&server.directory).ok_or("local_record_invalid")?,
    )?;
    let executable = std::env::current_exe()
        .map_err(|_| "local_executable_unavailable")?
        .with_file_name(format!("vesta-agent{}", std::env::consts::EXE_SUFFIX));
    local_files::guard(&executable)?;
    if !executable.is_file() {
        return Err("local_agent_executable_unavailable");
    }
    let mut command = std::process::Command::new(executable);
    command
        .args(["agent", "serve"])
        .arg(&server.config)
        .arg("--allow-local")
        .current_dir(&server.directory)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000008 | 0x08000000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    drop(lease);
    drop(registry);
    // An independent management agent, not a game process owned by this terminal.
    let child = command.spawn().map_err(|_| "local_agent_launch_failed")?;
    // A long-lived management terminal must also reap a manually started agent
    // if it exits later. A small detached OS thread does not block CLI shutdown.
    #[cfg(unix)]
    std::thread::Builder::new()
        .name("agent-reaper".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            let mut child = child;
            let _ = child.wait();
        })
        .map_err(|_| "local_agent_started_reaper_unavailable_check_status")?;
    #[cfg(not(unix))]
    drop(child);
    println!(
        "Agent start requested. Use status to check readiness. Minecraft startup is unchanged."
    );
    Ok(())
}

/// Caller holds the server lease and has proved all game/runtime processes quiescent.
pub fn disable_quiescent(directory: &Path) -> Result<()> {
    if let Some(registration) = load(directory)? {
        server_startup::disable(&registration).map_err(|error| {
            eprintln!("{error}");
            "local_agent_startup_removal_failed"
        })?;
    }
    Ok(())
}

pub fn command(root: &Path, name: &str, choice: Option<AgentStartupChoice>) -> Result<()> {
    let _registry = local_files::lock(&root.join("registry.lock"))?;
    let server = servers::resolve(root, name)?;
    match choice {
        Some(AgentStartupChoice::Boot) => {
            preflight(AgentStartupChoice::Boot)?;
            enable(&server.directory)?;
            println!("Agent startup: machine boot. Minecraft startup is unchanged.");
        }
        Some(AgentStartupChoice::Manual) => {
            let _lease = local_files::lock(
                &servers::managed_lease(&server.directory).ok_or("local_record_invalid")?,
            )?;
            crate::sessions::require_stopped(&server.directory)?;
            disable_quiescent(&server.directory)?;
            println!("Agent startup: manual. Minecraft startup is unchanged.");
        }
        None => {
            let enabled = match load(&server.directory)? {
                None => false,
                Some(registration) => {
                    server_startup::status(&registration).map_err(|error| {
                        eprintln!("{error}");
                        "local_agent_startup_status_failed"
                    })? == Status::Enabled
                }
            };
            println!(
                "Agent startup: {}. Minecraft startup is separate.",
                if enabled { "machine boot" } else { "manual" }
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_choices_do_not_prompt_and_manual_needs_no_os_registration() {
        assert_eq!(
            choose(Some(AgentStartupChoice::Manual)).unwrap(),
            AgentStartupChoice::Manual
        );
        assert_eq!(
            choose(Some(AgentStartupChoice::Boot)).unwrap(),
            AgentStartupChoice::Boot
        );
        assert!(preflight(AgentStartupChoice::Manual).is_ok());
    }
    #[test]
    fn receipts_cannot_redirect_startup_actions_to_another_server() {
        let temp = tempfile::tempdir().unwrap();
        let id = "srv_0123456789abcdef0123456789abcdef";
        let directory = temp.path().join(id);
        fs::create_dir(&directory).unwrap();
        assert!(load(&directory).unwrap().is_none());
        assert!(disable_quiescent(&directory).is_ok());
        let mut receipt = Registration {
            server_id: id.into(),
            server_dir: fs::canonicalize(&directory).unwrap(),
            executable: std::env::current_exe().unwrap(),
            unit_directory: temp.path().join("units"),
            principal: "test".into(),
        };
        let path = directory.join(RECEIPT);
        fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert!(load(&directory).unwrap().is_some());
        receipt.server_dir = temp.path().join("foreign");
        fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert_eq!(
            load(&directory).unwrap_err(),
            "local_agent_startup_receipt_identity_changed"
        );
        assert!(disable_quiescent(&directory).is_err());
        assert!(!temp.path().join("units").exists());
    }
}
