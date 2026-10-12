//! Save leases: autosave suspension for an online backup, bounded and owned by the
//! supervisor so it is undone even if the agent or the backup disappears.
//!
//! The lease is written durably before `save-off` is sent and is tied to one exact
//! generation. On expiry or explicit release the supervisor sends `save-on` to that
//! same generation only, and only if autosave was on before (the server's reply to
//! `save-off` tells us). A different or exited generation is never sent anything: a
//! restarted Minecraft starts with autosave on. If the supervisor itself dies, a
//! replacement finds the lease; with the game still running it reports the lease as
//! stranded (no stdin is available to undo it) until the game exits.
//!
//! Missing save-off acknowledgement leaves an unresolved durable obligation even
//! with a caller baseline; it never authorizes save-on. Health reports it and new
//! leases refuse until that exact generation exits. Ending leases retain delivery
//! receipts until the save-on write completes, not merely queue admission.
//! Save commands from other clients are refused during the save window. Flush
//! acknowledgements are considered only after the observed save-off acknowledgement;
//! console gaps and generation changes abort. Residual limitation: the log protocol
//! has no command IDs, so in-game/admin/plugin save commands or forged server INFO
//! log messages can still interfere. Local supervisor Command calls are fenced.
//! Tokens uniquely bind suspend, renewal and release, including same-generation
//! sequential backups.
use crate::{
    Result,
    client::SupervisorClient,
    record,
    wire::{Op, Reply},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use vesta_game_runtime::process::GenerationId;
use vesta_game_runtime::readiness::server_info_message;
use vesta_state::artifacts::ArtifactDirectory;

// Alternating slots retain the newest durable obligation during replacement.
const SLOTS: [(&str, &str); 2] = [
    ("save-lease-a.json", "save-lease-a.staged"),
    ("save-lease-b.json", "save-lease-b.staged"),
];
pub const MAX_LEASE_MS: u32 = 30 * 60 * 1000;
pub const MIN_LEASE_MS: u32 = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SaveLease {
    pub version: u8,
    #[serde(default)]
    pub token: String,
    pub generation: String,
    pub expires_at_ms: i64,
    pub restore_autosave: bool,
    /// Never restore/discard an unknown prior state until the generation exits.
    #[serde(default = "unknown")]
    pub unresolved: bool,
    /// Restoration begun; retry admission/delivery until delivered or exit.
    #[serde(default)]
    pub ending: bool,
}
fn unknown() -> bool {
    true
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Slot {
    seq: u64,
    lease: SaveLease,
}
fn newest(directory: &ArtifactDirectory) -> Result<Option<(usize, Slot)>> {
    let mut newest: Option<(usize, Slot)> = None;
    let mut invalid = false;
    for (i, (name, _)) in SLOTS.iter().enumerate() {
        match record::read_json::<Slot>(directory, name) {
            Ok(Some(slot)) if slot.lease.version == 1 => {
                if newest.as_ref().is_none_or(|(_, n)| slot.seq > n.seq) {
                    newest = Some((i, slot));
                }
            }
            Ok(None) => {}
            _ => invalid = true,
        }
    }
    // Upgrade legacy single-file records conservatively. Their prior state was
    // not represented, so absent fields decode as unresolved.
    if let Some(lease) = record::read_json::<SaveLease>(directory, "save-lease.json")? {
        if lease.version != 1 {
            return Err("supervisor_record_invalid");
        }
        if newest.is_none() {
            newest = Some((1, Slot { seq: 0, lease }));
        }
    }
    if newest.is_none() && invalid {
        return Err("supervisor_record_invalid");
    }
    Ok(newest)
}
pub fn read(directory: &ArtifactDirectory) -> Result<Option<SaveLease>> {
    Ok(newest(directory)?.map(|(_, slot)| slot.lease))
}
pub fn write(directory: &ArtifactDirectory, lease: &SaveLease) -> Result<()> {
    if newest(directory)?.is_some() {
        return Err("supervisor_record_exists");
    }
    record::publish(
        directory,
        SLOTS[0].1,
        SLOTS[0].0,
        &Slot {
            seq: 1,
            lease: lease.clone(),
        },
    )
}
/// Failed/interrupted replacement never removes the newest valid record.
pub fn replace(directory: &ArtifactDirectory, lease: &SaveLease) -> Result<()> {
    let (index, old) = newest(directory)?.ok_or("supervisor_record_invalid")?;
    let seq = old.seq.checked_add(1).ok_or("supervisor_record_invalid")?;
    let (name, staged) = SLOTS[1 - index];
    directory
        .remove(&record::name(name)?)
        .map_err(|_| "supervisor_record_unavailable")?;
    record::publish(
        directory,
        staged,
        name,
        &Slot {
            seq,
            lease: lease.clone(),
        },
    )
}
pub fn clear(directory: &ArtifactDirectory) -> Result<()> {
    let newest = newest(directory)?.map(|(i, _)| i).unwrap_or(0);
    directory
        .remove(&record::name("save-lease.json")?)
        .map_err(|_| "supervisor_record_unavailable")?;
    // Older first: interrupted clear retains the newest obligation.
    for i in [1 - newest, newest] {
        directory
            .remove(&record::name(SLOTS[i].0)?)
            .map_err(|_| "supervisor_record_unavailable")?;
    }
    directory
        .sync()
        .map_err(|_| "supervisor_record_unavailable")
}
pub(crate) fn token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "supervisor_random_unavailable")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
/// In-memory, monotonic stopped-game hold, lost on supervisor exit. Clients must
/// keep contact throughout a stopped backup. Tokens fence stale release/renewal.
pub fn hold(client: &SupervisorClient, lease: Duration) -> Result<String> {
    let lease_ms = u32::try_from(lease.as_millis()).map_err(|_| "supervisor_invalid_request")?;
    match client.call(Op::Hold { lease_ms }, Duration::from_secs(2))? {
        Reply::Held { token } => Ok(token),
        _ => Err("supervisor_hold_refused"),
    }
}
pub fn renew_hold(client: &SupervisorClient, token: &str, lease: Duration) -> Result<()> {
    let lease_ms = u32::try_from(lease.as_millis()).map_err(|_| "supervisor_invalid_request")?;
    match client.call(
        Op::HoldRenew {
            token: token.into(),
            lease_ms,
        },
        Duration::from_secs(2),
    )? {
        Reply::HoldRenewed => Ok(()),
        _ => Err("supervisor_hold_refused"),
    }
}
pub fn release_hold(client: &SupervisorClient, token: &str) -> Result<()> {
    match client.call(
        Op::HoldRelease {
            token: token.into(),
        },
        Duration::from_secs(2),
    )? {
        Reply::HoldReleased => Ok(()),
        _ => Err("supervisor_hold_refused"),
    }
}

/// Server acknowledgements for the save commands, across supported versions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveAck {
    /// `save-off` turned autosave off: it was on before.
    Disabled,
    /// `save-off` found autosave already off.
    AlreadyOff,
    /// `save-all flush` completed.
    Saved,
    /// `save-on` turned autosave on, or it already was.
    Enabled,
}

pub fn classify(line: &str) -> Option<SaveAck> {
    match server_info_message(line)? {
        "Automatic saving is now disabled" | "Turned off world auto-saving" => {
            Some(SaveAck::Disabled)
        }
        "Saving is already turned off" => Some(SaveAck::AlreadyOff),
        "Saved the game" | "Saved the world" => Some(SaveAck::Saved),
        "Automatic saving is now enabled"
        | "Turned on world auto-saving"
        | "Saving is already turned on" => Some(SaveAck::Enabled),
        _ => None,
    }
}

/// Client side of the lease protocol, usable from any agent thread.
/// Suspend autosave under a supervisor-owned lease for an online backup. Returns
/// the unique lease token and whether autosave will be restored when it ends.
pub fn suspend(
    client: &SupervisorClient,
    generation: GenerationId,
    lease: Duration,
    autosave_baseline: Option<bool>,
) -> Result<(String, bool)> {
    let lease_ms = u32::try_from(lease.as_millis()).map_err(|_| "supervisor_invalid_request")?;
    match client.call(
        Op::SaveSuspend {
            generation: generation.to_hex(),
            lease_ms,
            autosave_baseline,
        },
        Duration::from_secs(150),
    )? {
        Reply::SaveSuspended {
            token,
            restore_autosave,
        } => Ok((token, restore_autosave)),
        Reply::Refused { .. } => Err("supervisor_save_refused"),
        _ => Err("supervisor_invalid_response"),
    }
}

pub fn renew(
    client: &SupervisorClient,
    generation: GenerationId,
    token: &str,
    lease: Duration,
) -> Result<()> {
    let lease_ms = u32::try_from(lease.as_millis()).map_err(|_| "supervisor_invalid_request")?;
    match client.call(
        Op::SaveRenew {
            token: token.into(),
            generation: generation.to_hex(),
            lease_ms,
        },
        Duration::from_secs(2),
    )? {
        Reply::SaveRenewed => Ok(()),
        _ => Err("supervisor_save_refused"),
    }
}

/// End the lease now. Returns whether autosave was turned back on.
pub fn resume(client: &SupervisorClient, generation: GenerationId, token: &str) -> Result<bool> {
    match client.call(
        Op::SaveResume {
            token: token.into(),
            generation: generation.to_hex(),
        },
        Duration::from_secs(12),
    )? {
        Reply::SaveResumed { autosave_restored } => Ok(autosave_restored),
        _ => Err("supervisor_save_refused"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledgements_come_only_from_server_log_lines() {
        assert_eq!(
            classify("[12:00:00] [Server thread/INFO]: Automatic saving is now disabled"),
            Some(SaveAck::Disabled)
        );
        assert_eq!(
            classify("[12:00:00] [Server thread/INFO]: Saving is already turned off"),
            Some(SaveAck::AlreadyOff)
        );
        assert_eq!(
            classify("[12:00:00 INFO]: Saved the world"),
            Some(SaveAck::Saved)
        );
        assert_eq!(
            classify("[12:00:00] [Server thread/INFO]: Saved the game"),
            Some(SaveAck::Saved)
        );
        // Chat, other levels and unrelated lines never acknowledge.
        for line in [
            "[12:00:00] [Server thread/INFO]: <Steve> Saved the game",
            "[12:00:00] [Server thread/WARN]: Saved the game",
            "Saved the game",
            "[12:00:00] [Server thread/INFO]: Saved the game!",
        ] {
            assert_eq!(classify(line), None, "{line}");
        }
    }

    #[test]
    fn lease_records_are_create_new_and_replaceable() {
        let temp = tempfile::tempdir().unwrap();
        let state = vesta_state::StateDirectory::new(temp.path().join("state"), None).unwrap();
        state.prepare().unwrap();
        let directory = state.supervisor().unwrap();
        let lease = SaveLease {
            version: 1,
            token: "test".into(),
            unresolved: false,
            ending: false,
            generation: "0123456789abcdef0123456789abcdef".into(),
            expires_at_ms: 1,
            restore_autosave: true,
        };
        write(&directory, &lease).unwrap();
        assert_eq!(write(&directory, &lease), Err("supervisor_record_exists"));
        let renewed = SaveLease {
            expires_at_ms: 2,
            ..lease
        };
        replace(&directory, &renewed).unwrap();
        assert_eq!(read(&directory).unwrap(), Some(renewed));
        clear(&directory).unwrap();
        assert_eq!(read(&directory).unwrap(), None);
    }
    #[test]
    fn interrupted_or_failed_replace_preserves_newest_valid_record() {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let state = vesta_state::StateDirectory::new(temp.path().join("state"), None).unwrap();
        state.prepare().unwrap();
        let directory = state.supervisor().unwrap();
        let lease = SaveLease {
            version: 1,
            token: "test".into(),
            generation: "0123456789abcdef0123456789abcdef".into(),
            expires_at_ms: 1,
            restore_autosave: true,
            unresolved: false,
            ending: false,
        };
        write(&directory, &lease).unwrap();
        let older = record::name(SLOTS[1].0).unwrap();
        directory
            .create_new(&older)
            .unwrap()
            .write_all(b"{interrupted")
            .unwrap();
        assert_eq!(read(&directory).unwrap(), Some(lease.clone()));
        // A retained handle makes deletion fail on Windows; a directory at the
        // staging name also deterministically prevents publication everywhere.
        let staged = temp
            .path()
            .join("state")
            .join(vesta_state::artifacts::SUPERVISOR_DIR)
            .join(SLOTS[1].1);
        std::fs::create_dir(&staged).unwrap();
        let renewed = SaveLease {
            expires_at_ms: 2,
            ..lease.clone()
        };
        assert!(replace(&directory, &renewed).is_err());
        assert_eq!(read(&directory).unwrap(), Some(lease));
        std::fs::remove_dir(staged).unwrap();
        replace(&directory, &renewed).unwrap();
        assert_eq!(read(&directory).unwrap(), Some(renewed));
        clear(&directory).unwrap();
        assert!(read(&directory).unwrap().is_none());
    }

    #[test]
    fn legacy_marker_is_not_lost_and_is_conservatively_unknown() {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let state = vesta_state::StateDirectory::new(temp.path().join("state"), None).unwrap();
        state.prepare().unwrap();
        let directory = state.supervisor().unwrap();
        directory.create_new(&record::name("save-lease.json").unwrap()).unwrap()
            .write_all(br#"{"version":1,"generation":"0123456789abcdef0123456789abcdef","expires_at_ms":0,"restore_autosave":true}"#).unwrap();
        assert!(read(&directory).unwrap().unwrap().unresolved);
        clear(&directory).unwrap();
        assert!(read(&directory).unwrap().is_none());
    }
}
