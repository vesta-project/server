//! Recoverable prune moves in the artifact directory, never copy/delete or replace.
//!
//! `q-<operation>-<original>` preserves the complete original name. The 16-hex operation
//! is generated under the DB writer and checked against both prior audits and leftovers.
//! A committed success audit with that operation permits completion, except that the
//! hosted name is always restored. Without that audit every candidate (including an
//! abandoned stage or unreferenced publication) is restored: history rows alone cannot
//! distinguish a historical artifact deliberately pruned from an interrupted prune.
//! An occupied restore target is never overwritten or assumed equivalent. Recovery
//! refuses explicitly, leaving the quarantined bytes for operator recovery.
use super::{PackArtifacts, PackError, PruneReport, PublishedArtifact, owned_name, state_error};
use crate::packs::ReleaseError;
use rusqlite::{Connection, params};
use std::path::Path;
use vesta_state::artifacts::ArtifactName;

pub(crate) struct Quarantined {
    pub(super) operation: String,
    original: ArtifactName,
    quarantine: ArtifactName,
}

pub(crate) fn recovery_required() -> ReleaseError {
    ReleaseError::RecoveryRequired
}

impl PackArtifacts {
    pub(crate) fn prune_operation(&self, connection: &Connection) -> Result<String, ReleaseError> {
        let leftovers = self.quarantines()?;
        loop {
            let mut random = [0_u8; 8];
            getrandom::fill(&mut random).map_err(|_| PackError::Unavailable)?;
            let operation = hex::encode(random);
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM audit_events WHERE operation_id=?)",
                [&operation],
                |row| row.get(0),
            )?;
            if !exists && !leftovers.iter().any(|entry| entry.operation == operation) {
                return Ok(operation);
            }
        }
    }

    /// `moved` is owned by the caller even if a later move/sync/audit fails. Never hide
    /// a partially completed batch in an error or rely on best-effort Drop restoration.
    pub(crate) fn quarantine_except(
        &self,
        keep: Option<&PublishedArtifact>,
        operation: &str,
        connection: &Connection,
        moved: &mut Vec<Quarantined>,
    ) -> Result<PruneReport, ReleaseError> {
        let keep = keep.map(PublishedArtifact::name).transpose()?;
        let mut report = PruneReport::default();
        for original in self
            .directory
            .list(super::MAX_ARTIFACT_ENTRIES)
            .map_err(state_error)?
        {
            if Some(&original) == keep.as_ref()
                || delivery_retained(connection, original.as_str())?
                || !owned_name(original.as_str())
            {
                continue;
            }
            let quarantine = ArtifactName::new(&format!("q-{operation}-{}", original.as_str()))
                .map_err(state_error)?;
            // open rejects hard links, junctions, symlinks and special files. Close
            // before moving on Windows. Failed candidates remain at their old name.
            match self.directory.open(&original) {
                Ok(Some(file)) => drop(file),
                Ok(None) => continue,
                Err(_) => {
                    report.retained += 1;
                    continue;
                }
            }
            if rename_absent(
                &self.path.join(original.as_str()),
                &self.path.join(quarantine.as_str()),
            )
            .is_err()
            {
                report.retained += 1;
                continue;
            }
            moved.push(Quarantined {
                operation: operation.to_owned(),
                original,
                quarantine,
            });
            // Ownership is recorded before syncing: a sync failure still rolls back
            // this successful rename, and a crash leaves a self-describing name.
            self.directory.sync().map_err(|_| recovery_required())?;
            report.removed += 1;
        }
        Ok(report)
    }

    fn quarantines(&self) -> Result<Vec<Quarantined>, ReleaseError> {
        // Normal publication bounds entries. Refuse rather than silently omit recovery
        // if external files have grown the directory beyond the bounded scan.
        if self
            .directory
            .count(super::MAX_ARTIFACT_ENTRIES)
            .map_err(|_| recovery_required())?
            .is_none()
        {
            return Err(recovery_required());
        }
        let mut entries = Vec::new();
        for quarantine in self
            .directory
            .list(super::MAX_ARTIFACT_ENTRIES)
            .map_err(|_| recovery_required())?
        {
            let Some((operation, original)) = quarantine
                .as_str()
                .strip_prefix("q-")
                .and_then(|rest| rest.split_once('-'))
            else {
                continue;
            };
            if operation.len() != 16
                || !operation
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || !owned_name(original)
            {
                continue;
            }
            entries.push(Quarantined {
                operation: operation.to_owned(),
                original: ArtifactName::new(original).map_err(|_| recovery_required())?,
                quarantine,
            });
        }
        Ok(entries)
    }

    pub(crate) fn restore_quarantines(&self, entries: &[Quarantined]) -> Result<(), ReleaseError> {
        self.resolve_quarantines(entries, false)
    }

    pub(crate) fn complete_quarantines(&self, entries: &[Quarantined]) -> Result<(), ReleaseError> {
        self.resolve_quarantines(entries, true)
    }

    fn resolve_quarantines(
        &self,
        entries: &[Quarantined],
        complete: bool,
    ) -> Result<(), ReleaseError> {
        let mut failed = false;
        for entry in entries {
            let result = (|| {
                // Each primitive rechecks directory confinement; same-account ancestor
                // races remain the same limitation as publication, not a sandbox.
                let Some(file) = self
                    .directory
                    .open(&entry.quarantine)
                    .map_err(|_| recovery_required())?
                else {
                    // Another serialized recovery may already have restored it. If
                    // neither name exists, restoration is not proven and must refuse.
                    if !complete
                        && self
                            .directory
                            .open(&entry.original)
                            .map_err(|_| recovery_required())?
                            .is_none()
                    {
                        return Err(recovery_required());
                    }
                    return Ok(());
                };
                drop(file);
                if complete {
                    self.directory
                        .remove(&entry.quarantine)
                        .map_err(|_| recovery_required())?;
                } else {
                    rename_absent(
                        &self.path.join(entry.quarantine.as_str()),
                        &self.path.join(entry.original.as_str()),
                    )
                    .map_err(|_| recovery_required())?;
                    self.directory.sync().map_err(|_| recovery_required())?;
                }
                Ok::<_, ReleaseError>(())
            })();
            failed |= result.is_err();
        }
        if failed {
            Err(recovery_required())
        } else {
            Ok(())
        }
    }

    /// Must run under the DB writer, so no in-flight prune can be mistaken for a crash.
    pub(crate) fn recover_prunes(
        &self,
        connection: &Connection,
        server: &str,
        keep: Option<&PublishedArtifact>,
    ) -> Result<(), ReleaseError> {
        let keep = keep.map(PublishedArtifact::name).transpose()?;
        for entry in self.quarantines()? {
            let committed: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM audit_events WHERE server_id=? AND source='pack-release' AND action='pack_release.prune' AND target='pack-release' AND outcome='success' AND operation_id=?)",
                params![server, entry.operation],
                |row| row.get(0),
            )?;
            self.resolve_quarantines(
                std::slice::from_ref(&entry),
                committed
                    && Some(&entry.original) != keep.as_ref()
                    && !delivery_retained(connection, entry.original.as_str())?,
            )?;
        }
        Ok(())
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn rename_absent(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let from: Vec<u16> = from.as_os_str().encode_wide().chain([0]).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: both stable, NUL-terminated UTF-16 buffers live for the synchronous call.
    // No REPLACE_EXISTING or COPY_ALLOWED: atomic same-volume rename or refusal.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn rename_absent(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: stable NUL-terminated absolute paths; no-replace fails closed on
    // unsupported filesystems/kernels rather than falling back to replacement.
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn rename_absent(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

fn delivery_retained(c: &Connection, name: &str) -> Result<bool, ReleaseError> {
    let exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='player_pack_offering')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(false);
    }
    Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM player_pack_offering WHERE json_extract(body,'$.sha256') || '.' || json_extract(body,'$.format')=?)",[name],|r|r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, sync::Arc};
    use vesta_state::{Clock, StateDatabase, StateDirectory};

    struct FixedClock;
    impl Clock for FixedClock {
        fn millis(&self) -> i64 {
            1_791_115_200_000
        }
    }

    #[test]
    fn an_owned_failed_restore_is_reported_and_keeps_both_names_intact() {
        let temp = tempfile::tempdir().unwrap();
        let directory = StateDirectory::new(temp.path().join("state"), None).unwrap();
        let db = StateDatabase::open(directory, Arc::new(FixedClock)).unwrap();
        let store = PackArtifacts::open(&db).unwrap();
        let original = ArtifactName::new(&format!("{}.zip", "ab".repeat(32))).unwrap();
        let mut file = store.directory.create_new(&original).unwrap();
        file.write_all(b"owned bytes").unwrap();
        drop(file);
        let mut moved = Vec::new();
        db.write_with(|t| {
            store.quarantine_except(None, "0123456789abcdef", t.connection(), &mut moved)
        })
        .unwrap();
        let mut replacement = store.directory.create_new(&original).unwrap();
        replacement.write_all(b"replacement").unwrap();
        drop(replacement);
        assert_eq!(
            store.restore_quarantines(&moved).unwrap_err(),
            ReleaseError::RecoveryRequired
        );
        assert_eq!(
            std::fs::read(store.path.join(moved[0].quarantine.as_str())).unwrap(),
            b"owned bytes"
        );
        assert_eq!(
            std::fs::read(store.path.join(original.as_str())).unwrap(),
            b"replacement"
        );
        store.directory.remove(&original).unwrap();
        store.restore_quarantines(&moved).unwrap();
        assert_eq!(
            std::fs::read(store.path.join(original.as_str())).unwrap(),
            b"owned bytes"
        );
    }

    #[cfg(windows)]
    #[test]
    fn failed_post_commit_completion_leaves_quarantine_for_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let directory = StateDirectory::new(temp.path().join("state"), None).unwrap();
        let db = StateDatabase::open(directory, Arc::new(FixedClock)).unwrap();
        let store = PackArtifacts::open(&db).unwrap();
        let original = ArtifactName::new(&format!("{}.zip", "cd".repeat(32))).unwrap();
        let mut file = store.directory.create_new(&original).unwrap();
        file.write_all(b"owned bytes").unwrap();
        drop(file);
        let mut moved = Vec::new();
        db.write_with(|t| {
            store.quarantine_except(None, "0123456789abcdef", t.connection(), &mut moved)
        })
        .unwrap();
        let open = store.directory.open(&moved[0].quarantine).unwrap().unwrap();
        assert_eq!(
            store.complete_quarantines(&moved).unwrap_err(),
            ReleaseError::RecoveryRequired
        );
        assert!(store.path.join(moved[0].quarantine.as_str()).exists());
        drop(open);
        store.complete_quarantines(&moved).unwrap();
        assert!(!store.path.join(moved[0].quarantine.as_str()).exists());
    }
}
