//! Agent side of `[process] supervision = "separate"`: find or start the per-server
//! supervisor, bind to it by launch-definition identity, and release it when idle.
//! The agent never launches the game itself in this mode and never stops it on exit.
use crate::local_files::Result;
use std::{ffi::OsStr, path::Path, sync::Arc, time::Duration};
use vesta_control::{config::HostConfiguration, host::supervised_launch};
use vesta_state::StateDirectory;
use vesta_supervisor::{RemoteProcess, SupervisorClient, wire};

pub struct Attached {
    pub remote: RemoteProcess,
    pub state: StateDirectory,
}

pub fn attach(config: &HostConfiguration, source: &Path, base: &Path) -> Result<Attached> {
    let launch = supervised_launch(config).map_err(|e| e.code())?;
    launch
        .state
        .prepare()
        .map_err(|_| "supervisor_state_unavailable")?;
    let client = Arc::new(SupervisorClient::new(&launch.state)?);
    if vesta_supervisor::probe(&client)?.is_none() {
        let executable = std::env::current_exe()
            .map_err(|_| "supervisor_executable_unavailable")?
            .with_file_name(format!("vesta-supervisor{}", std::env::consts::EXE_SUFFIX));
        // Dropping the child handle neither stops nor detaches more than it already is.
        let (_child, _) = vesta_supervisor::spawn_detached(
            &client,
            &executable,
            &[
                OsStr::new("--host"),
                source.as_os_str(),
                OsStr::new("--base"),
                base.as_os_str(),
            ],
            vesta_supervisor::Containment::RequireIndependent,
        )?;
    }
    let mut identity = launch.identity;
    if let Some(hello) = vesta_supervisor::probe(&client)?
        && hello.launch_identity != identity
        && hello.snapshot.generation.is_none()
        && hello.snapshot.phase == wire::WirePhase::Stopped
        && !hello.health.orphan_running
        && !hello.health.launch_unresolved
        && !hello.health.observation_failed
        && hello.health.restart_pending_ms.is_none()
        && vesta_control::host::software_recovery_binding(config, &hello.launch_identity)
            .map_err(|e| e.code())?
    {
        identity = hello.launch_identity;
    }
    let remote = RemoteProcess::attach(client, identity)?;
    if remote.orphan_running() {
        // A game outlived an earlier supervisor: observable through its own logs only.
        eprintln!("supervisor_orphaned_game_running; degraded_no_stdin; start_refused");
    }
    Ok(Attached {
        remote,
        state: launch.state,
    })
}

/// Asks an attached supervisor to exit once this agent is done, including on startup
/// failure paths. A running game keeps it alive; that refusal is normal and harmless.
pub struct Release(Option<StateDirectory>);
impl Release {
    pub fn new(state: StateDirectory) -> Self {
        Self(Some(state))
    }
    pub fn none() -> Self {
        Self(None)
    }
    /// Normal shutdown: release and report what happened.
    pub fn finish(mut self) {
        if let Some(state) = self.0.take() {
            if release(&state) {
                println!("supervisor_released");
            } else {
                println!("supervisor_retained");
            }
        }
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        if let Some(state) = self.0.take() {
            let _ = release(&state);
        }
    }
}

/// Ask an idle supervisor to exit. A running game keeps it alive; that refusal is normal.
pub fn release(state: &StateDirectory) -> bool {
    let Ok(client) = SupervisorClient::new(state) else {
        return false;
    };
    matches!(
        client.call(wire::Op::Shutdown, Duration::from_secs(2)),
        Ok(wire::Reply::ShuttingDown)
    )
}

/// Operator resolution after a supervisor crashed between recording a launch intent
/// and its outcome. The operator confirms no game from that launch is still running;
/// the supervisor clears the intent only while no game or orphan is live. Nothing is
/// started, stopped or killed.
pub fn resolve_launch(root: &Path, name: &str, confirm: &str) -> Result<()> {
    if name != confirm {
        return Err("local_resolve_confirmation_mismatch");
    }
    request(root, name, wire::Op::ResolveLaunch, wire::Reply::Resolved)
}

/// Clear a tripped crash-loop breaker, crash history and any scheduled restart, so
/// opt-in crash restart applies again. Never starts or stops the game.
pub fn reset_restarts(root: &Path, name: &str) -> Result<()> {
    request(
        root,
        name,
        wire::Op::ResetRestarts,
        wire::Reply::RestartsReset,
    )
}

fn request(root: &Path, name: &str, op: wire::Op, expected: wire::Reply) -> Result<()> {
    let server = crate::servers::resolve(root, name)?;
    let config =
        HostConfiguration::load_file(&server.config, &server.directory).map_err(|e| e.code())?;
    let launch = supervised_launch(&config).map_err(|e| e.code())?;
    let client = SupervisorClient::new(&launch.state)?;
    match client.call(op, Duration::from_secs(5)) {
        Ok(reply) if reply == expected => Ok(()),
        Ok(wire::Reply::Refused { .. }) => Err("supervisor_request_refused"),
        Err(vesta_supervisor::client::NOT_RUNNING) => {
            Err("supervisor_not_running_start_serve_first")
        }
        _ => Err("supervisor_outcome_unknown_do_not_retry"),
    }
}

/// Initialize the host-selected repository, or the safe instance default.
pub fn backup_init(root: &Path, name: &str) -> Result<()> {
    let server = crate::servers::resolve(root, name)?;
    let config =
        HostConfiguration::load_file(&server.config, &server.directory).map_err(|e| e.code())?;
    let default_backups = vesta_control::config::BackupSettings::for_instance(config.base());
    let settings = config.backups().unwrap_or(&default_backups);
    let state = StateDirectory::new(config.state(), Some(config.game()))
        .map_err(|_| "local_state_refused")?;
    vesta_control::backups::initialize_repository(&state, settings, config.game())
}
