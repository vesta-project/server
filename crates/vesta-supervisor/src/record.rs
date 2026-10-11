//! Durable launch state owned by the supervisor.
//!
//! Before spawning, the supervisor publishes a launch intent. After spawning it
//! publishes the generation record (PID, native start time and, on Linux, boot ID)
//! and only then retires the intent. Each file is written to a staging name, synced and
//! hard-linked into place, so a published record is always complete. Recovery:
//! - a generation record whose process is alive, or cannot be observed, means a game
//!   outlived its supervisor. Nothing may launch a duplicate and nothing kills it;
//! - an intent without a generation record means a crash between intent and record:
//!   a child may exist that nothing can identify. Launching stays refused until an
//!   operator explicitly resolves it.
use crate::{
    Result,
    native::{self, Liveness},
};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use vesta_state::artifacts::{ArtifactDirectory, ArtifactName};

const GENERATION: &str = "generation.json";
const GENERATION_STAGED: &str = "generation.staged";
const INTENT: &str = "launch-intent.json";
const INTENT_STAGED: &str = "launch-intent.staged";
const MAX_RECORD_BYTES: u64 = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationRecord {
    pub version: u8,
    pub generation: String,
    pub pid: u32,
    pub process_start: u64,
    #[serde(default)]
    pub boot_id: Option<String>,
    pub launch_identity: String,
}
impl GenerationRecord {
    pub fn liveness(&self) -> Liveness {
        native::liveness(self.pid, self.process_start, self.boot_id.as_deref())
    }
    /// Fail closed: an unobservable recorded process is treated as still running.
    pub fn held(&self) -> bool {
        self.liveness() != Liveness::Gone
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LaunchIntent {
    pub version: u8,
    pub launch_identity: String,
}

pub(crate) fn name(text: &str) -> Result<ArtifactName> {
    ArtifactName::new(text).map_err(|_| "supervisor_record_unavailable")
}

pub(crate) fn read_json<T: for<'de> Deserialize<'de>>(
    directory: &ArtifactDirectory,
    file: &str,
) -> Result<Option<T>> {
    let Some(handle) = directory
        .open(&name(file)?)
        .map_err(|_| "supervisor_record_unavailable")?
    else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    handle
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "supervisor_record_unavailable")?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err("supervisor_record_invalid");
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "supervisor_record_invalid")
}

pub(crate) fn publish<T: Serialize>(
    directory: &ArtifactDirectory,
    staged: &str,
    published: &str,
    value: &T,
) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| "supervisor_record_unavailable")?;
    let (staged, published) = (name(staged)?, name(published)?);
    // A leftover staging name was never published; it carries no state.
    directory
        .remove(&staged)
        .map_err(|_| "supervisor_record_unavailable")?;
    let mut file = directory
        .create_new(&staged)
        .map_err(|_| "supervisor_record_unavailable")?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| "supervisor_record_unavailable")?;
    drop(file);
    let linked = directory.link_new(&staged, &published).map_err(|error| {
        if error.code() == "state_artifact_exists" {
            "supervisor_record_exists"
        } else {
            "supervisor_record_unavailable"
        }
    });
    let _ = directory.remove(&staged);
    linked?;
    directory
        .sync()
        .map_err(|_| "supervisor_record_unavailable")
}

/// The generation record. A malformed or oversized record refuses, never ignored.
pub fn read(directory: &ArtifactDirectory) -> Result<Option<GenerationRecord>> {
    let record: Option<GenerationRecord> = read_json(directory, GENERATION)?;
    if record.as_ref().is_some_and(|record| record.version != 1) {
        return Err("supervisor_record_invalid");
    }
    Ok(record)
}

pub fn read_intent(directory: &ArtifactDirectory) -> Result<Option<LaunchIntent>> {
    let intent: Option<LaunchIntent> = read_json(directory, INTENT)?;
    if intent.as_ref().is_some_and(|intent| intent.version != 1) {
        return Err("supervisor_record_invalid");
    }
    Ok(intent)
}

/// Publish the generation record; an existing record refuses (`supervisor_record_exists`).
pub fn write(directory: &ArtifactDirectory, record: &GenerationRecord) -> Result<()> {
    publish(directory, GENERATION_STAGED, GENERATION, record)
}

/// Publish the launch intent before any spawn; an existing intent refuses.
pub fn write_intent(directory: &ArtifactDirectory, intent: &LaunchIntent) -> Result<()> {
    publish(directory, INTENT_STAGED, INTENT, intent)
}

/// Remove the record after the recorded process was observed exited.
pub fn clear(directory: &ArtifactDirectory) -> Result<()> {
    directory
        .remove(&name(GENERATION)?)
        .map(|_| ())
        .map_err(|_| "supervisor_record_unavailable")
}

/// Retire the intent once its outcome is recorded, or after explicit resolution.
pub fn clear_intent(directory: &ArtifactDirectory) -> Result<()> {
    directory
        .remove(&name(INTENT)?)
        .map(|_| ())
        .map_err(|_| "supervisor_record_unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vesta_state::StateDirectory;

    fn directory() -> (tempfile::TempDir, ArtifactDirectory) {
        let temp = tempfile::tempdir().unwrap();
        let state = StateDirectory::new(temp.path().join("state"), None).unwrap();
        state.prepare().unwrap();
        let directory = state.supervisor().unwrap();
        (temp, directory)
    }

    fn record(pid: u32, process_start: u64) -> GenerationRecord {
        GenerationRecord {
            version: 1,
            generation: "0123456789abcdef0123456789abcdef".into(),
            pid,
            process_start,
            boot_id: native::boot_id(),
            launch_identity: "aa".repeat(32),
        }
    }

    #[test]
    fn records_are_published_whole_once_and_cleared_explicitly() {
        let (_temp, directory) = directory();
        assert_eq!(read(&directory).unwrap(), None);
        let pid = std::process::id();
        let own = record(pid, native::process_start(pid).unwrap());
        write(&directory, &own).unwrap();
        assert_eq!(write(&directory, &own), Err("supervisor_record_exists"));
        let stored = read(&directory).unwrap().unwrap();
        assert_eq!(stored.liveness(), Liveness::Alive);
        clear(&directory).unwrap();
        assert_eq!(read(&directory).unwrap(), None);

        let intent = LaunchIntent {
            version: 1,
            launch_identity: own.launch_identity.clone(),
        };
        write_intent(&directory, &intent).unwrap();
        assert_eq!(read_intent(&directory).unwrap(), Some(intent.clone()));
        assert_eq!(
            write_intent(&directory, &intent),
            Err("supervisor_record_exists")
        );
        clear_intent(&directory).unwrap();
        assert_eq!(read_intent(&directory).unwrap(), None);
    }

    #[test]
    fn reused_pid_with_different_start_time_is_gone_and_corrupt_records_refuse() {
        let (_temp, directory) = directory();
        let pid = std::process::id();
        let start = native::process_start(pid).unwrap();
        assert_eq!(record(pid, start ^ 1).liveness(), Liveness::Gone);
        // A leftover unpublished staging file is ignored and replaced.
        directory
            .create_new(&name(GENERATION_STAGED).unwrap())
            .unwrap()
            .write_all(b"{partial")
            .unwrap();
        write(&directory, &record(pid, start)).unwrap();
        assert!(read(&directory).unwrap().is_some());
        clear(&directory).unwrap();
        directory
            .create_new(&name(GENERATION).unwrap())
            .unwrap()
            .write_all(b"{partial")
            .unwrap();
        assert_eq!(read(&directory), Err("supervisor_record_invalid"));
    }
}
