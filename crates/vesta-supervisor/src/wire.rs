//! Version 1 agent-supervisor messages: closed, bounded, no paths, argv or launch input.
//!
//! A four-byte big-endian length precedes each JSON frame. Unknown fields, operations
//! and versions refuse. Generations travel as their canonical opaque hex lookup key.
use crate::Result;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use vesta_game_runtime::{
    console::{ConsoleLine, ConsoleStream},
    process::{ExitSummary, GenerationId, ProcessPhase, ProcessSnapshot},
};

pub const VERSION: u8 = 1;
pub const MAX_REQUEST_FRAME: usize = 16 * 1024;
pub const MAX_RESPONSE_FRAME: usize = 4 * 1024 * 1024;
/// Console text per sync response; JSON escaping stays within the response frame.
pub const MAX_SYNC_TEXT_BYTES: usize = 512 * 1024;
pub const MAX_WAIT_MS: u32 = 300_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    /// Identity, launch binding and current snapshot; no effect.
    Hello,
    /// Temporary responder controlled by the concrete game owner. A generation
    /// mismatch refuses configuration; returned source addresses are wake hints.
    Offline {
        expected_generation: Option<String>,
        configuration: Option<vesta_game_runtime::offline::Configuration>,
    },
    /// Snapshot plus console lines after the supervisor cursor; no effect.
    Sync {
        after: u64,
    },
    /// Launch the supervisor's own trusted definition, bound to its identity.
    Start {
        launch_identity: String,
    },
    /// Fixed graceful stop line, acknowledged after the actual stdin write.
    QueueStop {
        generation: String,
    },
    SleepStop {
        generation: String,
        expect_latest: u64,
    },
    /// Graceful stop and bounded wait for exit; never escalates to kill.
    Stop {
        generation: String,
        wait_ms: u32,
    },
    /// Explicit kill of the exact generation; a separate, deliberate request.
    Kill {
        generation: String,
        wait_ms: u32,
    },
    /// One validated console line, acknowledged after the actual stdin write.
    Command {
        generation: String,
        line: String,
    },
    /// The literal player-list command, written only if the supervisor console has no
    /// output after `expect_latest`, so every later console line follows the write.
    List {
        generation: String,
        expect_latest: u64,
    },
    /// Clear an unresolved launch intent left by a crash between intent and record.
    /// Explicit operator action; refused while any game or orphan is live.
    ResolveLaunch,
    /// Suspend autosave for an online backup: durable lease, `save-off`, then
    /// `save-all flush`, each acknowledged by the server. `autosave_baseline` is
    /// retained for caller compatibility, but never replaces an acknowledgement:
    /// missing save-off acknowledgement refuses and leaves an unknown obligation.
    SaveSuspend {
        generation: String,
        lease_ms: u32,
        autosave_baseline: Option<bool>,
    },
    /// Extend an active lease for the same generation.
    SaveRenew {
        generation: String,
        token: String,
        lease_ms: u32,
    },
    /// End the lease now, restoring autosave if it was on before.
    SaveResume {
        generation: String,
        token: String,
    },
    Hold {
        lease_ms: u32,
    },
    HoldRenew {
        token: String,
        lease_ms: u32,
    },
    HoldRelease {
        token: String,
    },
    /// Apply the agent's current trusted crash-restart policy (`None` disables it).
    /// Sent on every attach so configuration changes reach a running supervisor.
    Configure {
        restart: Option<WireRestartPolicy>,
    },
    /// Clear a crash-loop suspension, crash history and any scheduled restart.
    /// Explicit operator action; never starts or stops anything.
    ResetRestarts,
    /// Exit the supervisor; refused while a game generation or orphan is live.
    Shutdown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WireRestartPolicy {
    pub crash_limit: u32,
    pub window_secs: u64,
    pub backoff_secs: u64,
    pub max_backoff_secs: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u8,
    pub op: Op,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WirePhase {
    Stopped,
    Running,
    Stopping,
    Draining,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WireExit {
    pub generation: String,
    pub success: bool,
    pub code: Option<i32>,
    pub output_failed: bool,
    pub stdin_failed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WireSnapshot {
    pub generation: Option<String>,
    pub phase: WirePhase,
    pub output_failed: bool,
    pub stdin_failed: bool,
    pub last_exit: Option<WireExit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WireLine {
    /// Supervisor ring cursor; a jump from the previous line is lost output.
    pub cursor: u64,
    pub stderr: bool,
    pub text: String,
    pub truncated: bool,
}

/// Conditions that refuse launching or mark observation as unproven.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Health {
    /// A game from an earlier supervisor is alive or unobservable; starting is refused.
    pub orphan_running: bool,
    /// A launch intent has no recorded outcome; starting is refused until resolved.
    pub launch_unresolved: bool,
    /// Native observation failed at least once; current facts are not proven.
    pub observation_failed: bool,
    /// Opt-in crash restart is configured.
    pub restart_enabled: bool,
    /// The crash-loop breaker tripped: no automatic restart until reset.
    pub crash_loop: bool,
    /// Crashes inside the configured window.
    pub recent_crashes: u32,
    /// An automatic restart is scheduled this many milliseconds from now.
    pub restart_pending_ms: Option<u64>,
    /// The breaker is tripped in memory but its durable marker could not be written yet.
    pub restart_state_unsaved: bool,
    /// Milliseconds left on an active save lease (autosave suspended).
    pub save_lease_ms: Option<u64>,
    /// A previous supervisor's lease on a still-running game: autosave may stay off
    /// until that game restarts, because nothing holds its stdin.
    pub save_lease_stranded: bool,
    pub save_state_unknown: bool,
    pub maintenance_hold: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    /// Supervisor process ID, for diagnostics only; never authority.
    pub supervisor_pid: u32,
    pub launch_identity: String,
    /// Canonical state directory this supervisor serves.
    pub state: String,
    pub health: Health,
    pub snapshot: WireSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SyncPage {
    pub snapshot: WireSnapshot,
    pub lines: Vec<WireLine>,
    pub next_cursor: u64,
    pub latest_cursor: u64,
    pub gap: bool,
    pub health: Health,
    /// Supervisor console cursor just before the current generation was launched:
    /// lines after it belong to that generation.
    pub generation_floor: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Hello(Box<Hello>),
    Sync(Box<SyncPage>),
    Started {
        generation: String,
    },
    Delivered,
    Resolved,
    RestartsReset,
    Configured,
    Offline {
        wakes: Vec<std::net::IpAddr>,
    },
    Held {
        token: String,
    },
    HoldRenewed,
    HoldReleased,
    SaveSuspended {
        token: String,
        /// Autosave will be turned back on when the lease ends.
        restore_autosave: bool,
    },
    SaveRenewed,
    SaveResumed {
        autosave_restored: bool,
    },
    Exited {
        exit: WireExit,
    },
    TimedOut {
        snapshot: WireSnapshot,
    },
    ShuttingDown,
    /// Fixed redacted code; never echoes input, paths or console text.
    Refused {
        code: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u8,
    pub reply: Reply,
}

pub fn decode_request(bytes: &[u8]) -> Result<Request> {
    let request: Request =
        serde_json::from_slice(bytes).map_err(|_| "supervisor_invalid_request")?;
    if request.version != VERSION {
        return Err("supervisor_invalid_request");
    }
    match &request.op {
        Op::Stop { wait_ms, .. } | Op::Kill { wait_ms, .. } if *wait_ms > MAX_WAIT_MS => {
            Err("supervisor_invalid_request")
        }
        Op::Command { line, .. } if line.len() > 1024 => Err("supervisor_invalid_request"),
        Op::SaveSuspend { lease_ms, .. }
        | Op::SaveRenew { lease_ms, .. }
        | Op::Hold { lease_ms }
        | Op::HoldRenew { lease_ms, .. }
            if !(crate::save::MIN_LEASE_MS..=crate::save::MAX_LEASE_MS).contains(lease_ms) =>
        {
            Err("supervisor_invalid_request")
        }
        _ => Ok(request),
    }
}

pub fn decode_response(bytes: &[u8]) -> Result<Reply> {
    let response: Response =
        serde_json::from_slice(bytes).map_err(|_| "supervisor_invalid_response")?;
    if response.version != VERSION {
        return Err("supervisor_invalid_response");
    }
    Ok(response.reply)
}

fn encode<T: Serialize>(value: &T, maximum: usize) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|_| "supervisor_invalid_frame")?;
    if bytes.is_empty() || bytes.len() > maximum {
        return Err("supervisor_invalid_frame");
    }
    let mut framed = Vec::with_capacity(bytes.len() + 4);
    framed.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    framed.extend_from_slice(&bytes);
    Ok(framed)
}

pub fn encode_request(op: Op) -> Result<Vec<u8>> {
    encode(
        &Request {
            version: VERSION,
            op,
        },
        MAX_REQUEST_FRAME,
    )
}

pub fn encode_reply(reply: Reply) -> Result<Vec<u8>> {
    encode(
        &Response {
            version: VERSION,
            reply,
        },
        MAX_RESPONSE_FRAME,
    )
}

pub async fn read_async<R: AsyncRead + Unpin>(reader: &mut R, maximum: usize) -> Result<Vec<u8>> {
    let length = reader
        .read_u32()
        .await
        .map_err(|_| "supervisor_invalid_frame")? as usize;
    if length == 0 || length > maximum {
        return Err("supervisor_invalid_frame");
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|_| "supervisor_invalid_frame")?;
    Ok(bytes)
}

pub async fn write_async<W: AsyncWrite + Unpin>(writer: &mut W, framed: &[u8]) -> Result<()> {
    writer
        .write_all(framed)
        .await
        .map_err(|_| "supervisor_io_failed")?;
    writer.flush().await.map_err(|_| "supervisor_io_failed")
}

pub fn read_blocking<R: Read>(reader: &mut R, maximum: usize) -> Result<Vec<u8>> {
    let mut length = [0; 4];
    reader
        .read_exact(&mut length)
        .map_err(|_| "supervisor_invalid_frame")?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > maximum {
        return Err("supervisor_invalid_frame");
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| "supervisor_invalid_frame")?;
    Ok(bytes)
}

pub fn write_blocking<W: Write>(writer: &mut W, framed: &[u8]) -> Result<()> {
    writer
        .write_all(framed)
        .and_then(|()| writer.flush())
        .map_err(|_| "supervisor_io_failed")
}

pub fn snapshot_to_wire(snapshot: &ProcessSnapshot) -> WireSnapshot {
    WireSnapshot {
        generation: snapshot.generation.map(GenerationId::to_hex),
        phase: match snapshot.phase {
            ProcessPhase::Stopped => WirePhase::Stopped,
            ProcessPhase::Running => WirePhase::Running,
            ProcessPhase::Stopping => WirePhase::Stopping,
            ProcessPhase::Draining => WirePhase::Draining,
        },
        output_failed: snapshot.output_failed,
        stdin_failed: snapshot.stdin_failed,
        last_exit: snapshot.last_exit.map(|exit| exit_to_wire(&exit)),
    }
}

pub fn exit_to_wire(exit: &ExitSummary) -> WireExit {
    WireExit {
        generation: exit.generation.to_hex(),
        success: exit.success,
        code: exit.code,
        output_failed: exit.output_failed,
        stdin_failed: exit.stdin_failed,
    }
}

pub fn exit_from_wire(exit: &WireExit) -> Result<ExitSummary> {
    Ok(ExitSummary {
        generation: GenerationId::parse_hex(&exit.generation)
            .map_err(|_| "supervisor_invalid_response")?,
        success: exit.success,
        code: exit.code,
        output_failed: exit.output_failed,
        stdin_failed: exit.stdin_failed,
    })
}

pub fn snapshot_from_wire(snapshot: &WireSnapshot) -> Result<ProcessSnapshot> {
    Ok(ProcessSnapshot {
        generation: snapshot
            .generation
            .as_deref()
            .map(GenerationId::parse_hex)
            .transpose()
            .map_err(|_| "supervisor_invalid_response")?,
        phase: match snapshot.phase {
            WirePhase::Stopped => ProcessPhase::Stopped,
            WirePhase::Running => ProcessPhase::Running,
            WirePhase::Stopping => ProcessPhase::Stopping,
            WirePhase::Draining => ProcessPhase::Draining,
        },
        output_failed: snapshot.output_failed,
        stdin_failed: snapshot.stdin_failed,
        last_exit: snapshot
            .last_exit
            .as_ref()
            .map(exit_from_wire)
            .transpose()?,
    })
}

pub fn line_to_wire(line: &ConsoleLine) -> WireLine {
    WireLine {
        cursor: line.cursor,
        stderr: line.stream == ConsoleStream::Stderr,
        text: line.text.clone(),
        truncated: line.truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_refuse_unknown_fields_ops_versions_and_oversized_values() {
        for bytes in [
            br#"{"version":1,"op":"launch"}"#.as_slice(),
            br#"{"version":2,"op":"hello"}"#,
            br#"{"version":1,"op":"hello","path":"/bin/sh"}"#,
            br#"{"version":1,"op":{"start":{"launch_identity":"x","argv":["sh"]}}}"#,
            br#"{"version":1,"op":{"stop":{"generation":"x","wait_ms":300001}}}"#,
        ] {
            assert!(decode_request(bytes).is_err());
        }
        let long = format!(
            r#"{{"version":1,"op":{{"command":{{"generation":"x","line":"{}"}}}}}}"#,
            "a".repeat(1025)
        );
        assert!(decode_request(long.as_bytes()).is_err());
        assert_eq!(
            decode_request(br#"{"version":1,"op":{"sync":{"after":4}}}"#)
                .unwrap()
                .op,
            Op::Sync { after: 4 }
        );
    }

    #[test]
    fn frames_round_trip_and_refuse_truncated_empty_or_oversized() {
        let framed = encode_request(Op::Hello).unwrap();
        let bytes = read_blocking(&mut framed.as_slice(), MAX_REQUEST_FRAME).unwrap();
        assert_eq!(decode_request(&bytes).unwrap().op, Op::Hello);
        for bytes in [
            vec![0, 0],
            vec![0, 0, 0, 0],
            vec![0, 0, 0, 4, b'x'],
            vec![0, 1, 0, 0],
        ] {
            assert!(read_blocking(&mut bytes.as_slice(), MAX_REQUEST_FRAME).is_err());
        }
        let reply = encode_reply(Reply::Refused {
            code: "supervisor_busy".into(),
        })
        .unwrap();
        let bytes = read_blocking(&mut reply.as_slice(), MAX_RESPONSE_FRAME).unwrap();
        assert!(matches!(
            decode_response(&bytes).unwrap(),
            Reply::Refused { .. }
        ));
    }

    #[test]
    fn snapshots_round_trip_and_refuse_noncanonical_generations() {
        let wire = WireSnapshot {
            generation: Some("0123456789abcdef0123456789abcdef".into()),
            phase: WirePhase::Running,
            output_failed: false,
            stdin_failed: false,
            last_exit: None,
        };
        let native = snapshot_from_wire(&wire).unwrap();
        assert_eq!(snapshot_to_wire(&native), wire);
        let bad = WireSnapshot {
            generation: Some("0123456789ABCDEF0123456789ABCDEF".into()),
            ..wire
        };
        assert!(snapshot_from_wire(&bad).is_err());
    }
}
