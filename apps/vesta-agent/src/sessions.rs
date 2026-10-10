//! Conservative orphan guard for managed instances, not a P3 supervisor.
//! An unclean session is never inferred stopped merely because OS locks vanished.
use crate::local_files::{self, Result};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
const PENDING: &str = "operator-session-pending";
const MARKER: &[u8] = b"vesta-owned-agent-session-v1: shutdown-not-proven\n";
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
}
impl Sessions {
    /// Caller must retain all matching external instance leases before this call.
    pub fn begin(directories: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        let directories = directories.into_iter().collect::<BTreeSet<_>>();
        for directory in &directories {
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
            local_files::write_new(&directory.join(PENDING), MARKER)?;
            instances.push(directory);
        }
        Ok(Self { instances })
    }
    /// Called ONLY after actual runtime+I/O completion, or Host::open refused
    /// before owning any runtime. No Drop cleanup: unwind/crash retains evidence.
    pub fn stopped(self) -> Result<()> {
        for directory in self.instances {
            let pending = directory.join(PENDING);
            if local_files::read(&pending, MARKER.len())? != MARKER {
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
            // Archive even operator markers instead of erasing/replacing bytes.
            fs::rename(pending, closed).map_err(|_| "local_session_unavailable")?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unfinished_session_survives_drop_and_refuses_restart_or_delete_proof() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().to_owned();
        let session = Sessions::begin([directory.clone()]).unwrap();
        drop(session);
        assert_eq!(
            require_stopped(&directory),
            Err("local_shutdown_unproven_requires_supervisor_recovery")
        );
        assert!(Sessions::begin([directory.clone()]).is_err());
        assert_eq!(fs::read(directory.join(PENDING)).unwrap(), MARKER);
    }
    #[test]
    fn proven_stop_archives_marker_without_erasure() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().to_owned();
        Sessions::begin([directory.clone(), directory.clone()])
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
