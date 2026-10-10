//! Protected single-level directory creation and optimistically concurrent file mutations.
//!
//! Callers name the bytes they expect to replace (`If-Match`) or require absence, so a
//! stale edit refuses with the current hash instead of overwriting newer content. The
//! previous bytes are copied into the bounded owner-only trash and recorded before the
//! game file changes. New bytes are staged as a hidden create-new file beside the target
//! and moved into place in the same directory. Pending provenance commits before the
//! destructive effect. Authority and the precondition are rechecked in the completion
//! transaction together with the success audit. Failed completion is compensated only
//! while the effect's hash still matches. Explicit reconciliation retains uncertain data.
//! Every mutation commits an intent before its effect; startup reports recovered
//! success, aborted unchanged intent, or explicit recovery-required ambiguity.
//! Path checks are same-account hygiene, not race-free confinement.
use super::{FileBrowser, FileError, RelativePath, Result, io_error, native};
use crate::identity::{self, Capability, Subject};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    collections::HashMap,
    fs,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};
use vesta_state::{
    artifacts::{ArtifactDirectory, ArtifactName},
    audit::{AuditActor, AuditRecord},
};

pub const MAX_WRITE_BYTES: usize = 16 * 1_024 * 1_024;
/// Larger existing files cannot be made recoverable, so overwriting/deleting them refuses.
pub const MAX_TRASH_FILE_BYTES: u64 = 256 * 1_024 * 1_024;
/// Free space a write must leave on the target volume.
pub const SPACE_RESERVE_BYTES: u64 = 64 * 1_024 * 1_024;
/// Per-server durable reservations, including cleanup-blocked terminal intents.
pub const MAX_FILE_INTENTS: usize = 1_024;
/// Aggregate game-tree stage/compensation reservations, independent of trash/uploads.
pub const MAX_INTENT_STAGED_BYTES: u64 = 1_024 * 1_024 * 1_024;
const RECONCILE_BATCH_SIZE: usize = 64;
const MAX_RECONCILE_BATCHES: usize = 64;
const SOURCE: &str = "files";
const TARGET: &str = "file";
const STAGE_PREFIX: &str = ".vesta-stage-";
#[path = "journal.rs"]
mod journal;
use journal::{Intent, crash_point, interrupted};

/// Trash ceiling: the lower of an absolute byte limit and a share of the state volume.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrashPolicy {
    max_bytes: u64,
    max_volume_percent: u8,
    retention: Duration,
    max_entries: usize,
}
impl TrashPolicy {
    pub fn new(
        max_bytes: u64,
        max_volume_percent: u8,
        retention: Duration,
        max_entries: usize,
    ) -> Result<Self> {
        if max_bytes == 0
            || !(1..=100).contains(&max_volume_percent)
            || retention.is_zero()
            || i64::try_from(retention.as_millis()).is_err()
            || max_entries == 0
            || max_entries > 100_000
        {
            return Err(FileError::InvalidLimit);
        }
        Ok(Self {
            max_bytes,
            max_volume_percent,
            retention,
            max_entries,
        })
    }
}
impl Default for TrashPolicy {
    /// 10 GiB, 10 % of the volume and seven days, as the requirements recommend.
    fn default() -> Self {
        Self {
            max_bytes: 10 * 1_024 * 1_024 * 1_024,
            max_volume_percent: 10,
            retention: Duration::from_secs(7 * 24 * 60 * 60),
            max_entries: 10_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WritePrecondition {
    /// Create only; refuse if anything exists at the path.
    Absent,
    /// Replace only the file whose current SHA-256 (lowercase hex) is this.
    Matches(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileChange {
    pub sha256: String,
    pub size: u64,
    /// The recoverable copy of the replaced bytes, when something was replaced.
    pub trash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrashEntry {
    pub id: String,
    pub path: RelativePath,
    pub sha256: String,
    pub size: u64,
    pub trashed_at: i64,
    pub expires_at: i64,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Reason {
    Replace,
    Delete,
}
impl Reason {
    const fn wire(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Delete => "delete",
        }
    }
}

impl FileBrowser {
    /// Creates exactly one directory level; the existing parent must be authorized.
    pub fn mkdir(&self, subject: &Subject, path: &RelativePath) -> Result<()> {
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = admission.lock().map_err(|_| FileError::Unavailable)?;
        let intent = self.begin_intent(subject, "mkdir", path, &Intent::default())?;
        let mut target = None;
        let result = self
            .db
            .write_with(|transaction| {
                let connection = transaction.connection();
                self.authorize_destination(connection, subject, path, Capability::FilesMkdir)?;
                let directory = self.parent(path)?.join(leaf(path));
                require_absent(&directory)?;
                self.authorize_destination(connection, subject, path, Capability::FilesMkdir)?;
                crash_point("before")?;
                fs::create_dir(&directory).map_err(no_replace_error)?;
                target = Some(directory);
                crash_point("after")?;
                self.finish_intent(connection, &intent)?;
                Ok(())
            })
            .map_err(FileError::from);
        if interrupted() {
            return result;
        }
        if let Err(error) = &result
            && target.is_some()
        {
            // Never recursively remove: an intervening child's data must be retained.
            let reversed = (|| {
                let checked = self.root.resolve(Some(path))?;
                if Some(&checked.path) != target.as_ref() || !checked.metadata.is_dir() {
                    return Err(FileError::LinkedPath);
                }
                fs::remove_dir(&checked.path).map_err(io_error)
            })();
            if reversed.is_err() {
                let failed: Result<()> = Err(FileError::RecoveryRequired(Box::new(error.clone())));
                self.failed_intent(&intent, &failed)?;
                return failed;
            }
        }
        self.failed_intent(&intent, &result)?;
        result
    }

    /// Atomically moves one regular file, never replacing a destination. Both names
    /// require rename authority, and the source must still have the expected bytes.
    /// Unlike a hard-link/delete move, the native operation has no two-name crash window.
    pub fn rename(
        &self,
        subject: &Subject,
        from: &RelativePath,
        to: &RelativePath,
        expected_sha256: &str,
    ) -> Result<FileChange> {
        check_hex(expected_sha256)?;
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = admission.lock().map_err(|_| FileError::Unavailable)?;
        let intent = self.begin_intent(
            subject,
            "rename",
            from,
            &Intent {
                to: Some(to.as_str().into()),
                previous: Some(expected_sha256.into()),
                effect: Some(expected_sha256.into()),
                ..Intent::default()
            },
        )?;
        let mut targets = None;
        let result = self
            .db
            .write_with(|transaction| {
                let connection = transaction.connection();
                self.authorize(connection, subject, from, Capability::FilesRename)?;
                self.authorize_destination(connection, subject, to, Capability::FilesRename)?;
                let checked = self.root.resolve(Some(from))?;
                let source = checked.path;
                let destination = self.parent(to)?.join(leaf(to));
                require_hash(&source, Some(expected_sha256))?;
                require_mutable_target(&source)?;
                require_absent(&destination)?;
                let size = checked.metadata.len();
                self.authorize(connection, subject, from, Capability::FilesRename)?;
                self.authorize_destination(connection, subject, to, Capability::FilesRename)?;
                crash_point("before")?;
                native::rename_absent(&source, &destination)?;
                targets = Some((source, destination));
                crash_point("after")?;
                self.finish_intent(connection, &intent)?;
                Ok(FileChange {
                    sha256: expected_sha256.to_owned(),
                    size,
                    trash: None,
                })
            })
            .map_err(FileError::from);
        if interrupted() {
            return result;
        }
        let result = if let Some((source, destination)) = targets {
            settle_rename(result, &source, &destination, expected_sha256)
        } else {
            result
        };
        self.failed_intent(&intent, &result)?;
        result
    }

    pub fn write(
        &self,
        subject: &Subject,
        path: &RelativePath,
        precondition: &WritePrecondition,
        bytes: &[u8],
    ) -> Result<FileChange> {
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = admission.lock().map_err(|_| FileError::Unavailable)?;
        self.write_locked(subject, path, precondition, bytes, None)
    }

    /// Upload retirement and the success audit commit together. Caller holds admission.
    pub(super) fn write_locked(
        &self,
        subject: &Subject,
        path: &RelativePath,
        precondition: &WritePrecondition,
        bytes: &[u8],
        upload: Option<&str>,
    ) -> Result<FileChange> {
        if bytes.len() > MAX_WRITE_BYTES {
            return Err(FileError::TooLarge);
        }
        if let WritePrecondition::Matches(expected) = precondition {
            check_hex(expected)?;
        }
        let capability = match precondition {
            WritePrecondition::Absent => Capability::FilesCreate,
            WritePrecondition::Matches(_) => Capability::FilesOverwrite,
        };
        let sha256 = hex::encode(Sha256::digest(bytes));
        let parent = self.authorized_parent(subject, path, capability)?;
        let target = parent.join(leaf(path));
        let incoming = match precondition {
            WritePrecondition::Absent => 0,
            WritePrecondition::Matches(_) => fs::metadata(&target).map_err(io_error)?.len(),
        };
        self.require_mutation_space(&parent, bytes.len() as u64, incoming)?;
        let stage = Stage::planned(&parent)?;
        let trash = match precondition {
            WritePrecondition::Absent => None,
            WritePrecondition::Matches(_) => Some(random_id()?),
        };
        // Reserve count/bytes durably before creating any game stage or trash copy.
        let intent = self.begin_intent(
            subject,
            "write",
            path,
            &Intent {
                previous: match precondition {
                    WritePrecondition::Absent => None,
                    WritePrecondition::Matches(sha) => Some(sha.clone()),
                },
                effect: Some(sha256.clone()),
                stage: Some(stage.leaf()?),
                trash: trash.clone(),
                upload: upload.map(str::to_owned),
                size: bytes.len() as u64,
                staged_bytes: Some((bytes.len() as u64).max(incoming)),
                ..Intent::default()
            },
        )?;
        let prepared = (|| {
            if let (WritePrecondition::Matches(expected), Some(id)) = (precondition, &trash) {
                self.prepare_trash_with_id(
                    subject,
                    path,
                    expected,
                    Reason::Replace,
                    Some(&sha256),
                    id,
                )?;
            }
            stage.create(bytes)
        })();
        if let Err(error) = &prepared {
            // Creation owns even a partially written stage. Drop its private
            // handle/name first so cleanup does not mistake a failed preparation
            // for an uncertain native effect. Failed removal stays accounted.
            drop(stage);
            self.failed_intent(&intent, &prepared)?;
            if let Some(id) = &trash {
                let _ = self.retire_failed(id, &prepared);
            }
            return Err(error.clone());
        }
        stage.retained.set(true);
        let applied = Cell::new(None::<Applied>);
        let result = self.db.write_with(|transaction| {
            let connection = transaction.connection();
            require_hash(&stage.path, Some(&sha256))?;
            self.authorize(connection, subject, path, capability)?;
            match precondition {
                WritePrecondition::Absent => require_absent(&target)?,
                WritePrecondition::Matches(expected) => {
                    require_hash(&target, Some(expected))?;
                    require_mutable_target(&target)?;
                    stage.permissions(&permissions_for(
                        connection,
                        self.db.server_id(),
                        trash.as_deref().ok_or(FileError::Unavailable)?,
                    )?)?;
                }
            }
            self.authorize(connection, subject, path, capability)?;
            crash_point("before")?;
            match precondition {
                WritePrecondition::Absent => stage.link_absent(&target)?,
                WritePrecondition::Matches(_) => stage.replace(&target)?,
            }
            applied.set(Some(Applied {
                trash: trash.clone(),
                effect: Some(sha256.clone()),
            }));
            crash_point("after")?;
            self.finish_intent(connection, &intent)?;
            complete(connection, self.db.server_id(), trash.as_deref())?;
            if let Some(id) = upload {
                let removed = connection.execute(
                    "DELETE FROM file_uploads WHERE server_id=? AND id=? AND principal_id=? AND device_id=? AND expires_at>?",
                    params![self.db.server_id(), id, subject.principal.as_str(), subject.device.as_str(), self.db.clock().millis()],
                )?;
                if removed != 1 {
                    return Err(FileError::NotFound);
                }
            }
            Ok(FileChange {
                sha256: sha256.clone(),
                size: bytes.len() as u64,
                trash: trash.clone(),
            })
        });
        if interrupted() {
            return result.map_err(FileError::from);
        }
        let result = self.settle_with_intent(
            result.map_err(FileError::from),
            applied.take(),
            &target,
            Some(&intent),
        );
        if interrupted() {
            return result;
        }
        if let Err(primary) = &result
            && let Some(id) = trash.as_deref()
            && self.retire_failed(id, &result).is_err()
        {
            return Err(FileError::RecoveryRequired(Box::new(primary.clone())));
        }
        self.failed_intent(&intent, &result)?;
        stage
            .retained
            .set(matches!(&result, Err(FileError::RecoveryRequired(_))));
        result
    }

    /// Moves the file into the trash. The caller must name the bytes it expects to delete.
    pub fn delete(
        &self,
        subject: &Subject,
        path: &RelativePath,
        expected_sha256: &str,
    ) -> Result<TrashEntry> {
        check_hex(expected_sha256)?;
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = admission.lock().map_err(|_| FileError::Unavailable)?;
        let parent = self.authorized_parent(subject, path, Capability::FilesDelete)?;
        let target = parent.join(leaf(path));
        let id = random_id()?;
        let intent = self.begin_intent(
            subject,
            "delete",
            path,
            &Intent {
                previous: Some(expected_sha256.into()),
                trash: Some(id.clone()),
                staged_bytes: Some(fs::metadata(&target).map_err(io_error)?.len()),
                ..Intent::default()
            },
        )?;
        let prepared =
            self.prepare_trash_with_id(subject, path, expected_sha256, Reason::Delete, None, &id);
        if let Err(error) = &prepared {
            self.failed_intent(&intent, &prepared)?;
            return Err(error.clone());
        }
        let applied = Cell::new(None::<Applied>);
        let result = self.db.write_with(|transaction| {
            let connection = transaction.connection();
            self.authorize(connection, subject, path, Capability::FilesDelete)?;
            require_hash(&target, Some(expected_sha256))?;
            require_mutable_target(&target)?;
            // Read-only and sharing-denied Windows targets refuse without changing attributes.
            self.authorize(connection, subject, path, Capability::FilesDelete)?;
            crash_point("before")?;
            fs::remove_file(&target).map_err(io_error)?;
            applied.set(Some(Applied {
                trash: Some(id.clone()),
                effect: None,
            }));
            crash_point("after")?;
            self.finish_intent(connection, &intent)?;
            complete(connection, self.db.server_id(), Some(&id))?;
            load_entry(connection, self.db.server_id(), &id)?.ok_or(FileError::Unavailable)
        });
        if interrupted() {
            return result.map_err(FileError::from);
        }
        let result = self.settle_with_intent(
            result.map_err(FileError::from),
            applied.take(),
            &target,
            Some(&intent),
        );
        if interrupted() {
            return result;
        }
        self.failed_intent(&intent, &result)?;
        if let Err(primary) = &result
            && self.retire_failed(&id, &result).is_err()
        {
            return Err(FileError::RecoveryRequired(Box::new(primary.clone())));
        }
        result
    }

    /// Puts a trashed copy back at its original path, which must now be absent.
    pub fn restore(&self, subject: &Subject, id: &str) -> Result<FileChange> {
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = admission.lock().map_err(|_| FileError::Unavailable)?;
        let name = trash_name(id)?;
        let entry = self
            .db
            .read_with(|snapshot| load_entry(snapshot.connection(), self.db.server_id(), id))
            .map_err(FileError::from)?
            .ok_or(FileError::NotFound)?;
        let parent = self.authorized_parent(subject, &entry.path, Capability::FilesCreate)?;
        require_space(&parent, entry.size)?;
        let trash = self.trash_dir()?;
        let bytes = read_trash(&trash, &name, &entry)?;
        let stage = Stage::planned(&parent)?;
        let permissions = self
            .db
            .read_with(|snapshot| {
                let connection = snapshot.connection();
                let status: String = connection.query_row(
                    "SELECT status FROM file_trash WHERE server_id=? AND id=?",
                    params![self.db.server_id(), id],
                    |row| row.get(0),
                )?;
                if status != "complete" {
                    return Err(FileError::RecoveryRequired(Box::new(
                        FileError::Unavailable,
                    )));
                }
                permissions_for(connection, self.db.server_id(), id)
            })
            .map_err(FileError::from)?;
        let target = parent.join(leaf(&entry.path));
        let intent = self.begin_intent(
            subject,
            "restore",
            &entry.path,
            &Intent {
                effect: Some(entry.sha256.clone()),
                stage: Some(stage.leaf()?),
                trash: Some(id.into()),
                size: entry.size,
                ..Intent::default()
            },
        )?;
        let prepared = stage
            .create(&bytes)
            .and_then(|()| stage.permissions(&permissions));
        if let Err(error) = &prepared {
            drop(stage);
            self.failed_intent(&intent, &prepared)?;
            return Err(error.clone());
        }
        stage.retained.set(true);
        let applied = Cell::new(None::<Applied>);
        let result = self.db.write_with(|transaction| {
            let connection = transaction.connection();
            require_hash(&stage.path, Some(&entry.sha256))?;
            self.authorize(connection, subject, &entry.path, Capability::FilesCreate)?;
            let current = load_entry(connection, self.db.server_id(), id)?;
            if current.as_ref() != Some(&entry) {
                return Err(FileError::NotFound);
            }
            let status: String = connection.query_row(
                "SELECT status FROM file_trash WHERE server_id=? AND id=?",
                params![self.db.server_id(), id],
                |row| row.get(0),
            )?;
            if status != "complete" {
                return Err(FileError::RecoveryRequired(Box::new(
                    FileError::Unavailable,
                )));
            }
            require_absent(&target)?;
            self.authorize(connection, subject, &entry.path, Capability::FilesCreate)?;
            crash_point("before")?;
            stage.link_absent(&target)?;
            applied.set(Some(Applied {
                trash: None,
                effect: Some(entry.sha256.clone()),
            }));
            crash_point("after")?;
            self.finish_intent(connection, &intent)?;
            connection.execute(
                "UPDATE file_trash SET status='restored' WHERE server_id=? AND id=?",
                params![self.db.server_id(), id],
            )?;
            Ok(FileChange {
                sha256: entry.sha256.clone(),
                size: entry.size,
                trash: None,
            })
        });
        if interrupted() {
            return result.map_err(FileError::from);
        }
        let result = self.settle_with_intent(
            result.map_err(FileError::from),
            applied.take(),
            &target,
            Some(&intent),
        );
        if interrupted() {
            return result;
        }
        self.failed_intent(&intent, &result)?;
        stage
            .retained
            .set(matches!(&result, Err(FileError::RecoveryRequired(_))));
        let change = result?;
        // Restore already committed successfully. Cleanup failure must not misreport
        // it as failed; retain its accounted restored row for a later purge retry.
        let _ = self.remove_recorded_copy(id, &trash, &name);
        Ok(change)
    }

    /// Entries whose original path the subject may currently read.
    pub fn trash_entries(&self, subject: &Subject) -> Result<Vec<TrashEntry>> {
        self.db
            .read_with(|snapshot| {
                let connection = snapshot.connection();
                let policy = identity::policy_in(
                    connection,
                    self.db.server_id(),
                    self.db.clock().millis(),
                    Some(subject),
                )?;
                let mut statement = connection.prepare(
                    "SELECT id FROM file_trash WHERE server_id=? AND status!='restored' ORDER BY trashed_at, id LIMIT 10001",
                )?;
                let ids = statement
                    .query_map([self.db.server_id()], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut entries = Vec::new();
                for id in ids {
                    let entry =
                        load_entry(connection, self.db.server_id(), &id)?.ok_or(FileError::Unavailable)?;
                    if policy.check(Capability::FilesRead, Some(&entry.path.resource()))
                        == identity::Decision::Allow
                    {
                        entries.push(entry);
                    }
                }
                Ok(entries)
            })
            .map_err(FileError::from)
    }

    /// Trusted maintenance. Recovery copies never expire automatically. Rows are
    /// retired only after copy removal succeeds (absent copies are also safe to retire).
    pub fn purge_expired_trash(&self) -> Result<usize> {
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = admission.lock().map_err(|_| FileError::Unavailable)?;
        let trash = self.trash_dir()?;
        let ids = self.db.read_with(|snapshot| {
            let mut statement = snapshot.connection().prepare(
                "SELECT id FROM file_trash WHERE server_id=? AND ((status='complete' AND expires_at<=?) OR status='restored') AND NOT EXISTS (SELECT 1 FROM file_intents WHERE status IN ('pending','recovery-required') AND json_extract(details_json,'$.trash')=file_trash.id)")?;
            let ids = statement.query_map(params![self.db.server_id(), self.db.clock().millis()], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, FileError>(ids)
        }).map_err(FileError::from)?;
        let mut removed = 0;
        for id in ids {
            if self
                .remove_recorded_copy(&id, &trash, &trash_name(&id)?)
                .is_ok()
            {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Trusted startup reconciliation, at most 64 batches of 64 rows per queue.
    /// Never replay an effect or remove uncertain data. Audited terminal intents
    /// are retired after owned-stage cleanup; recovery-required rows stay reserved.
    /// Call again if a legacy backlog exceeds the per-call work bound.
    pub fn reconcile_pending(&self) -> Result<usize> {
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = admission.lock().map_err(|_| FileError::Unavailable)?;
        let mut total = self.reconcile_intents()?;
        for _ in 0..MAX_RECONCILE_BATCHES {
            let count = self.db.write_with(|transaction| {
            let connection = transaction.connection();
            let mut statement = connection.prepare("SELECT id,path,effect_sha256,principal_id,device_id,reason FROM file_trash WHERE server_id=? AND status='pending' AND NOT EXISTS (SELECT 1 FROM file_intents WHERE json_extract(details_json,'$.trash')=file_trash.id) ORDER BY id LIMIT ?")?;
            let rows = statement.query_map(params![self.db.server_id(), RECONCILE_BATCH_SIZE as i64], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, Option<String>>(2)?,row.get::<_, String>(3)?,row.get::<_, String>(4)?,row.get::<_, String>(5)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (id, text, effect, principal, device, reason) in &rows {
                let path = RelativePath::parse(text)?;
                let entry = load_entry(connection, self.db.server_id(), id)?.ok_or(FileError::Unavailable)?;
                let copy_valid = read_trash(&self.trash_dir()?, &trash_name(id)?, &entry).is_ok();
                let target = self.parent(&path).map(|parent| parent.join(leaf(&path)));
                let known = copy_valid && target.as_ref().is_ok_and(|target| {
                    effect.as_ref() != Some(&entry.sha256) && require_hash(target, effect.as_deref()).is_ok()
                        && self.checked_existing_resource(&path).is_ok()
                });
                connection.execute("UPDATE file_trash SET status=? WHERE server_id=? AND id=?", params![if known { "complete" } else { "recovery-required" }, self.db.server_id(), id])?;
                if known {
                    let subject = Subject { principal: crate::identity::PrincipalId::parse(principal)?, device: crate::identity::DeviceId::parse(device)? };
                    self.audit(connection, &subject, if reason == "delete" { "files.delete" } else { "files.write" }, &path, [("trash", Some(id.clone())), ("reconciled", Some("true".into()))])?;
                }
            }
            Ok(rows.len())
        }).map_err(FileError::from)?;
            total += count;
            if count < RECONCILE_BATCH_SIZE {
                break;
            }
        }
        Ok(total)
    }

    fn remove_recorded_copy(
        &self,
        id: &str,
        trash: &ArtifactDirectory,
        name: &ArtifactName,
    ) -> Result<()> {
        trash.remove(name).map_err(state_io)?;
        self.db
            .write_with(|transaction| {
                transaction.connection().execute(
                    "DELETE FROM file_trash WHERE server_id=? AND id=?",
                    params![self.db.server_id(), id],
                )?;
                Ok::<_, FileError>(())
            })
            .map_err(FileError::from)
    }

    fn retire_failed<T>(&self, id: &str, result: &Result<T>) -> Result<()> {
        if matches!(result, Err(FileError::RecoveryRequired(_))) {
            self.db.write_with(|transaction| {
                transaction.connection().execute("UPDATE file_trash SET status='recovery-required' WHERE server_id=? AND id=?", params![self.db.server_id(), id])?;
                Ok::<_, FileError>(())
            }).map_err(FileError::from)
        } else {
            // A refused or successfully compensated operation no longer needs the
            // copy. Mark cleanup work before removal, so sharing-denied cleanup is
            // retried by purge rather than mistaken for an uncertain crash effect.
            self.db
                .write_with(|transaction| {
                    transaction.connection().execute(
                        "UPDATE file_trash SET status='restored' WHERE server_id=? AND id=?",
                        params![self.db.server_id(), id],
                    )?;
                    Ok::<_, FileError>(())
                })
                .map_err(FileError::from)?;
            self.remove_recorded_copy(id, &self.trash_dir()?, &trash_name(id)?)
        }
    }

    pub(super) fn authorized_parent(
        &self,
        subject: &Subject,
        path: &RelativePath,
        capability: Capability,
    ) -> Result<PathBuf> {
        self.db
            .read_with(|snapshot| {
                self.authorize(snapshot.connection(), subject, path, capability)?;
                self.parent(path)
            })
            .map_err(FileError::from)
    }

    /// The resolved canonical parent directory; the leaf name itself is rechecked too.
    fn parent(&self, path: &RelativePath) -> Result<PathBuf> {
        let parent = parent(path)?;
        let checked = self.root.resolve(parent.as_ref())?;
        if !checked.metadata.is_dir() {
            return Err(FileError::NotDirectory);
        }
        self.root.resource(&checked.path.join(leaf(path)))?;
        Ok(checked.path)
    }

    pub(super) fn authorize(
        &self,
        connection: &Connection,
        subject: &Subject,
        path: &RelativePath,
        capability: Capability,
    ) -> Result<i64> {
        self.authorize_path(connection, subject, path, capability, false)
    }

    fn authorize_destination(
        &self,
        connection: &Connection,
        subject: &Subject,
        path: &RelativePath,
        capability: Capability,
    ) -> Result<i64> {
        self.authorize_path(connection, subject, path, capability, true)
    }

    fn authorize_path(
        &self,
        connection: &Connection,
        subject: &Subject,
        path: &RelativePath,
        capability: Capability,
        allow_directory: bool,
    ) -> Result<i64> {
        let now = self.db.clock().millis();
        let policy = identity::policy_in(connection, self.db.server_id(), now, Some(subject))?;
        let parent = self.parent(path)?;
        let resource = self.root.resource(&parent.join(leaf(path)))?;
        for resource in [path.resource(), resource] {
            if let identity::Decision::Deny(reason) = policy.check(capability, Some(&resource)) {
                return Err(crate::ControlError::forbidden(reason).into());
            }
        }
        if let Some(resource) = self.checked_existing_path(path, allow_directory)? {
            super::authorize(&policy, capability, &resource)?;
        }
        Ok(now)
    }

    fn checked_existing_resource(
        &self,
        path: &RelativePath,
    ) -> Result<Option<crate::identity::Resource>> {
        self.checked_existing_path(path, false)
    }

    fn checked_existing_path(
        &self,
        path: &RelativePath,
        allow_directory: bool,
    ) -> Result<Option<crate::identity::Resource>> {
        let checked = match self.root.resolve(Some(path)) {
            Ok(checked) => checked,
            Err(FileError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        if allow_directory && checked.metadata.is_dir() {
            return Ok(Some(checked.resource));
        }
        if !checked.metadata.is_file() {
            return Err(FileError::NotRegular);
        }
        let file = native::open_regular(&checked.path)?;
        let actual = native::actual_path(&file, &checked.path)?;
        let resource = self.root.resource(&actual)?;
        if resource != checked.resource {
            return Err(FileError::LinkedPath);
        }
        Ok(Some(resource))
    }

    /// Verifies the current bytes against `expected`, copies them into the bounded trash
    /// and records the entry. Returns the new trash ID.
    #[cfg(test)]
    fn prepare_trash(
        &self,
        subject: &Subject,
        path: &RelativePath,
        expected: &str,
        reason: Reason,
        effect: Option<&str>,
    ) -> Result<String> {
        self.prepare_trash_with_id(subject, path, expected, reason, effect, &random_id()?)
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_trash_with_id(
        &self,
        subject: &Subject,
        path: &RelativePath,
        expected: &str,
        reason: Reason,
        effect: Option<&str>,
        id: &str,
    ) -> Result<String> {
        let mut owned_copy = None;
        let result = self.db.write_with(|transaction| {
            let connection = transaction.connection();
            let capability = if reason == Reason::Delete { Capability::FilesDelete } else { Capability::FilesOverwrite };
            let now = self.authorize(connection, subject, path, capability)?;
            let checked = self.root.resolve(Some(path))?;
            let mut source = native::open_regular(&checked.path)?;
            let resource = self.root.resource(&native::actual_path(&source, &checked.path)?)?;
            let policy = identity::policy_in(connection, self.db.server_id(), now, Some(subject))?;
            super::authorize(&policy, capability, &checked.resource)?;
            super::authorize(&policy, capability, &resource)?;
            let metadata = source.metadata().map_err(io_error)?;
            if metadata.len() > MAX_TRASH_FILE_BYTES { return Err(FileError::TooLarge); }
            self.require_trash_capacity(connection, metadata.len())?;
            require_space(self.db.directory().path(), metadata.len())?;
            let trash = self.trash_dir()?;
            let name = trash_name(id)?;
            let mut copy = trash.create_new(&name).map_err(state_io)?;
            owned_copy = Some(CopyGuard { trash: trash.clone(), name: name.clone(), retained: false });
            let (current, size) = copy_source(&mut source, &mut copy)?;
            if current != expected { return Err(FileError::Conflict(Some(current))); }
            self.require_trash_capacity_excluding(connection, size, Some(&name))?;
            require_space(self.db.directory().path(), 0)?;
            copy.sync_all().map_err(io_error)?;
            drop(copy);
            trash.sync().map_err(state_io)?;
            let retention = i64::try_from(self.trash_policy.retention.as_millis()).map_err(|_| FileError::InvalidLimit)?;
            connection.execute("INSERT INTO file_trash (id,server_id,path,sha256,size,reason,principal_id,device_id,status,effect_sha256,unix_mode,readonly,trashed_at,expires_at) VALUES (?,?,?,?,?,?,?,?,'pending',?,?,?,?,?)", params![
                id,self.db.server_id(),path.as_str(),current,i64::try_from(size).map_err(|_| FileError::TooLarge)?,reason.wire(),subject.principal.as_str(),subject.device.as_str(),effect,unix_mode(&metadata.permissions()),i64::from(metadata.permissions().readonly()),now,now.saturating_add(retention)
            ])?;
            Ok(id.to_owned())
        }).map_err(FileError::from);
        if let Some(copy) = &mut owned_copy {
            if let Err(error) = &result
                && copy.trash.remove(&copy.name).is_err()
            {
                // An unrecorded copy is still charged by physical quota scanning.
                return Err(FileError::RecoveryRequired(Box::new(error.clone())));
            }
            copy.retained = true;
        }
        result
    }

    pub(super) fn require_mutation_space(
        &self,
        parent: &Path,
        stage: u64,
        trash: u64,
    ) -> Result<()> {
        let state = self.db.directory().path();
        let same = native::same_volume(parent, state)?;
        let (target_need, state_need) = space_footprints(stage, trash, same);
        require_space(parent, target_need)?;
        require_space(state, state_need)
    }

    fn require_trash_capacity(&self, connection: &Connection, incoming: u64) -> Result<()> {
        self.require_trash_capacity_excluding(connection, incoming, None)
    }

    fn require_trash_capacity_excluding(
        &self,
        connection: &Connection,
        incoming: u64,
        owned: Option<&ArtifactName>,
    ) -> Result<()> {
        let (mut entries, used): (i64, i64) = connection.query_row(
            "SELECT count(*), COALESCE(sum(size),0) FROM file_trash WHERE server_id=?",
            [self.db.server_id()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (_, total) = native::volume_space(self.db.directory().path())?;
        let ceiling = self
            .trash_policy
            .max_bytes
            .min(total / 100 * u64::from(self.trash_policy.max_volume_percent));
        let mut used = u64::try_from(used).map_err(|_| FileError::Unavailable)?;
        // Also account for unrecorded copies left by a crash before pending commit or
        // denied cleanup. Never remove unknown data merely to fit the quota.
        let trash = self.trash_dir()?;
        let Some(count) = trash.count(100_001).map_err(state_io)? else {
            return Err(FileError::TrashFull);
        };
        let names = trash.list(100_001).map_err(state_io)?;
        if names.len() != count {
            return Err(FileError::TrashFull);
        }
        for name in names {
            if owned == Some(&name) {
                continue;
            }
            let recorded = if let Some(id) = name.as_str().strip_suffix(".bin") {
                connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM file_trash WHERE server_id=? AND id=?)",
                    params![self.db.server_id(), id],
                    |row| row.get::<_, bool>(0),
                )?
            } else {
                false
            };
            if !recorded {
                let file = trash
                    .open(&name)
                    .map_err(state_io)?
                    .ok_or(FileError::Unavailable)?;
                used = used.saturating_add(file.metadata().map_err(io_error)?.len());
                entries += 1;
            }
        }
        if usize::try_from(entries).map_err(|_| FileError::Unavailable)?
            >= self.trash_policy.max_entries
            || used.saturating_add(incoming) > ceiling
        {
            return Err(FileError::TrashFull);
        }
        Ok(())
    }

    fn trash_dir(&self) -> Result<ArtifactDirectory> {
        self.db.directory().trash().map_err(state_io)
    }

    fn audit<const N: usize>(
        &self,
        connection: &Connection,
        subject: &Subject,
        action: &str,
        path: &RelativePath,
        metadata: [(&str, Option<String>); N],
    ) -> Result<()> {
        let actor = AuditActor::new(
            subject.principal.as_str().to_owned(),
            subject.device.as_str().to_owned(),
            SOURCE.to_owned(),
        )?;
        let mut fields = path_fields("path", path.as_str());
        for (key, value) in metadata {
            if key == "to" {
                if let Some(value) = value {
                    fields.extend(path_fields(key, &value));
                }
            } else {
                fields.push((key.to_owned(), value));
            }
        }
        let record = AuditRecord::new(
            actor,
            action.to_owned(),
            TARGET.to_owned(),
            "success".to_owned(),
            None,
            fields,
        )?;
        self.audit.append(connection, &record)?;
        Ok(())
    }

    /// After a failed transaction, reverse a filesystem change it had already made.
    #[cfg(test)]
    fn settle<T>(&self, result: Result<T>, applied: Option<Applied>, target: &Path) -> Result<T> {
        self.settle_with_intent(result, applied, target, None)
    }

    fn settle_with_intent<T>(
        &self,
        result: Result<T>,
        applied: Option<Applied>,
        target: &Path,
        intent: Option<&str>,
    ) -> Result<T> {
        let (Err(error), Some(applied)) = (&result, applied) else {
            return result;
        };
        let reversed = (|| {
            require_hash(target, applied.effect.as_deref())?;
            if let Some(id) = &applied.trash {
                let trash = self.trash_dir()?;
                let name = trash_name(id)?;
                let (entry, permissions) = self
                    .db
                    .read_with(|snapshot| {
                        let connection = snapshot.connection();
                        Ok::<_, FileError>((
                            load_entry(connection, self.db.server_id(), id)?
                                .ok_or(FileError::Unavailable)?,
                            permissions_for(connection, self.db.server_id(), id)?,
                        ))
                    })
                    .map_err(FileError::from)?;
                let bytes = read_trash(&trash, &name, &entry)?;
                let parent = target.parent().ok_or(FileError::Unavailable)?;
                let stage = Stage::planned(parent)?;
                if let Some(intent) = intent {
                    self.record_compensation_stage(intent, &stage.leaf()?, entry.size)?;
                    stage.retained.set(true);
                }
                stage.create(&bytes)?;
                stage.permissions(&permissions)?;
                crash_point("compensation")?;
                if applied.effect.is_some() {
                    stage.replace(target)?;
                } else {
                    stage.link_absent(target)?;
                }
            } else {
                fs::remove_file(target).map_err(io_error)?;
            }
            Ok::<(), FileError>(())
        })();
        match reversed {
            Ok(()) => result,
            // The file change stands but was not recorded: never report it as clean.
            Err(_) => Err(FileError::RecoveryRequired(Box::new(error.clone()))),
        }
    }
}

struct Applied {
    trash: Option<String>,
    effect: Option<String>,
}

/// A hidden create-new stage beside the target; removed on drop unless consumed.
struct Stage {
    path: PathBuf,
    created: Cell<bool>,
    retained: Cell<bool>,
}
impl Stage {
    fn planned(parent: &Path) -> Result<Self> {
        Ok(Self {
            path: parent.join(format!("{STAGE_PREFIX}{}.tmp", random_id()?)),
            created: Cell::new(false),
            retained: Cell::new(false),
        })
    }
    #[cfg(all(test, unix))]
    fn write(parent: &Path, bytes: &[u8]) -> Result<Self> {
        let stage = Self::planned(parent)?;
        stage.create(bytes)?;
        Ok(stage)
    }
    fn create(&self, bytes: &[u8]) -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&self.path).map_err(io_error)?;
        self.created.set(true);
        file.write_all(bytes).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        Ok(())
    }
    fn leaf(&self) -> Result<String> {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .ok_or(FileError::InvalidPath)
    }
    fn permissions(&self, stored: &StoredPermissions) -> Result<()> {
        let mut permissions = fs::metadata(&self.path).map_err(io_error)?.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(stored.mode.ok_or(FileError::Unavailable)?);
        }
        #[cfg(windows)]
        permissions.set_readonly(stored.readonly);
        fs::set_permissions(&self.path, permissions).map_err(io_error)
    }
    /// No-replace: a hard link fails if anything appeared at the target meanwhile.
    fn link_absent(&self, target: &Path) -> Result<()> {
        fs::hard_link(&self.path, target).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                FileError::Conflict(None)
            } else {
                io_error(error)
            }
        })?;
        remove_stage(&self.path);
        Ok(())
    }
    fn replace(&self, target: &Path) -> Result<()> {
        fs::rename(&self.path, target).map_err(io_error)
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        if self.created.get() && !self.retained.get() {
            remove_stage(&self.path);
        }
    }
}

fn settle_rename<T>(
    result: Result<T>,
    source: &Path,
    destination: &Path,
    expected: &str,
) -> Result<T> {
    let Err(error) = &result else {
        return result;
    };
    let reversed = (|| {
        require_hash(destination, Some(expected))?;
        require_absent(source)?;
        native::rename_absent(destination, source)
    })();
    if reversed.is_err() {
        return Err(FileError::RecoveryRequired(Box::new(error.clone())));
    }
    result
}

pub(super) fn no_replace_error(error: std::io::Error) -> FileError {
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        FileError::Conflict(None)
    } else {
        io_error(error)
    }
}

fn require_absent(target: &Path) -> Result<()> {
    match fs::symlink_metadata(target) {
        Ok(metadata) => {
            let current = if metadata.is_file() && !metadata.file_type().is_symlink() {
                native::open_regular(target)
                    .ok()
                    .and_then(|file| hash_file(file).ok())
            } else {
                None
            };
            Err(FileError::Conflict(current))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

pub(super) fn require_space(parent: &Path, incoming: u64) -> Result<()> {
    let (available, _) = native::volume_space(parent)?;
    require_available(available, incoming)
}

fn space_footprints(stage: u64, trash: u64, same: bool) -> (u64, u64) {
    (
        stage.saturating_add(if same { trash } else { 0 }),
        trash.saturating_add(if same { stage } else { 0 }),
    )
}

fn require_available(available: u64, incoming: u64) -> Result<()> {
    if available < incoming.saturating_add(SPACE_RESERVE_BYTES) {
        return Err(FileError::InsufficientSpace);
    }
    Ok(())
}

fn read_trash(
    trash: &ArtifactDirectory,
    name: &ArtifactName,
    entry: &TrashEntry,
) -> Result<Vec<u8>> {
    let file = trash
        .open(name)
        .map_err(state_io)?
        .ok_or(FileError::NotFound)?;
    let mut bytes = Vec::new();
    file.take(entry.size + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 != entry.size || hex::encode(Sha256::digest(&bytes)) != entry.sha256 {
        return Err(FileError::Conflict(None));
    }
    Ok(bytes)
}

fn copy_source(source: &mut impl Read, copy: &mut impl Write) -> Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1_024];
    let mut size = 0_u64;
    loop {
        let read = source.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            return Ok((hex::encode(hasher.finalize()), size));
        }
        size += read as u64;
        if size > MAX_TRASH_FILE_BYTES {
            return Err(FileError::TooLarge);
        }
        hasher.update(&buffer[..read]);
        copy.write_all(&buffer[..read]).map_err(io_error)?;
    }
}

fn hash_file(mut file: File) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1_024];
    let mut size = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            return Ok(hex::encode(hasher.finalize()));
        }
        size += read as u64;
        if size > MAX_TRASH_FILE_BYTES {
            return Err(FileError::TooLarge);
        }
        hasher.update(&buffer[..read]);
    }
}

fn load_entry(connection: &Connection, server: &str, id: &str) -> Result<Option<TrashEntry>> {
    let row = connection
        .query_row(
            "SELECT path,sha256,size,trashed_at,expires_at FROM file_trash WHERE server_id=? AND id=?",
            params![server, id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;
    row.map(|(path, sha256, size, trashed_at, expires_at)| {
        Ok(TrashEntry {
            id: id.to_owned(),
            path: RelativePath::parse(&path).map_err(|_| FileError::Unavailable)?,
            sha256,
            size: u64::try_from(size).map_err(|_| FileError::Unavailable)?,
            trashed_at,
            expires_at,
        })
    })
    .transpose()
}

fn parent(path: &RelativePath) -> Result<Option<RelativePath>> {
    path.as_str()
        .rsplit_once('/')
        .map(|(parent, _)| RelativePath::parse(parent))
        .transpose()
}

fn leaf(path: &RelativePath) -> &str {
    path.as_str()
        .rsplit_once('/')
        .map_or(path.as_str(), |(_, leaf)| leaf)
}

pub(super) fn check_hex(text: &str) -> Result<()> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(FileError::InvalidPath);
    }
    Ok(())
}

pub(super) fn random_id() -> Result<String> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|_| FileError::Unavailable)?;
    Ok(hex::encode(random))
}

fn trash_name(id: &str) -> Result<ArtifactName> {
    if id.len() != 32
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(FileError::NotFound);
    }
    ArtifactName::new(&format!("{id}.bin")).map_err(|_| FileError::NotFound)
}

pub(super) fn state_io(error: vesta_state::StateError) -> FileError {
    match error.code() {
        "state_artifact_linked" | "state_dir_linked" | "state_file_linked" => FileError::LinkedPath,
        _ => FileError::Unavailable,
    }
}

/// Process-wide admission keyed by canonical database directory, deliberately broader
/// than a single root. Separate browsers and separately opened databases share it.
pub(super) fn mutation_admission(directory: &Path) -> Result<Arc<Mutex<()>>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let key = fs::canonicalize(directory).map_err(io_error)?;
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| FileError::Unavailable)?;
    locks.retain(|_, lock| lock.strong_count() != 0);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}

struct CopyGuard {
    trash: ArtifactDirectory,
    name: ArtifactName,
    retained: bool,
}
impl Drop for CopyGuard {
    fn drop(&mut self) {
        if !self.retained {
            let _ = self.trash.remove(&self.name);
        }
    }
}

struct StoredPermissions {
    #[cfg_attr(not(unix), allow(dead_code))]
    mode: Option<u32>,
    #[cfg_attr(not(windows), allow(dead_code))]
    readonly: bool,
}
fn permissions_for(connection: &Connection, server: &str, id: &str) -> Result<StoredPermissions> {
    connection
        .query_row(
            "SELECT unix_mode,readonly FROM file_trash WHERE server_id=? AND id=?",
            params![server, id],
            |row| {
                Ok(StoredPermissions {
                    mode: row.get(0)?,
                    readonly: row.get(1)?,
                })
            },
        )
        .map_err(FileError::from)
}
fn unix_mode(permissions: &fs::Permissions) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(permissions.mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = permissions;
        None
    }
}
fn complete(connection: &Connection, server: &str, id: Option<&str>) -> Result<()> {
    if let Some(id) = id {
        connection.execute("UPDATE file_trash SET status='complete' WHERE server_id=? AND id=? AND status='pending'", params![server,id])?;
    }
    Ok(())
}
fn require_mutable_target(target: &Path) -> Result<()> {
    #[cfg(windows)]
    if fs::metadata(target)
        .map_err(io_error)?
        .permissions()
        .readonly()
    {
        return Err(FileError::Unavailable);
    }
    #[cfg(not(windows))]
    let _ = target;
    Ok(())
}

fn require_hash(target: &Path, expected: Option<&str>) -> Result<()> {
    match expected {
        None => require_absent(target),
        Some(expected) => {
            let current = hash_file(native::open_regular(target)?)?;
            if current == expected {
                Ok(())
            } else {
                Err(FileError::Conflict(Some(current)))
            }
        }
    }
}
fn remove_stage(path: &Path) {
    let _ = native::remove_private_stage(path);
}

#[cfg(test)]
#[path = "mutation_tests.rs"]
mod tests;

/// Display is explicitly abbreviated; full UTF-8 path identity is always hashed.
fn path_fields(key: &str, text: &str) -> Vec<(String, Option<String>)> {
    let limit = vesta_state::audit::MAX_METADATA_VALUE_UNITS;
    let display = if text.encode_utf16().count() <= limit {
        text.to_owned()
    } else {
        let prefix: String = text
            .chars()
            .scan(0, |units, c| {
                *units += c.len_utf16();
                (*units <= 120).then_some(c)
            })
            .collect();
        let suffix: String = text
            .chars()
            .rev()
            .scan(0, |units, c| {
                *units += c.len_utf16();
                (*units <= 120).then_some(c)
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{prefix}...[abbrev]...{suffix}")
    };
    vec![
        (key.into(), Some(display)),
        (
            format!("{key}_sha256"),
            Some(hex::encode(Sha256::digest(text.as_bytes()))),
        ),
    ]
}
