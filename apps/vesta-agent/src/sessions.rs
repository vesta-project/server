//! Conservative orphan guard for managed instances.
//! An unclean in-process session is never inferred stopped merely because OS locks
//! vanished. In separate-supervision mode the agent never owns the game, so an
//! unfinished supervised marker is recoverable; the supervisor keeps continuity.
use crate::local_files::{self, Result};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
const PENDING: &str = "operator-session-pending";
const MARKER: &[u8] = b"vesta-owned-agent-session-v1: shutdown-not-proven\n";
const SUPERVISED_MARKER: &[u8] = b"vesta-owned-agent-session-v1: supervised-agent-only\n";
pub fn require_stopped(directory: &Path) -> Result<()> {
    let pending = directory.join(PENDING);
    local_files::guard(&pending)?;
    match fs::symlink_metadata(pending) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err("local_shutdown_unproven_requires_supervisor_recovery"),
    }
}
pub struct Sessions {
    instances: Vec<PathBuf>,
    marker: &'static [u8],
}
impl Sessions {
    /// Caller must retain all matching external instance leases before this call.
    /// With `supervised`, an earlier supervised agent's unfinished marker is archived;
    /// any other unfinished marker still refuses.
    pub fn begin(directories: impl IntoIterator<Item = PathBuf>, supervised: bool) -> Result<Self> {
        let directories = directories.into_iter().collect::<BTreeSet<_>>();
        let marker = if supervised {
            SUPERVISED_MARKER
        } else {
            MARKER
        };
        for directory in &directories {
            if supervised
                && require_stopped(directory).is_err()
                && local_files::read(&directory.join(PENDING), SUPERVISED_MARKER.len())
                    .is_ok_and(|bytes| bytes == SUPERVISED_MARKER)
            {
                archive(directory, SUPERVISED_MARKER)?;
            }
            require_stopped(directory)?;
        }
        let mut instances = Vec::new();
        for directory in directories {
            let history = directory.join("operator-sessions");
            local_files::guard(&history)?;
            match fs::create_dir(&history) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && history.is_dir() => {}
                Err(_) => return Err("local_session_unavailable"),
            }
            // Persist before any runtime owner/queued game effect can be opened.
            local_files::write_new(&directory.join(PENDING), marker)?;
            instances.push(directory);
        }
        Ok(Self { instances, marker })
    }
    /// Called ONLY after actual runtime+I/O completion, or Host::open refused
    /// before owning any runtime. No Drop cleanup: unwind/crash retains evidence.
    pub fn stopped(self) -> Result<()> {
        for directory in self.instances {
            archive(&directory, self.marker)?;
        }
        Ok(())
    }
}
/// Archive an exact pending marker instead of erasing or replacing its bytes.
fn archive(directory: &Path, marker: &[u8]) -> Result<()> {
    let pending = directory.join(PENDING);
    if local_files::read(&pending, marker.len())? != marker {
        return Err("local_session_changed");
    }
    let closed = directory
        .join("operator-sessions")
        .join(uuid::Uuid::new_v4().to_string());
    local_files::guard(&closed)?;
    match fs::symlink_metadata(&closed) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        _ => return Err("local_session_unavailable"),
    }
    fs::rename(pending, closed).map_err(|_| "local_session_unavailable")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supervised_marker_is_recoverable_but_in_process_marker_is_not() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().to_owned();
        // A crashed supervised agent never owned the game: a new supervised agent recovers.
        drop(Sessions::begin([directory.clone()], true).unwrap());
        assert!(require_stopped(&directory).is_err());
        assert!(Sessions::begin([directory.clone()], false).is_err());
        Sessions::begin([directory.clone()], true)
            .unwrap()
            .stopped()
            .unwrap();
        // A crashed in-process agent may have orphaned its game: supervision cannot recover.
        drop(Sessions::begin([directory.clone()], false).unwrap());
        assert!(Sessions::begin([directory.clone()], true).is_err());
        assert_eq!(fs::read(directory.join(PENDING)).unwrap(), MARKER);
    }
    #[test]
    fn unfinished_session_survives_drop_and_refuses_restart_or_delete_proof() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().to_owned();
        let session = Sessions::begin([directory.clone()], false).unwrap();
        drop(session);
        assert_eq!(
            require_stopped(&directory),
            Err("local_shutdown_unproven_requires_supervisor_recovery")
        );
        assert!(Sessions::begin([directory.clone()], false).is_err());
        assert_eq!(fs::read(directory.join(PENDING)).unwrap(), MARKER);
    }
    #[test]
    fn proven_stop_archives_marker_without_erasure() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().to_owned();
        Sessions::begin([directory.clone(), directory.clone()], false)
            .unwrap()
            .stopped()
            .unwrap();
        require_stopped(&directory).unwrap();
        let archived: Vec<_> = fs::read_dir(directory.join("operator-sessions"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(archived.len(), 1);
        assert_eq!(fs::read(&archived[0]).unwrap(), MARKER);
    }
}
