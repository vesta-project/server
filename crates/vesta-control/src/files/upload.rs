//! Bounded resumable staging in protected state. IDs are lookup keys, not authority.
//! Fixed expiry is not renewed by chunks. Reservations charge the whole declared size,
//! including incomplete uploads, so a crashed append cannot evade the byte ceiling.
//! Filesystem work shares mutation admission across browsers; database transactions
//! serialize reservations. As elsewhere, this is not hostile same-account confinement.
use super::mutation::{check_hex, mutation_admission, random_id, require_space, state_io};
use super::{
    FileBrowser, FileChange, FileError, MAX_WRITE_BYTES, RelativePath, Result, WritePrecondition,
};
use crate::identity::{self, Capability, Subject};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    io::{Read, Seek, SeekFrom, Write},
    time::Duration,
};
use vesta_state::artifacts::{ArtifactDirectory, ArtifactName};

pub const MAX_UPLOAD_CHUNK_BYTES: usize = 1_024 * 1_024;
const MAX_SCAN: usize = 100_001;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UploadPolicy {
    per_subject: usize,
    global: usize,
    staged_bytes: u64,
    expiry: Duration,
}
impl UploadPolicy {
    pub fn new(
        per_subject: usize,
        global: usize,
        staged_bytes: u64,
        expiry: Duration,
    ) -> Result<Self> {
        if per_subject == 0
            || global == 0
            || per_subject > global
            || global > 10_000
            || staged_bytes == 0
            || staged_bytes > 1024 * 1024 * 1024
            || expiry.as_millis() == 0
            || i64::try_from(expiry.as_millis()).is_err()
        {
            return Err(FileError::InvalidLimit);
        }
        Ok(Self {
            per_subject,
            global,
            staged_bytes,
            expiry,
        })
    }
}
impl Default for UploadPolicy {
    fn default() -> Self {
        Self {
            per_subject: 4,
            global: 64,
            staged_bytes: 256 * 1024 * 1024,
            expiry: Duration::from_secs(3600),
        }
    }
}

/// Opaque random identifier; parsing does not confer authority.
#[derive(Clone, Eq, PartialEq)]
pub struct UploadId(String);
impl UploadId {
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() != 32
            || !text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(FileError::NotFound);
        }
        Ok(Self(text.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    fn name(&self) -> Result<ArtifactName> {
        ArtifactName::new(&format!("{}.bin", self.0)).map_err(state_io)
    }
}
impl fmt::Debug for UploadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UploadId[<redacted>]")
    }
}

struct Upload {
    path: RelativePath,
    precondition: WritePrecondition,
    total: u64,
    sha256: String,
}
impl Upload {
    fn capability(&self) -> Capability {
        capability(&self.precondition)
    }
}
fn capability(precondition: &WritePrecondition) -> Capability {
    match precondition {
        WritePrecondition::Absent => Capability::FilesCreate,
        WritePrecondition::Matches(_) => Capability::FilesOverwrite,
    }
}

impl FileBrowser {
    pub fn with_upload_policy(mut self, policy: UploadPolicy) -> Self {
        self.upload_policy = policy;
        self
    }

    pub fn begin(
        &self,
        subject: &Subject,
        path: &RelativePath,
        precondition: &WritePrecondition,
        total_size: u64,
        final_sha256: &str,
    ) -> Result<UploadId> {
        if total_size > MAX_WRITE_BYTES as u64 {
            return Err(FileError::TooLarge);
        }
        check_hex(final_sha256)?;
        if let WritePrecondition::Matches(hash) = precondition {
            check_hex(hash)?;
        }
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = self.mutation_guard(&admission)?;
        // Authorize before maintenance or inspecting quota. Recheck in reservation transaction.
        self.authorized_parent(subject, path, capability(precondition))?;
        self.purge_uploads_locked()?;
        let directory = self.upload_dir()?;
        let id = UploadId(random_id()?);
        let name = id.name()?;
        let mut created = false;
        let result = self.db.write_with(|tx| {
            let c = tx.connection();
            let now = self.authorize(c, subject, path, capability(precondition))?;
            let (global, own, reserved): (i64, i64, i64) = c.query_row(
                "SELECT count(*), COALESCE(sum(CASE WHEN principal_id=? AND device_id=? THEN 1 ELSE 0 END),0), COALESCE(sum(total_size),0) FROM file_uploads",
                params![subject.principal.as_str(), subject.device.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            if global >= self.upload_policy.global as i64 || own >= self.upload_policy.per_subject as i64
                || u64::try_from(reserved).map_err(|_| FileError::Unavailable)?.saturating_add(total_size) > self.upload_policy.staged_bytes
            { return Err(FileError::UploadFull); }
            // Unknown or unremovable physical files cannot evade count/space accounting.
            let physical = directory.count(MAX_SCAN).map_err(state_io)?.ok_or(FileError::UploadFull)?;
            if physical != usize::try_from(global).map_err(|_| FileError::Unavailable)? { return Err(FileError::UploadFull); }
            require_space(self.db.directory().path(), total_size)?;
            let expires = now.checked_add(self.upload_policy.expiry.as_millis() as i64).ok_or(FileError::InvalidLimit)?;
            let file = directory.create_new(&name).map_err(state_io)?;
            created = true;
            file.sync_all().map_err(super::io_error)?;
            drop(file);
            directory.sync().map_err(state_io)?;
            let expected = match precondition { WritePrecondition::Absent => None, WritePrecondition::Matches(s) => Some(s.as_str()) };
            c.execute("INSERT INTO file_uploads (id,server_id,principal_id,device_id,path,expected_sha256,total_size,final_sha256,expires_at) VALUES (?,?,?,?,?,?,?,?,?)",
                params![id.as_str(), self.db.server_id(), subject.principal.as_str(), subject.device.as_str(), path.as_str(), expected, total_size as i64, final_sha256, expires])?;
            self.remote_upload_complete(c, subject, "files.upload.begin", serde_json::json!({"upload_id":id.as_str(),"offset":"0"}))?;
            Ok(id.clone())
        }).map_err(FileError::from);
        if let Err(error) = &result
            && created
            && directory.remove(&name).is_err()
        {
            return Err(FileError::RecoveryRequired(Box::new(error.clone())));
        }
        result
    }

    /// Returns current staged length, including on a byte-identical duplicate range.
    ///
    /// The staged length is always the stage file's actual length, never a separately
    /// recorded counter. If a write fails partway, the persisted prefix stays; a retry
    /// at an offset at or below that length is accepted only when every overlapping
    /// byte matches, and then only the remainder is appended and synced. A mismatch
    /// is a conflict; a gap (offset beyond the length) is a conflict.
    pub fn append(
        &self,
        subject: &Subject,
        id: &UploadId,
        offset: u64,
        bytes: &[u8],
    ) -> Result<u64> {
        if bytes.len() > MAX_UPLOAD_CHUNK_BYTES {
            return Err(FileError::TooLarge);
        }
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = self.mutation_guard(&admission)?;
        self.db
            .write_with(|tx| {
                let upload = self.owned_upload(tx.connection(), subject, id)?;
                let end = offset
                    .checked_add(bytes.len() as u64)
                    .ok_or(FileError::TooLarge)?;
                if end > upload.total {
                    return Err(FileError::TooLarge);
                }
                let mut file = self
                    .upload_dir()?
                    .open(&id.name()?)
                    .map_err(state_io)?
                    .ok_or(FileError::NotFound)?;
                let length = file.metadata().map_err(super::io_error)?.len();
                if length > upload.total {
                    return Err(FileError::TooLarge);
                }
                if offset > length {
                    return Err(FileError::Conflict(None));
                }
                // Bytes already staged in [offset, min(end, length)) must match exactly.
                // This covers byte-identical duplicates and a retry after an earlier
                // append persisted only a prefix of its chunk before failing.
                let overlap =
                    usize::try_from(length.min(end) - offset).map_err(|_| FileError::TooLarge)?;
                if overlap > 0 {
                    file.seek(SeekFrom::Start(offset))
                        .map_err(super::io_error)?;
                    let mut previous = vec![0; overlap];
                    file.read_exact(&mut previous).map_err(super::io_error)?;
                    if previous != bytes[..overlap] {
                        return Err(FileError::Conflict(None));
                    }
                }
                let remainder = &bytes[overlap..];
                if remainder.is_empty() {
                    // A previous append may have written bytes but failed its flush.
                    file.sync_all().map_err(super::io_error)?;
                    self.remote_upload_complete(
                        tx.connection(),
                        subject,
                        "files.upload.append",
                        serde_json::json!({"offset":length.to_string()}),
                    )?;
                    return Ok(length);
                }
                require_space(self.db.directory().path(), remainder.len() as u64)?;
                file.seek(SeekFrom::Start(length))
                    .map_err(super::io_error)?;
                // On failure no truncation is attempted: the next call re-reads the
                // file's actual length and verifies any persisted prefix above.
                file.write_all(remainder).map_err(super::io_error)?;
                file.sync_all().map_err(super::io_error)?;
                self.remote_upload_complete(
                    tx.connection(),
                    subject,
                    "files.upload.append",
                    serde_json::json!({"offset":end.to_string()}),
                )?;
                Ok(end)
            })
            .map_err(FileError::from)
    }

    pub fn finish(&self, subject: &Subject, id: &UploadId) -> Result<FileChange> {
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = self.mutation_guard(&admission)?;
        let upload = self
            .db
            .read_with(|s| self.owned_upload(s.connection(), subject, id))
            .map_err(FileError::from)?;
        let directory = self.upload_dir()?;
        let file = directory
            .open(&id.name()?)
            .map_err(state_io)?
            .ok_or(FileError::NotFound)?;
        if file.metadata().map_err(super::io_error)?.len() != upload.total {
            return Err(FileError::Conflict(None));
        }
        let mut bytes = Vec::new();
        file.take(upload.total + 1)
            .read_to_end(&mut bytes)
            .map_err(super::io_error)?;
        if bytes.len() as u64 != upload.total
            || hex::encode(Sha256::digest(&bytes)) != upload.sha256
        {
            return Err(FileError::Conflict(None));
        }
        if matches!(upload.precondition, WritePrecondition::Matches(_)) {
            match self.root.resolve(Some(&upload.path)) {
                Err(FileError::NotFound) => return Err(FileError::Conflict(None)),
                Err(error) => return Err(error),
                Ok(_) => {}
            }
        }
        let change = self.write_locked(
            subject,
            &upload.path,
            &upload.precondition,
            &bytes,
            Some(id.as_str()),
        )?;
        // The row is retired atomically with success. A crash here leaves an orphan,
        // cleaned on reconciliation; never misreport a committed write as a retryable failure.
        let _ = directory.remove(&id.name()?);
        Ok(change)
    }

    pub fn abort(&self, subject: &Subject, id: &UploadId) -> Result<()> {
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = self.mutation_guard(&admission)?;
        self.db
            .write_with(|tx| {
                let connection = tx.connection();
                let upload = self.owned_upload_record(connection, subject, id, false)?;
                let policy = identity::policy_in(
                    connection,
                    self.db.server_id(),
                    self.db.clock().millis(),
                    Some(subject),
                )?;
                // Aborting only removes protected staging, not a game-tree entry.
                // A renamed/missing destination must not strand its reservation.
                super::authorize(&policy, upload.capability(), &upload.path.resource())?;
                let retained: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM file_intents WHERE status IN ('pending','recovery-required') AND json_extract(details_json,'$.upload')=?)",
                    [id.as_str()], |row| row.get(0),
                )?;
                if retained {
                    return Err(FileError::RecoveryRequired(Box::new(FileError::Unavailable)));
                }
                self.upload_dir()?.remove(&id.name()?).map_err(state_io)?;
                tx.connection().execute(
                    "DELETE FROM file_uploads WHERE server_id=? AND id=?",
                    params![self.db.server_id(), id.as_str()],
                )?;
                self.remote_upload_complete(connection, subject, "files.upload.abort", serde_json::json!({"outcome":"succeeded"}))?;
                Ok(())
            })
            .map_err(FileError::from)
    }

    /// Trusted host maintenance: purge expired rows, missing stages and owned orphan names.
    pub fn purge_expired_uploads(&self) -> Result<usize> {
        let admission = mutation_admission(self.db.directory().path())?;
        let _guard = self.mutation_guard(&admission)?;
        self.purge_uploads_locked()
    }
    fn purge_uploads_locked(&self) -> Result<usize> {
        let directory = self.upload_dir()?;
        self.db.write_with(|tx| {
            let c = tx.connection();
            let mut statement = c.prepare("SELECT id,expires_at FROM file_uploads WHERE server_id=? ORDER BY id LIMIT 100001")?;
            let rows = statement.query_map([self.db.server_id()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
            if rows.len() >= MAX_SCAN { return Err(FileError::UploadFull); }
            let mut purged = 0;
            for (text, expires) in rows {
                let retained: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM file_intents WHERE status IN ('pending','recovery-required') AND json_extract(details_json,'$.upload')=?)", [&text], |r| r.get(0))?;
                if retained { continue; }
                let id = UploadId::parse(&text)?;
                if expires <= self.db.clock().millis() || directory.open(&id.name()?).map_err(state_io)?.is_none() {
                    directory.remove(&id.name()?).map_err(state_io)?;
                    c.execute("DELETE FROM file_uploads WHERE server_id=? AND id=?", params![self.db.server_id(), text])?;
                    purged += 1;
                }
            }
            if directory.count(MAX_SCAN).map_err(state_io)?.is_none() { return Err(FileError::UploadFull); }
            for name in directory.list(MAX_SCAN).map_err(state_io)? {
                let Some(text) = name.as_str().strip_suffix(".bin") else { continue; };
                if UploadId::parse(text).is_err() { continue; }
                let recorded: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM file_uploads WHERE id=?) OR EXISTS(SELECT 1 FROM file_intents WHERE status IN ('pending','recovery-required') AND json_extract(details_json,'$.upload')=?)", params![text,text], |r| r.get(0))?;
                if !recorded { directory.remove(&name).map_err(state_io)?; purged += 1; }
            }
            Ok(purged)
        }).map_err(FileError::from)
    }
    fn upload_dir(&self) -> Result<ArtifactDirectory> {
        self.db.directory().uploads().map_err(state_io)
    }
    fn owned_upload(&self, c: &Connection, subject: &Subject, id: &UploadId) -> Result<Upload> {
        let upload = self.owned_upload_record(c, subject, id, true)?;
        self.authorize(c, subject, &upload.path, upload.capability())?;
        Ok(upload)
    }
    pub(super) fn upload_authority(
        &self,
        subject: &Subject,
        id: &UploadId,
    ) -> Result<(RelativePath, Capability)> {
        self.db
            .read_with(|snapshot| {
                let upload = self.owned_upload_record(snapshot.connection(), subject, id, false)?;
                Ok((upload.path.clone(), upload.capability()))
            })
            .map_err(FileError::from)
    }
    fn owned_upload_record(
        &self,
        c: &Connection,
        subject: &Subject,
        id: &UploadId,
        require_unexpired: bool,
    ) -> Result<Upload> {
        let row: Option<(String, Option<String>, i64, String)> = c.query_row(
            "SELECT path,expected_sha256,total_size,final_sha256 FROM file_uploads WHERE server_id=? AND id=? AND principal_id=? AND device_id=? AND (? OR expires_at>?)",
            params![self.db.server_id(), id.as_str(), subject.principal.as_str(), subject.device.as_str(), !require_unexpired, self.db.clock().millis()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).optional()?;
        let (path, expected, total, sha256) = row.ok_or(FileError::NotFound)?;
        let upload = Upload {
            path: RelativePath::parse(&path)?,
            precondition: expected.map_or(WritePrecondition::Absent, WritePrecondition::Matches),
            total: u64::try_from(total).map_err(|_| FileError::Unavailable)?,
            sha256,
        };
        if upload.total > MAX_WRITE_BYTES as u64 {
            return Err(FileError::TooLarge);
        }
        Ok(upload)
    }
}

#[cfg(test)]
#[path = "upload_tests.rs"]
mod tests;
