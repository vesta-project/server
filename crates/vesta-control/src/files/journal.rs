//! Durable provenance for every game-tree mutation. Never replay a native effect on startup.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Intent {
    #[serde(default)]
    pub(super) directory: bool,
    /// Private disk payloads are admitted against actual free space rather than RAM quotas.
    #[serde(default)]
    pub(super) disk_backed: bool,
    pub(super) to: Option<String>,
    pub(super) previous: Option<String>,
    pub(super) effect: Option<String>,
    pub(super) stage: Option<String>,
    #[serde(default)]
    pub(super) compensation_stage: Option<String>,
    #[serde(default)]
    pub(super) stage_cleaned: bool,
    pub(super) trash: Option<String>,
    pub(super) upload: Option<String>,
    pub(super) size: u64,
    /// Reservation includes temporary compensation bytes. Old v11 rows charge `size`.
    #[serde(default)]
    pub(super) staged_bytes: Option<u64>,
}

impl FileBrowser {
    pub(super) fn begin_intent(
        &self,
        subject: &Subject,
        kind: &str,
        path: &RelativePath,
        intent: &Intent,
    ) -> Result<String> {
        let id = random_id()?;
        // Construct the exact bounded audit representation before any game-tree effect.
        intent_record(subject, kind, path, intent, &id, "success", false)?;
        let details = serde_json::to_string(intent).map_err(|_| FileError::Unavailable)?;
        self.db.write_with(|tx| {
            let c = tx.connection();
            let capability = match kind {
                "mkdir" => Capability::FilesMkdir,
                "rename" => Capability::FilesRename,
                "delete" => Capability::FilesDelete,
                "write" if intent.previous.is_some() => Capability::FilesOverwrite,
                _ => Capability::FilesCreate,
            };
            if kind == "mkdir" || intent.directory {
                self.authorize_destination(c, subject, path, capability)?;
            } else {
                self.authorize(c, subject, path, capability)?;
            }
            let target = self.parent(path)?.join(leaf(path));
            if intent.directory {
                let checked = self.root.resolve(Some(path))?;
                if !checked.metadata.is_dir() { return Err(FileError::NotDirectory); }
            } else { require_hash(&target, intent.previous.as_deref())?; }
            if let Some(to) = &intent.to {
                let to = RelativePath::parse(to)?;
                self.authorize_destination(c, subject, &to, capability)?;
                require_absent(&self.parent(&to)?.join(leaf(&to)))?;
            }
            self.require_intent_capacity(c, intent)?;
            c.execute("INSERT INTO file_intents (id,server_id,principal_id,device_id,kind,path,details_json,status,created_at) VALUES (?,?,?,?,?,?,?,'pending',?)", params![id,self.db.server_id(),subject.principal.as_str(),subject.device.as_str(),kind,path.as_str(),details,self.db.clock().millis()])?;
            Ok(id.clone())
        }).map_err(FileError::from)
    }

    pub(super) fn finish_intent(&self, c: &Connection, id: &str) -> Result<()> {
        let (principal, device, kind, text, json): (String, String, String, String, String) = c.query_row(
            "SELECT principal_id,device_id,kind,path,details_json FROM file_intents WHERE server_id=? AND id=? AND status='pending'",
            params![self.db.server_id(), id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )?;
        let subject = Subject {
            principal: crate::identity::PrincipalId::parse(&principal)?,
            device: crate::identity::DeviceId::parse(&device)?,
        };
        let intent: Intent = serde_json::from_str(&json).map_err(|_| FileError::Unavailable)?;
        let record = intent_record(
            &subject,
            &kind,
            &RelativePath::parse(&text)?,
            &intent,
            id,
            "success",
            false,
        )?;
        self.check_remote_completion_authority(c, &subject)?;
        // This is the final proof boundary, after the filesystem effect and
        // before either the successful receipt or journal retirement commits.
        if let Some(guard) = &self.stopped_guard {
            guard
                .check()
                .map_err(|e| FileError::RecoveryRequired(Box::new(e)))?;
        }
        self.audit.append(c, &record)?;
        if self.stopped_guard.is_some() && text == "whitelist.json" {
            self.audit.append(
                c,
                &AuditRecord::new(
                    AuditActor::new(
                        subject.principal.as_str().into(),
                        subject.device.as_str().into(),
                        "players".into(),
                    )?,
                    "players.whitelist.offline".into(),
                    "whitelist".into(),
                    "success".into(),
                    None,
                    [],
                )?,
            )?;
        }
        let mut outcome = match kind.as_str() {
            "mkdir" => serde_json::json!({"outcome":"succeeded"}),
            "delete" => serde_json::json!({"outcome":"succeeded","trash_id":intent.trash}),
            _ => {
                serde_json::json!({"outcome":"succeeded","sha256":intent.effect,"size":intent.size.to_string(),"trash_id":if kind == "write" { intent.trash } else { None }})
            }
        };
        if self.properties_write {
            outcome["restart_needed"] = serde_json::json!(true);
            self.audit.append(
                c,
                &AuditRecord::new(
                    AuditActor::new(
                        subject.principal.as_str().into(),
                        subject.device.as_str().into(),
                        "config".into(),
                    )?,
                    "config.server_properties".into(),
                    "server-properties".into(),
                    "success".into(),
                    None,
                    [],
                )?,
            )?;
        }
        if self.content_write {
            self.audit.append(
                c,
                &AuditRecord::new(
                    AuditActor::new(
                        subject.principal.as_str().into(),
                        subject.device.as_str().into(),
                        "content".into(),
                    )?,
                    "content.install".into(),
                    "provider-content".into(),
                    "success".into(),
                    None,
                    [],
                )?,
            )?;
        }
        self.remote_complete(c, &subject, self.remote_outcome.clone().unwrap_or(outcome))?;

        let count = c.execute("UPDATE file_intents SET status='complete' WHERE server_id=? AND id=? AND status='pending'", params![self.db.server_id(), id])?;
        if count != 1 {
            return Err(FileError::Unavailable);
        }
        Ok(())
    }

    /// A compensation stage is also durable provenance: record its random name
    /// before creation so interruption cannot leave unaccounted game-tree bytes.
    pub(super) fn record_compensation_stage(
        &self,
        id: &str,
        stage: &str,
        bytes: u64,
    ) -> Result<()> {
        self.db.write_with(|tx| {
            let c = tx.connection();
            let json: String = c.query_row(
                "SELECT details_json FROM file_intents WHERE server_id=? AND id=? AND status='pending'",
                params![self.db.server_id(), id], |r| r.get(0),
            )?;
            let mut intent: Intent = serde_json::from_str(&json).map_err(|_| FileError::Unavailable)?;
            if intent.staged_bytes.unwrap_or(intent.size) < bytes || intent.compensation_stage.is_some() {
                return Err(FileError::Unavailable);
            }
            intent.compensation_stage = Some(stage.to_owned());
            c.execute("UPDATE file_intents SET details_json=? WHERE server_id=? AND id=? AND status='pending'",
                params![serde_json::to_string(&intent).map_err(|_| FileError::Unavailable)?,self.db.server_id(),id])?;
            Ok::<_, FileError>(())
        }).map_err(FileError::from)
    }

    /// Count all retained rows, including terminal rows whose cleanup/audit failed.
    /// This is deliberately stricter than counting only pending/recovery rows: a
    /// cleanup failure must never let more retained stages evade admission.
    fn require_intent_capacity(&self, c: &Connection, intent: &Intent) -> Result<()> {
        let (count, bytes): (i64, i64) = c.query_row(
            "SELECT count(*), COALESCE(sum(CASE WHEN COALESCE(json_extract(details_json,'$.stage_cleaned'),0)=0 THEN COALESCE(json_extract(details_json,'$.staged_bytes'),CASE WHEN json_extract(details_json,'$.stage') IS NOT NULL THEN json_extract(details_json,'$.size') ELSE 0 END,0) ELSE 0 END),0) FROM file_intents WHERE server_id=?",
            [self.db.server_id()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let incoming = intent.staged_bytes.unwrap_or_else(|| {
            if intent.stage.is_some() {
                intent.size
            } else {
                0
            }
        });
        if count >= MAX_FILE_INTENTS as i64
            || !intent.disk_backed
                && u64::try_from(bytes)
                    .map_err(|_| FileError::Unavailable)?
                    .saturating_add(incoming)
                    > MAX_INTENT_STAGED_BYTES
        {
            return Err(FileError::IntentsFull);
        }
        Ok(())
    }

    pub(super) fn failed_intent<T>(&self, id: &str, result: &Result<T>) -> Result<()> {
        if result.is_err() {
            self.db.write_with(|tx| {
                tx.connection().execute("UPDATE file_intents SET status=? WHERE server_id=? AND id=? AND status='pending'", params![if matches!(result, Err(FileError::RecoveryRequired(_))) { "recovery-required" } else { "aborted" },self.db.server_id(),id])?;
                Ok::<_, FileError>(())
            }).map_err(FileError::from)?;
        }
        // Never misreport a committed success (or replace its primary failure) if
        // maintenance fails. The durable row keeps its reservation for retry.
        let _ = self.cleanup_intent_stages();
        Ok(())
    }

    pub(super) fn reconcile_intents(&self) -> Result<usize> {
        let mut total = 0;
        for _ in 0..MAX_RECONCILE_BATCHES {
            let count = self.db.write_with(|tx| {
            let c = tx.connection();
            let mut query = c.prepare("SELECT id,principal_id,device_id,kind,path,details_json FROM file_intents WHERE server_id=? AND status='pending' ORDER BY created_at,id LIMIT ?")?;
            let rows = query.query_map(params![self.db.server_id(), RECONCILE_BATCH_SIZE as i64], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            for (id, principal, device, kind, text, json) in &rows {
                let intent: Intent = serde_json::from_str(json).map_err(|_| FileError::Unavailable)?;
                let path = RelativePath::parse(text)?;
                let subject = Subject { principal: crate::identity::PrincipalId::parse(principal)?, device: crate::identity::DeviceId::parse(device)? };
                let target = self.parent(&path).map(|p| p.join(leaf(&path)));
                let valid_target = self.checked_existing_path(&path, kind == "mkdir" || intent.directory).is_ok();
                let copy_valid = if let Some(trash) = &intent.trash {
                    if let Some(entry) = load_entry(c, self.db.server_id(), trash)? { open_trash(&self.trash_dir()?, &trash_name(trash)?, &entry).and_then(hash_file).is_ok_and(|hash|hash==entry.sha256) } else { false }
                } else { true };
                // A replacement consumes its stage in one rename. If identical
                // old/new hashes cannot distinguish the target, an intact private
                // stage still proves the replace has not happened.
                let unconsumed_stage = kind == "write" && intent.effect == intent.previous && intent.stage.as_deref().is_some_and(|name| {
                    let valid_name = name.strip_prefix(STAGE_PREFIX).and_then(|s| s.strip_suffix(".tmp")).is_some_and(|id| trash_name(id).is_ok());
                    valid_name && target.as_ref().ok().and_then(|p| p.parent()).is_some_and(|parent| require_hash(&parent.join(name), intent.effect.as_deref()).is_ok())
                });
                let (observed, unchanged) = if let Ok(target) = &target {
                    if intent.directory {
                        // Directory contents/inodes are not a durable ownership
                        // proof. Never replay or infer success after process loss.
                        (false, false)
                    } else if kind == "rename" {
                        let destination = intent.to.as_deref().map(RelativePath::parse).transpose()?.ok_or(FileError::Unavailable)?;
                        let to = self.parent(&destination).map(|p| p.join(leaf(&destination)));
                        (valid_target && self.checked_existing_resource(&destination).is_ok() && require_absent(target).is_ok() && to.as_ref().is_ok_and(|to| require_hash(to, intent.effect.as_deref()).is_ok()),
                         valid_target && require_hash(target, intent.previous.as_deref()).is_ok() && to.as_ref().is_ok_and(|to| require_absent(to).is_ok()))
                    } else if kind == "mkdir" {
                        // An arbitrary directory has no content identity proving our ownership.
                        // Preserve it and explicitly require recovery rather than claim success.
                        (false, valid_target && require_absent(target).is_ok())
                    } else {
                        (valid_target && copy_valid && intent.effect != intent.previous && require_hash(target, intent.effect.as_deref()).is_ok(),
                         valid_target && copy_valid && (intent.effect != intent.previous || unconsumed_stage) && require_hash(target, intent.previous.as_deref()).is_ok())
                    }
                } else { (false, false) };
                let status = if observed { "complete" } else if unchanged { "aborted" } else { "recovery-required" };
                if let Some(trash) = &intent.trash {
                    c.execute("UPDATE file_trash SET status=? WHERE server_id=? AND id=?", params![if observed && kind == "restore" { "restored" } else if observed { "complete" } else if unchanged && kind != "restore" { "restored" } else if unchanged { "complete" } else { "recovery-required" },self.db.server_id(),trash])?;
                }
                if observed && let Some(upload) = &intent.upload {
                    c.execute("DELETE FROM file_uploads WHERE server_id=? AND id=? AND principal_id=? AND device_id=?", params![self.db.server_id(),upload,principal,device])?;
                }
                let record = intent_record(&subject, kind, &path, &intent, id, if observed { "success" } else { status }, true)?;
                self.audit.append(c, &record)?;
                c.execute("UPDATE file_intents SET status=? WHERE server_id=? AND id=?", params![status,self.db.server_id(),id])?;
            }
            Ok(rows.len())
        }).map_err(FileError::from)?;
            total += count;
            if count < RECONCILE_BATCH_SIZE {
                break;
            }
        }
        self.cleanup_intent_stages()?;
        Ok(total)
    }

    fn cleanup_intent_stages(&self) -> Result<()> {
        for _ in 0..MAX_RECONCILE_BATCHES {
            let rows = self.db.read_with(|snapshot| {
            let mut query = snapshot.connection().prepare("SELECT id,path,details_json,principal_id,device_id,kind,status FROM file_intents WHERE server_id=? AND status IN ('complete','aborted') ORDER BY created_at,id LIMIT ?")?;
            let rows = query.query_map(params![self.db.server_id(), RECONCILE_BATCH_SIZE as i64], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_,FileError>(rows)
        }).map_err(FileError::from)?;
            let count = rows.len();
            for (id, text, json, principal, device, kind, status) in rows {
                let intent: Intent =
                    serde_json::from_str(&json).map_err(|_| FileError::Unavailable)?;
                let path = RelativePath::parse(&text)?;
                let cleanup = (|| {
                    if intent.stage_cleaned {
                        return Ok(());
                    }
                    for (leaf_name, expected) in [
                        (intent.stage.as_deref(), intent.effect.as_deref()),
                        (
                            intent.compensation_stage.as_deref(),
                            intent.previous.as_deref(),
                        ),
                    ] {
                        let Some(leaf_name) = leaf_name else {
                            continue;
                        };
                        let stage_id = leaf_name
                            .strip_prefix(STAGE_PREFIX)
                            .and_then(|s| s.strip_suffix(".tmp"))
                            .ok_or(FileError::InvalidPath)?;
                        trash_name(stage_id)?;
                        let stage = self.parent(&path)?.join(leaf_name);
                        // Random name and content hash identify owned cleanup. Never
                        // sweep uncertain, linked or merely prefix-matching files.
                        if require_absent(&stage).is_err() {
                            require_hash(&stage, expected)?;
                            native::remove_private_stage(&stage)?;
                        }
                    }
                    Ok::<_, FileError>(())
                })();
                if cleanup.is_err() {
                    let subject = Subject {
                        principal: crate::identity::PrincipalId::parse(&principal)?,
                        device: crate::identity::DeviceId::parse(&device)?,
                    };
                    self.db.write_with(|tx| {
                    let c = tx.connection();
                    let record = intent_record(&subject, &kind, &path, &intent, &id, "recovery-required", true)?;
                    self.audit.append(c, &record)?;
                    c.execute("UPDATE file_intents SET status='recovery-required' WHERE server_id=? AND id=?", params![self.db.server_id(),id])?;
                    Ok::<_,FileError>(())
                }).map_err(FileError::from)?;
                    continue;
                }
                self.db.write_with(|tx| {
                let c = tx.connection();
                // Success/reconciliation already carry the intent id. A normally
                // compensated abort has no success audit, so record its outcome
                // before retiring its only durable provenance.
                let audited: bool = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM audit_events WHERE operation_id=?)",
                    [&id], |r| r.get(0),
                )?;
                if status == "aborted" && !audited {
                    let subject = Subject {
                        principal: crate::identity::PrincipalId::parse(&principal)?,
                        device: crate::identity::DeviceId::parse(&device)?,
                    };
                    self.audit.append(c, &intent_record(&subject, &kind, &path, &intent, &id, "aborted", false)?)?;
                }
                c.execute("DELETE FROM file_intents WHERE server_id=? AND id=? AND status IN ('complete','aborted')", params![self.db.server_id(),id])?;
                Ok::<_,FileError>(())
            }).map_err(FileError::from)?;
            }
            if count < RECONCILE_BATCH_SIZE {
                break;
            }
        }
        Ok(())
    }
}

fn intent_record(
    subject: &Subject,
    kind: &str,
    path: &RelativePath,
    intent: &Intent,
    id: &str,
    outcome: &str,
    recovered: bool,
) -> Result<AuditRecord> {
    let mut fields = path_fields("path", path.as_str());
    if let Some(to) = &intent.to {
        fields.extend(path_fields("to", RelativePath::parse(to)?.as_str()));
    }
    fields.extend([
        (
            "sha256".into(),
            intent.effect.clone().or_else(|| intent.previous.clone()),
        ),
        (
            "size".into(),
            matches!(kind, "write" | "restore").then(|| intent.size.to_string()),
        ),
        ("trash".into(), intent.trash.clone()),
        ("reconciled".into(), Some(recovered.to_string())),
    ]);
    Ok(AuditRecord::new(
        AuditActor::new(
            subject.principal.as_str().into(),
            subject.device.as_str().into(),
            SOURCE.into(),
        )?,
        format!("files.{kind}"),
        TARGET.into(),
        outcome.into(),
        Some(id.into()),
        fields,
    )?)
}

// A test-only crash boundary returns without compensation, emulating process loss
// while allowing the fixture to reopen the database without poisoning admission.
#[cfg(test)]
thread_local! {
    static CRASH_AT: Cell<Option<&'static str>> = const { Cell::new(None) };
    static CRASHED: Cell<bool> = const { Cell::new(false) };
    static AFTER_WRITE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_COMPENSATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn crash_point(point: &str) -> Result<()> {
    #[cfg(test)]
    if CRASH_AT.with(|at| at.get() == Some(point)) {
        CRASH_AT.with(|at| at.set(None));
        CRASHED.with(|flag| flag.set(true));
        return Err(FileError::RecoveryRequired(Box::new(
            FileError::Unavailable,
        )));
    }
    let _ = point;
    Ok(())
}
pub(super) fn interrupted() -> bool {
    #[cfg(test)]
    {
        CRASHED.with(|flag| flag.replace(false))
    }
    #[cfg(not(test))]
    {
        false
    }
}

#[cfg(test)]
pub(super) fn after_write_for_test(hook: impl FnOnce() + 'static) {
    AFTER_WRITE.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}
#[cfg(test)]
pub(super) fn run_after_write_for_test() {
    AFTER_WRITE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}
#[cfg(test)]
pub(super) fn before_compensation_for_test(hook: impl FnOnce() + 'static) {
    BEFORE_COMPENSATION.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}
#[cfg(test)]
pub(super) fn run_before_compensation_for_test() {
    BEFORE_COMPENSATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}
#[cfg(test)]
pub(super) fn interrupt_at(point: &'static str) {
    CRASH_AT.with(|at| at.set(Some(point)));
}
