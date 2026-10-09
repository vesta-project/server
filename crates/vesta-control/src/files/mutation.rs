//! Recoverable, optimistically concurrent single-file writes, deletes and restores.
//!
//! Callers name the bytes they expect to replace (`If-Match`) or require absence, so a
//! stale edit refuses with the current hash instead of overwriting newer content. The
//! previous bytes are copied into the bounded owner-only trash and recorded before the
//! game file changes. New bytes are staged as a hidden create-new file beside the target
//! and moved into place in the same directory. Pending provenance commits before the
//! destructive effect. Authority and the precondition are rechecked in the completion
//! transaction together with the success audit. Failed completion is compensated only
//! while the effect's hash still matches. Explicit reconciliation retains uncertain data.
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
const SOURCE: &str = "files";
const TARGET: &str = "file";
const STAGE_PREFIX: &str = ".vesta-stage-";

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
    pub fn write(
        &self,
        subject: &Subject,
        path: &RelativePath,
        precondition: &WritePrecondition,
        bytes: &[u8],
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
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = admission.lock().map_err(|_| FileError::Unavailable)?;
        let parent = self.authorized_parent(subject, path, capability)?;
        let target = parent.join(leaf(path));
        let incoming = match precondition {
            WritePrecondition::Absent => 0,
            WritePrecondition::Matches(_) => fs::metadata(&target).map_err(io_error)?.len(),
        };
        self.require_mutation_space(&parent, bytes.len() as u64, incoming)?;
        let stage = Stage::write(&parent, bytes)?;
        let trash = match precondition {
            WritePrecondition::Absent => None,
            WritePrecondition::Matches(expected) => {
                Some(self.prepare_trash(subject, path, expected, Reason::Replace, Some(&sha256))?)
            }
        };
        let applied = Cell::new(None::<Applied>);
        let result = self.db.write_with(|transaction| {
            let connection = transaction.connection();
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
            match precondition {
                WritePrecondition::Absent => stage.link_absent(&target)?,
                WritePrecondition::Matches(_) => stage.replace(&target)?,
            }
            applied.set(Some(Applied {
                trash: trash.clone(),
                effect: Some(sha256.clone()),
            }));
            complete(connection, self.db.server_id(), trash.as_deref())?;
            self.audit(
                connection,
                subject,
                "files.write",
                path,
                [
                    ("sha256", Some(sha256.clone())),
                    ("size", Some(bytes.len().to_string())),
                    ("trash", trash.clone()),
                ],
            )?;
            Ok(FileChange {
                sha256: sha256.clone(),
                size: bytes.len() as u64,
                trash: trash.clone(),
            })
        });
        drop(stage);
        let result = self.settle(result.map_err(FileError::from), applied.take(), &target);
        if let Err(primary) = &result
            && let Some(id) = trash.as_deref()
            && self.retire_failed(id, &result).is_err()
        {
            return Err(FileError::RecoveryRequired(Box::new(primary.clone())));
        }
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
        let id = self.prepare_trash(subject, path, expected_sha256, Reason::Delete, None)?;
        let applied = Cell::new(None::<Applied>);
        let result = self.db.write_with(|transaction| {
            let connection = transaction.connection();
            self.authorize(connection, subject, path, Capability::FilesDelete)?;
            require_hash(&target, Some(expected_sha256))?;
            require_mutable_target(&target)?;
            // Read-only and sharing-denied Windows targets refuse without changing attributes.
            fs::remove_file(&target).map_err(io_error)?;
            applied.set(Some(Applied {
                trash: Some(id.clone()),
                effect: None,
            }));
            complete(connection, self.db.server_id(), Some(&id))?;
            self.audit(
                connection,
                subject,
                "files.delete",
                path,
                [("trash", Some(id.clone()))],
            )?;
            load_entry(connection, self.db.server_id(), &id)?.ok_or(FileError::Unavailable)
        });
        let result = self.settle(result.map_err(FileError::from), applied.take(), &target);
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
        let stage = Stage::write(&parent, &bytes)?;
        let permissions = self
            .db
            .read_with(|snapshot| permissions_for(snapshot.connection(), self.db.server_id(), id))
            .map_err(FileError::from)?;
        stage.permissions(&permissions)?;
        let target = parent.join(leaf(&entry.path));
        let applied = Cell::new(None::<Applied>);
        let result = self.db.write_with(|transaction| {
            let connection = transaction.connection();
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
            stage.link_absent(&target)?;
            applied.set(Some(Applied {
                trash: None,
                effect: Some(entry.sha256.clone()),
            }));
            connection.execute(
                "UPDATE file_trash SET status='restored' WHERE server_id=? AND id=?",
                params![self.db.server_id(), id],
            )?;
            self.audit(
                connection,
                subject,
                "files.restore",
                &entry.path,
                [("trash", Some(id.to_owned()))],
            )?;
            Ok(FileChange {
                sha256: entry.sha256.clone(),
                size: entry.size,
                trash: None,
            })
        });
        drop(stage);
        let change = self.settle(result.map_err(FileError::from), applied.take(), &target)?;
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
                "SELECT id FROM file_trash WHERE server_id=? AND ((status='complete' AND expires_at<=?) OR status='restored')")?;
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

    /// Explicit startup reconciliation, invoked by trusted host composition. No data is
    /// deleted. Known completed effects become complete with a recovered success audit;
    /// unchanged or unknown targets retain their copy and become recovery-required.
    pub fn reconcile_pending(&self) -> Result<usize> {
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = admission.lock().map_err(|_| FileError::Unavailable)?;
        self.db.write_with(|transaction| {
            let connection = transaction.connection();
            let mut statement = connection.prepare("SELECT id,path,effect_sha256,principal_id,device_id,reason FROM file_trash WHERE server_id=? AND status='pending'")?;
            let rows = statement.query_map([self.db.server_id()], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, Option<String>>(2)?,row.get::<_, String>(3)?,row.get::<_, String>(4)?,row.get::<_, String>(5)?)))?
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
        }).map_err(FileError::from)
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

    fn authorized_parent(
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

    fn authorize(
        &self,
        connection: &Connection,
        subject: &Subject,
        path: &RelativePath,
        capability: Capability,
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
        if let Some(resource) = self.checked_existing_resource(path)? {
            super::authorize(&policy, capability, &resource)?;
        }
        Ok(now)
    }

    fn checked_existing_resource(
        &self,
        path: &RelativePath,
    ) -> Result<Option<crate::identity::Resource>> {
        let checked = match self.root.resolve(Some(path)) {
            Ok(checked) => checked,
            Err(FileError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
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
    #[allow(clippy::too_many_arguments)]
    fn prepare_trash(
        &self,
        subject: &Subject,
        path: &RelativePath,
        expected: &str,
        reason: Reason,
        effect: Option<&str>,
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
            let id = random_id()?;
            let name = trash_name(&id)?;
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
            Ok(id)
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

    fn require_mutation_space(&self, parent: &Path, stage: u64, trash: u64) -> Result<()> {
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
        let mut fields = vec![("path".to_owned(), Some(path.as_str().to_owned()))];
        fields.extend(metadata.map(|(key, value)| (key.to_owned(), value)));
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
    fn settle<T>(&self, result: Result<T>, applied: Option<Applied>, target: &Path) -> Result<T> {
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
                let stage = Stage::write(parent, &bytes)?;
                stage.permissions(&permissions)?;
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
}
impl Stage {
    fn write(parent: &Path, bytes: &[u8]) -> Result<Self> {
        let path = parent.join(format!("{STAGE_PREFIX}{}.tmp", random_id()?));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(io_error)?;
        let stage = Self { path };
        file.write_all(bytes).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        Ok(stage)
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
        remove_stage(&self.path);
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

fn require_space(parent: &Path, incoming: u64) -> Result<()> {
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

fn check_hex(text: &str) -> Result<()> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(FileError::InvalidPath);
    }
    Ok(())
}

fn random_id() -> Result<String> {
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

fn state_io(error: vesta_state::StateError) -> FileError {
    match error.code() {
        "state_artifact_linked" | "state_dir_linked" | "state_file_linked" => FileError::LinkedPath,
        _ => FileError::Unavailable,
    }
}

/// Process-wide admission keyed by canonical database directory, deliberately broader
/// than a single root. Separate browsers and separately opened databases share it.
fn mutation_admission(directory: &Path) -> Result<Arc<Mutex<()>>> {
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
