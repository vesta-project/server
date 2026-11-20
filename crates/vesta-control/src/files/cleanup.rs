use super::mutation::{mutation_admission, state_io, trash_name};
use super::{FileBrowser, FileError, RelativePath, Result, io_error};
use rusqlite::params;
use std::{fs, time::SystemTime};

pub enum CleanupFiles {
    Logs { cutoff: SystemTime },
    Trash,
}

impl FileBrowser {
    /// Acquire the same lease as file mutations and hold a writer transaction
    /// across the observation and effect so a job cannot be admitted mid-cleanup.
    pub fn cleanup_files(
        &self,
        selection: CleanupFiles,
        dry_run: bool,
        ready: impl FnOnce() -> bool,
    ) -> Result<Option<u64>> {
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = self.mutation_guard(&admission)?;
        self.db.write_with(|tx| {
            let c = tx.connection();
            let busy: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM operations WHERE state NOT IN ('succeeded','failed','cancelled'))
                    OR EXISTS(SELECT 1 FROM backup_jobs WHERE state IN ('awaiting_confirmation','queued','running','recovery_required','uncertain'))
                    OR EXISTS(SELECT 1 FROM software_jobs WHERE state IN ('queued','staging','activating','recovery_required'))
                    OR EXISTS(SELECT 1 FROM workflow_runs WHERE state IN ('queued','running','suspended'))",
                [], |row| row.get(0),
            )?;
            if busy || !ready() {
                return Ok(None);
            }
            let bytes = match selection {
                CleanupFiles::Logs { cutoff } => self.cleanup_logs(cutoff, dry_run)?,
                CleanupFiles::Trash => {
                    let trash = self.trash_dir()?;
                    let mut query = c.prepare(
                        "SELECT id FROM file_trash WHERE server_id=? AND status IN ('complete','restored')
                        AND NOT EXISTS (SELECT 1 FROM file_intents WHERE status IN ('pending','recovery-required')
                            AND json_extract(details_json,'$.trash')=file_trash.id) ORDER BY id"
                    )?;
                    let ids = query.query_map([self.db.server_id()], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    let mut bytes = 0_u64;
                    for id in ids {
                        let name = trash_name(&id)?;
                        let len = trash.open(&name).map_err(state_io)?.map(|file| {
                            file.metadata().map(|m| m.len()).map_err(io_error)
                        }).transpose()?.unwrap_or(0);
                        if !dry_run {
                            trash.remove(&name).map_err(state_io)?;
                            c.execute("DELETE FROM file_trash WHERE server_id=? AND id=?", params![self.db.server_id(), id])?;
                        }
                        bytes = bytes.saturating_add(len);
                    }
                    bytes
                }
            };
            Ok::<_, FileError>(Some(bytes))
        }).map_err(FileError::from)
    }

    fn cleanup_logs(&self, cutoff: SystemTime, dry_run: bool) -> Result<u64> {
        let logs = RelativePath::parse("logs")?;
        let path = match self.root.resolve(Some(&logs)) {
            Ok(path) => path,
            Err(FileError::NotFound) => return Ok(0),
            Err(error) => return Err(error),
        };
        #[cfg(target_os = "linux")]
        crate::machine::removed::refuse_nested_mounts(&path.path)
            .map_err(|_| FileError::ProtectedPath)?;
        let mut bytes = 0_u64;
        for entry in fs::read_dir(path.path).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let name = entry.file_name();
            let Some(name) = name.to_str().filter(|name| rotated_log_name(name)) else {
                continue;
            };
            let relative = RelativePath::parse(&format!("logs/{name}"))?;
            let Ok(file) = self.root.resolve(Some(&relative)) else {
                continue;
            };
            if !file.metadata.is_file() || !file.metadata.modified().is_ok_and(|at| at < cutoff) {
                continue;
            }
            if !dry_run && fs::remove_file(file.path).is_err() {
                continue;
            }
            bytes = bytes.saturating_add(file.metadata.len());
        }
        Ok(bytes)
    }
}

/// Minecraft's default rotated Log4j archive name.
pub(super) fn rotated_log_name(name: &str) -> bool {
    let Some((date, index)) = name
        .strip_suffix(".log.gz")
        .and_then(|name| name.rsplit_once('-'))
    else {
        return false;
    };
    !index.is_empty()
        && index.bytes().all(|byte| byte.is_ascii_digit())
        && chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok()
}
