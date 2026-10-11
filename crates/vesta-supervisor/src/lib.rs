//! Independent per-server process supervisor (P3, first slice).
//!
//! The supervisor owns one managed game: trusted launch, continuous output draining
//! into the bounded console ring, stdin delivery and exit observation. It has no
//! network listener; its only control surface is a protected same-account local
//! endpoint (Windows named pipe with a current-SID-only DACL, Linux owner-only Unix
//! socket with peer uid checks). It outlives agent restarts and never kills or
//! restarts the game on its own. A durable generation record (PID plus native start
//! time) prevents duplicate launches after any restart.
//!
//! Not in this slice: separate OS identities, crash-restart policy, readiness, save
//! leases, OS service installation, self-update and Linux execution evidence.
#![deny(unsafe_code)]

pub mod client;
pub mod native;
#[cfg(windows)]
pub mod pipe_security;
pub mod record;
pub mod remote;
pub mod restart;
pub mod server;
pub mod wire;

pub type Result<T> = std::result::Result<T, &'static str>;

use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub use client::SupervisorClient;
pub use remote::RemoteProcess;
pub use restart::RestartPolicy;
pub use server::{SupervisorConfig, run};

const READY_WAIT: Duration = Duration::from_secs(10);

/// SHA-256 over the launch definition bytes and the resolution context that gives
/// them meaning (base, document, game, state and working directory paths). Both sides
/// must agree before control; relative bytes alone do not identify an effective launch.
pub fn launch_identity(parts: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"vesta-launch-identity-v1");
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    format!("{:x}", digest.finalize())
}

/// Ask a running supervisor for its identity. `Ok(None)` means none is running.
pub fn probe(client: &SupervisorClient) -> Result<Option<wire::Hello>> {
    match client.call(wire::Op::Hello, Duration::from_secs(2)) {
        Ok(wire::Reply::Hello(hello)) => Ok(Some(*hello)),
        Err(client::NOT_RUNNING) => Ok(None),
        Ok(_) => Err("supervisor_invalid_response"),
        Err(error) => Err(error),
    }
}

/// Start `executable args…` detached from the caller's console and process group, so
/// agent exit or Ctrl+C never reaches the supervisor or its game. Waits until it
/// answers. The returned child is only for reaping; dropping it does not stop it.
/// Whether a supervisor may run inside an enclosing Windows job whose closure could end
/// it. Production requires independence; owned test fixtures may accept containment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Containment {
    RequireIndependent,
    AllowContained,
}

pub fn spawn_detached(
    client: &SupervisorClient,
    executable: &Path,
    args: &[&std::ffi::OsStr],
    containment: Containment,
) -> Result<(Child, wire::Hello)> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    let mut child = {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // Leave any enclosing job so closing it cannot take the supervisor down.
        command.creation_flags(
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB,
        );
        match command.spawn() {
            Ok(child) => child,
            // Breakaway refused: staying inside the caller's job is acceptable only when
            // closing that job cannot kill the supervisor and its game.
            Err(_)
                if containment == Containment::AllowContained
                    || pipe_security::job_allows_independent_lifetime() =>
            {
                command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
                command.spawn().map_err(|_| "supervisor_spawn_failed")?
            }
            Err(_) => return Err("supervisor_job_lifetime_unproven"),
        }
    };
    #[cfg(unix)]
    let mut child = {
        use std::os::unix::process::CommandExt;
        // Unix process groups are not killed with the parent; no job containment applies.
        let _ = containment;
        command.process_group(0);
        command.spawn().map_err(|_| "supervisor_spawn_failed")?
    };
    let until = Instant::now() + READY_WAIT;
    loop {
        if let Some(hello) = probe(client)? {
            return Ok((child, hello));
        }
        if child.try_wait().ok().flatten().is_some() {
            return Err("supervisor_exited_during_start");
        }
        if Instant::now() >= until {
            // Still owned by the caller; it may yet answer. Never killed here.
            return Err("supervisor_start_timed_out");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether a supervisor answers or a recorded game is still alive for this state.
/// Used to refuse destructive local actions such as deleting the instance.
pub fn instance_busy(state: &vesta_state::StateDirectory) -> Result<bool> {
    let client = SupervisorClient::new(state)?;
    if probe(&client)?.is_some() {
        return Ok(true);
    }
    let directory = state
        .supervisor()
        .map_err(|_| "supervisor_state_unavailable")?;
    // Fail closed: an unobservable recorded game, or an unresolved launch, counts as live.
    Ok(
        record::read(&directory)?.is_some_and(|record| record.held())
            || record::read_intent(&directory)?.is_some(),
    )
}
