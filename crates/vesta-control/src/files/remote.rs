//! Durable device-scoped HTTP request admission. Outcomes commit with native success.
use super::{FileBrowser, FileError, RelativePath, Result, UploadId, WritePrecondition};
use crate::identity::{self, Capability, Resource, Subject};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Mutex;

// Separate from native mutation admission: the request owns this through preparation
// and completion, while the existing browser owns its native effect lock.
static ADMISSION: Mutex<()> = Mutex::new(());
pub(super) const REMOTE_MUTATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

// Never retain request admission indefinitely while waiting for a native effect slot.
pub(super) fn lock_admission<'a>(
    lock: &'a Mutex<()>,
    deadline: Option<std::time::Instant>,
) -> Result<std::sync::MutexGuard<'a, ()>> {
    let Some(deadline) = deadline else {
        return lock.lock().map_err(|_| FileError::Unavailable);
    };
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(FileError::Unavailable);
        }
        match lock.try_lock() {
            Ok(guard) => {
                if std::time::Instant::now() >= deadline {
                    return Err(FileError::Unavailable);
                }
                return Ok(guard);
            }
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(FileError::Unavailable),
            Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(
                std::time::Duration::from_millis(5)
                    .min(deadline.saturating_duration_since(std::time::Instant::now())),
            ),
        }
    }
}
const MAX_OUTCOMES: i64 = 10_000;
const OUTCOME_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const OUTCOME_PURGE_BATCH: i64 = 128;

pub enum RemoteFileMutation<'a> {
    Write {
        path: &'a RelativePath,
        precondition: &'a WritePrecondition,
        bytes: &'a [u8],
    },
    Delete {
        path: &'a RelativePath,
        expected_sha256: &'a str,
    },
    Restore {
        id: &'a str,
    },
    Mkdir {
        path: &'a RelativePath,
    },
    Rename {
        from: &'a RelativePath,
        to: &'a RelativePath,
        expected_sha256: &'a str,
    },
    Begin {
        path: &'a RelativePath,
        precondition: &'a WritePrecondition,
        total_size: u64,
        sha256: &'a str,
    },
    Append {
        id: &'a UploadId,
        offset: u64,
        bytes: &'a [u8],
    },
    Finish {
        id: &'a UploadId,
    },
    Abort {
        id: &'a UploadId,
    },
}
impl RemoteFileMutation<'_> {
    fn input(&self) -> Value {
        let precondition = |value: &WritePrecondition| match value {
            WritePrecondition::Absent => json!({"absent":true}),
            WritePrecondition::Matches(hash) => json!({"matches":hash}),
        };
        let hash = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
        match self {
            Self::Write {
                path,
                precondition: p,
                bytes,
            } => json!(["write", path.as_str(), precondition(p), hash(bytes)]),
            Self::Delete {
                path,
                expected_sha256,
            } => json!(["delete", path.as_str(), expected_sha256]),
            Self::Restore { id } => json!(["restore", id]),
            Self::Mkdir { path } => json!(["mkdir", path.as_str()]),
            Self::Rename {
                from,
                to,
                expected_sha256,
            } => json!(["rename", from.as_str(), to.as_str(), expected_sha256]),
            Self::Begin {
                path,
                precondition: p,
                total_size,
                sha256,
            } => json!(["begin", path.as_str(), precondition(p), total_size, sha256]),
            Self::Append { id, offset, bytes } => {
                json!(["append", id.as_str(), offset, hash(bytes)])
            }
            Self::Finish { id } => json!(["finish", id.as_str()]),
            Self::Abort { id } => json!(["abort", id.as_str()]),
        }
    }
    fn authority(&self, files: &FileBrowser, subject: &Subject) -> Result<Vec<(String, String)>> {
        let write = |p: &WritePrecondition| match p {
            WritePrecondition::Absent => Capability::FilesCreate,
            WritePrecondition::Matches(_) => Capability::FilesOverwrite,
        };
        let item =
            |cap: Capability, path: &RelativePath| (cap.id().to_owned(), path.as_str().to_owned());
        Ok(match self {
            Self::Write {
                path, precondition, ..
            }
            | Self::Begin {
                path, precondition, ..
            } => vec![item(write(precondition), path)],
            Self::Delete { path, .. } => vec![item(Capability::FilesDelete, path)],
            Self::Mkdir { path } => vec![item(Capability::FilesMkdir, path)],
            Self::Rename { from, to, .. } => vec![
                item(Capability::FilesRename, from),
                item(Capability::FilesRename, to),
            ],
            Self::Restore { id } => {
                files.check_remote_surface(subject, &[Capability::FilesRead])?;
                let entry = files
                    .trash_entries(subject)?
                    .into_iter()
                    .find(|e| e.id == *id)
                    .ok_or(FileError::NotFound)?;
                if entry.path.as_str().contains('~') {
                    return Err(FileError::ProtectedPath);
                }
                if entry.path.as_str().split('/').any(|name| {
                    [
                        "server.properties",
                        "whitelist.json",
                        "banned-players.json",
                        "banned-ips.json",
                        "ops.json",
                    ]
                    .iter()
                    .any(|p| name.eq_ignore_ascii_case(p))
                }) {
                    return Err(FileError::ProtectedPath);
                }

                vec![
                    item(Capability::FilesRead, &entry.path),
                    item(Capability::FilesCreate, &entry.path),
                ]
            }
            Self::Append { id, .. } | Self::Finish { id } | Self::Abort { id } => {
                let (path, capability) = files.upload_authority(subject, id)?;
                vec![item(capability, &path)]
            }
        })
    }
}
impl FileBrowser {
    /// Admission without disclosing a trash/upload lookup result. This does not
    /// authorize any resource; the browser checks exact scopes before its effect.
    pub fn check_remote_surface(
        &self,
        subject: &Subject,
        capabilities: &[Capability],
    ) -> Result<()> {
        self.db
            .read_with(|snapshot| {
                let policy = identity::policy_in(
                    snapshot.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                    Some(subject),
                )?;
                if capabilities.iter().any(|cap| policy.has_scoped_grant(*cap)) {
                    Ok(())
                } else {
                    Err(crate::ControlError::forbidden(identity::DenyReason::NotGranted).into())
                }
            })
            .map_err(FileError::from)
    }
    /// Key is a bounded transport-scoped digest, never a permission. Pending records
    /// after process loss refuse replay until operator reconciliation; no blind effect retry.
    pub fn mutate_remote(
        &self,
        subject: &Subject,
        key: &str,
        request: RemoteFileMutation<'_>,
    ) -> Result<Value> {
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(FileError::InvalidPath);
        }
        let remote = self.for_remote();
        remote.execute_remote(
            subject,
            key,
            request.input(),
            || request.authority(&remote, subject),
            |files| match request {
                RemoteFileMutation::Write {
                    path,
                    precondition,
                    bytes,
                } => files.write(subject, path, precondition, bytes).map(change),
                RemoteFileMutation::Delete {
                    path,
                    expected_sha256,
                } => files
                    .delete(subject, path, expected_sha256)
                    .map(|e| json!({"outcome":"succeeded","trash_id":e.id})),
                RemoteFileMutation::Restore { id } => files.restore(subject, id).map(change),
                RemoteFileMutation::Mkdir { path } => files
                    .mkdir(subject, path)
                    .map(|()| json!({"outcome":"succeeded"})),
                RemoteFileMutation::Rename {
                    from,
                    to,
                    expected_sha256,
                } => files.rename(subject, from, to, expected_sha256).map(change),
                RemoteFileMutation::Begin {
                    path,
                    precondition,
                    total_size,
                    sha256,
                } => files
                    .begin(subject, path, precondition, total_size, sha256)
                    .map(|id| json!({"upload_id":id.as_str(),"offset":"0"})),
                RemoteFileMutation::Append { id, offset, bytes } => files
                    .append(subject, id, offset, bytes)
                    .map(|offset| json!({"offset":offset.to_string()})),
                RemoteFileMutation::Finish { id } => files.finish(subject, id).map(change),
                RemoteFileMutation::Abort { id } => files
                    .abort(subject, id)
                    .map(|()| json!({"outcome":"succeeded"})),
            },
        )
    }

    pub(crate) fn execute_remote(
        &self,
        subject: &Subject,
        key: &str,
        input: Value,
        authority: impl FnOnce() -> Result<Vec<(String, String)>>,
        effect: impl FnOnce(&Self) -> Result<Value>,
    ) -> Result<Value> {
        let mut files = self.clone();
        files.remote_deadline = Some(
            self.remote_deadline
                .unwrap_or_else(|| std::time::Instant::now() + REMOTE_MUTATION_TIMEOUT),
        );
        files.execute_remote_bounded(subject, key, input, authority, effect)
    }
    fn execute_remote_bounded(
        &self,
        subject: &Subject,
        key: &str,
        input: Value,
        authority: impl FnOnce() -> Result<Vec<(String, String)>>,
        effect: impl FnOnce(&Self) -> Result<Value>,
    ) -> Result<Value> {
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(FileError::InvalidPath);
        }
        let guard = self.request_admission()?;
        self.purge_remote_outcomes()?;
        let input = format!("{:x}", Sha256::digest(input.to_string().as_bytes()));
        let existing: Option<(String,String,Option<String>)> = self.db.read_with(|s| {
            s.connection().query_row("SELECT input_sha256,authority_json,outcome_json FROM management_file_outcomes WHERE server_id=? AND principal_id=? AND device_id=? AND request_key=?",
                params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),key], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(FileError::from)
        }).map_err(FileError::from)?;
        if let Some((saved, authority, outcome)) = existing {
            let authority: Vec<(String, String)> =
                serde_json::from_str(&authority).map_err(|_| FileError::Unavailable)?;
            self.db
                .read_with(|s| self.remote_authorize(s.connection(), subject, &authority))
                .map_err(FileError::from)?;
            if saved != input {
                return Err(FileError::IdempotencyConflict);
            }
            return outcome
                .map(|s| serde_json::from_str(&s).map_err(|_| FileError::Unavailable))
                .unwrap_or_else(|| {
                    Err(FileError::RecoveryRequired(Box::new(
                        FileError::Unavailable,
                    )))
                });
        }
        let authority = authority()?;
        self.db
            .write_with(|tx| {
                let c = tx.connection();
                self.remote_authorize(c, subject, &authority)?;
                let count: i64 = c.query_row(
                    "SELECT count(*) FROM management_file_outcomes WHERE server_id=?",
                    [self.db.server_id()],
                    |r| r.get(0),
                )?;
                if count >= MAX_OUTCOMES {
                    return Err(FileError::IntentsFull);
                }
                c.execute(
                    "INSERT INTO management_file_outcomes (server_id,principal_id,device_id,request_key,input_sha256,authority_json,outcome_json) VALUES (?,?,?,?,?,?,NULL)",
                    params![
                        self.db.server_id(),
                        subject.principal.as_str(),
                        subject.device.as_str(),
                        key,
                        input,
                        serde_json::to_string(&authority).map_err(|_| FileError::Unavailable)?
                    ],
                )?;
                Ok::<_, FileError>(())
            })
            .map_err(FileError::from)?;
        let mut files = self.clone();
        files.remote_key = Some(key.to_owned());
        let guard = if self.content_write {
            drop(guard);
            None
        } else {
            Some(guard)
        };
        let result = effect(&files);
        drop(guard);
        if result.is_err() && !matches!(result, Err(FileError::RecoveryRequired(_))) {
            self.db.write_with(|tx| {
                tx.connection().execute("DELETE FROM management_file_outcomes WHERE server_id=? AND principal_id=? AND device_id=? AND request_key=? AND outcome_json IS NULL",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),key])?;
                Ok::<_,FileError>(())
            }).map_err(FileError::from)?;
        }
        result
    }
    fn purge_remote_outcomes(&self) -> Result<()> {
        let cutoff = self
            .db
            .clock()
            .millis()
            .saturating_sub(OUTCOME_RETENTION_MS);
        self.db.write_with(|tx| {
            tx.connection().execute("DELETE FROM management_file_outcomes WHERE rowid IN (SELECT rowid FROM management_file_outcomes WHERE server_id=? AND outcome_json IS NOT NULL AND completed_at<? ORDER BY completed_at,rowid LIMIT ?)",params![self.db.server_id(),cutoff,OUTCOME_PURGE_BATCH])?;
            Ok::<_,FileError>(())
        }).map_err(FileError::from)
    }
    fn request_admission(&self) -> Result<std::sync::MutexGuard<'static, ()>> {
        lock_admission(&ADMISSION, self.remote_deadline)
    }
    pub(crate) fn remote_effect_admission(
        &self,
    ) -> Result<Option<std::sync::MutexGuard<'static, ()>>> {
        if self.content_write {
            Ok(Some(self.request_admission()?))
        } else {
            Ok(None)
        }
    }
    pub(super) fn remote_authorize(
        &self,
        c: &Connection,
        subject: &Subject,
        authority: &[(String, String)],
    ) -> Result<()> {
        let policy = identity::policy_in(
            c,
            self.db.server_id(),
            self.db.clock().millis(),
            Some(subject),
        )?;
        for (cap, path) in authority {
            let capability = Capability::from_id(cap).ok_or(FileError::Unavailable)?;
            let resource = Resource::parse(path).map_err(|_| FileError::InvalidPath)?;
            if capability.scoped() {
                RelativePath::parse(path)?;
            }
            if capability.scoped() {
                super::authorize(&policy, capability, &resource)?;
                let relative = RelativePath::parse(path)?;
                match self.root.resolve(Some(&relative)) {
                    Ok(checked) => super::authorize(&policy, capability, &checked.resource)?,
                    Err(FileError::NotFound) => {}
                    Err(error) => return Err(error),
                }
            } else if !policy.check(capability, None).allowed() {
                return Err(
                    crate::ControlError::forbidden(identity::DenyReason::NotGranted).into(),
                );
            }
        }
        Ok(())
    }
    pub(crate) fn check_global_authority(
        &self,
        subject: &Subject,
        capability: Capability,
    ) -> Result<()> {
        self.db
            .read_with(|snapshot| {
                self.remote_authorize(
                    snapshot.connection(),
                    subject,
                    &[(capability.id().into(), String::new())],
                )
            })
            .map_err(FileError::from)
    }
    pub(super) fn check_remote_completion_authority(
        &self,
        c: &Connection,
        subject: &Subject,
    ) -> Result<()> {
        if let Some(key) = &self.remote_key {
            let text: String = c.query_row("SELECT authority_json FROM management_file_outcomes WHERE server_id=? AND principal_id=? AND device_id=? AND request_key=? AND outcome_json IS NULL",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),key],|r| r.get(0))?;
            let authority: Vec<(String, String)> =
                serde_json::from_str(&text).map_err(|_| FileError::Unavailable)?;
            self.remote_authorize(c, subject, &authority)?;
        }
        Ok(())
    }
    pub(crate) fn database(&self) -> &vesta_state::StateDatabase {
        &self.db
    }
    pub(crate) fn for_remote(&self) -> Self {
        let mut files = self.clone();
        files.root = files.root.clone().remote(false);
        files
    }
    pub(crate) fn for_content(&self, deadline: std::time::Instant) -> Self {
        let mut files = self.clone();
        files.content_write = true;
        files.remote_deadline = Some(deadline);
        files.root = files.root.clone().remote(false);
        files
    }
    pub(crate) fn for_properties(
        &self,
        approval: Option<crate::operations::PropertiesApproval>,
    ) -> Self {
        let mut files = self.clone();
        files.properties_write = true;
        files.root = files.root.clone().remote(true);
        files.properties_approval = approval;
        files
    }
    pub(super) fn before_remote_write(&self, c: &Connection, subject: &Subject) -> Result<()> {
        if let Some(guard) = &self.whitelist_guard {
            guard.check()?;
        }
        if self
            .remote_deadline
            .is_some_and(|d| std::time::Instant::now() >= d)
        {
            return Err(FileError::Unavailable);
        }
        self.check_remote_completion_authority(c, subject)?;
        if let Some(approval) = &self.properties_approval {
            crate::operations::accept_properties_confirmation(
                c,
                self.db.server_id(),
                self.db.clock().millis(),
                subject,
                approval,
            )?;
            use vesta_state::audit::{AuditActor, AuditRecord};
            self.audit.append(
                c,
                &AuditRecord::new(
                    AuditActor::new(
                        subject.principal.as_str().into(),
                        subject.device.as_str().into(),
                        "config".into(),
                    )?,
                    "confirmation.accept".into(),
                    "server-properties".into(),
                    "accepted".into(),
                    None,
                    [],
                )?,
            )?;
        }
        Ok(())
    }
    pub(crate) fn remote_content_authority(
        &self,
        subject: &Subject,
        path: &RelativePath,
    ) -> Result<()> {
        if let Some(key) = &self.remote_key {
            self.db.write_with(|tx| {
                let c = tx.connection();
                let text: String = c.query_row("SELECT authority_json FROM management_file_outcomes WHERE server_id=? AND principal_id=? AND device_id=? AND request_key=? AND outcome_json IS NULL",params![self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),key],|r| r.get(0))?;
                let mut authority: Vec<(String,String)> = serde_json::from_str(&text).map_err(|_| FileError::Unavailable)?;
                authority.push((Capability::FilesCreate.id().into(),path.as_str().into()));
                self.remote_authorize(c,subject,&authority)?;
                c.execute("UPDATE management_file_outcomes SET authority_json=? WHERE server_id=? AND principal_id=? AND device_id=? AND request_key=?",params![serde_json::to_string(&authority).map_err(|_| FileError::Unavailable)?,self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),key])?;
                Ok::<_,FileError>(())
            }).map_err(FileError::from)?;
        }
        Ok(())
    }
    pub(super) fn remote_complete(
        &self,
        c: &Connection,
        subject: &Subject,
        outcome: Value,
    ) -> Result<()> {
        if let Some(key) = &self.remote_key {
            let changed = c.execute("UPDATE management_file_outcomes SET outcome_json=?,completed_at=? WHERE server_id=? AND principal_id=? AND device_id=? AND request_key=? AND outcome_json IS NULL",params![outcome.to_string(),self.db.clock().millis(),self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),key])?;
            if changed != 1 {
                return Err(FileError::Unavailable);
            }
        }
        Ok(())
    }
    pub(super) fn remote_upload_complete(
        &self,
        c: &Connection,
        subject: &Subject,
        action: &str,
        outcome: Value,
    ) -> Result<()> {
        if self.remote_key.is_some() {
            use vesta_state::audit::{AuditActor, AuditRecord};
            let record = AuditRecord::new(
                AuditActor::new(
                    subject.principal.as_str().into(),
                    subject.device.as_str().into(),
                    "files".into(),
                )?,
                action.into(),
                "upload".into(),
                "success".into(),
                None,
                [],
            )?;
            self.audit.append(c, &record)?;
        }
        self.remote_complete(c, subject, outcome)
    }
}
fn change(value: super::FileChange) -> Value {
    json!({"outcome":"succeeded","sha256":value.sha256,"size":value.size.to_string(),"trash_id":value.trash})
}
