//! Durable provenance for every game-tree mutation. Never replay a native effect on startup.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Intent {
    pub(super) to: Option<String>,
    pub(super) previous: Option<String>,
    pub(super) effect: Option<String>,
    pub(super) stage: Option<String>,
    #[serde(default)]
    pub(super) stage_cleaned: bool,
    pub(super) trash: Option<String>,
    pub(super) upload: Option<String>,
    pub(super) size: u64,
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
            if kind == "mkdir" {
                self.authorize_destination(c, subject, path, capability)?;
            } else {
                self.authorize(c, subject, path, capability)?;
            }
            let target = self.parent(path)?.join(leaf(path));
            require_hash(&target, intent.previous.as_deref())?;
            if let Some(to) = &intent.to {
                let to = RelativePath::parse(to)?;
                self.authorize_destination(c, subject, &to, capability)?;
                require_absent(&self.parent(&to)?.join(leaf(&to)))?;
            }
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
        self.audit.append(c, &record)?;
        let count = c.execute("UPDATE file_intents SET status='complete' WHERE server_id=? AND id=? AND status='pending'", params![self.db.server_id(), id])?;
        if count != 1 {
            return Err(FileError::Unavailable);
        }
        Ok(())
    }

    pub(super) fn failed_intent<T>(&self, id: &str, result: &Result<T>) -> Result<()> {
        if result.is_ok() {
            return Ok(());
        }
        self.db.write_with(|tx| {
            tx.connection().execute("UPDATE file_intents SET status=? WHERE server_id=? AND id=? AND status='pending'", params![if matches!(result, Err(FileError::RecoveryRequired(_))) { "recovery-required" } else { "aborted" },self.db.server_id(),id])?;
            Ok::<_, FileError>(())
        }).map_err(FileError::from)
    }

    pub(super) fn reconcile_intents(&self) -> Result<usize> {
        let count = self.db.write_with(|tx| {
            let c = tx.connection();
            let mut query = c.prepare("SELECT id,principal_id,device_id,kind,path,details_json FROM file_intents WHERE server_id=? AND status='pending' ORDER BY created_at,id")?;
            let rows = query.query_map([self.db.server_id()], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            for (id, principal, device, kind, text, json) in &rows {
                let intent: Intent = serde_json::from_str(json).map_err(|_| FileError::Unavailable)?;
                let path = RelativePath::parse(text)?;
                let subject = Subject { principal: crate::identity::PrincipalId::parse(principal)?, device: crate::identity::DeviceId::parse(device)? };
                let target = self.parent(&path).map(|p| p.join(leaf(&path)));
                let valid_target = self.checked_existing_path(&path, kind == "mkdir").is_ok();
                let copy_valid = if let Some(trash) = &intent.trash {
                    if let Some(entry) = load_entry(c, self.db.server_id(), trash)? { read_trash(&self.trash_dir()?, &trash_name(trash)?, &entry).is_ok() } else { false }
                } else { true };
                // A replacement consumes its stage in one rename. If identical
                // old/new hashes cannot distinguish the target, an intact private
                // stage still proves the replace has not happened.
                let unconsumed_stage = kind == "write" && intent.effect == intent.previous && intent.stage.as_deref().is_some_and(|name| {
                    let valid_name = name.strip_prefix(STAGE_PREFIX).and_then(|s| s.strip_suffix(".tmp")).is_some_and(|id| trash_name(id).is_ok());
                    valid_name && target.as_ref().ok().and_then(|p| p.parent()).is_some_and(|parent| require_hash(&parent.join(name), intent.effect.as_deref()).is_ok())
                });
                let (observed, unchanged) = if let Ok(target) = &target {
                    if kind == "rename" {
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
        self.cleanup_intent_stages()?;
        Ok(count)
    }

    fn cleanup_intent_stages(&self) -> Result<()> {
        let rows = self.db.read_with(|snapshot| {
            let mut query = snapshot.connection().prepare("SELECT id,path,details_json,principal_id,device_id,kind FROM file_intents WHERE server_id=? AND status IN ('complete','aborted') AND json_extract(details_json,'$.stage') IS NOT NULL AND COALESCE(json_extract(details_json,'$.stage_cleaned'),0)=0")?;
            let rows = query.query_map([self.db.server_id()], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_,FileError>(rows)
        }).map_err(FileError::from)?;
        for (id, text, json, principal, device, kind) in rows {
            let mut intent: Intent =
                serde_json::from_str(&json).map_err(|_| FileError::Unavailable)?;
            let path = RelativePath::parse(&text)?;
            let cleanup = (|| {
                let leaf_name = intent.stage.as_deref().ok_or(FileError::Unavailable)?;
                let stage_id = leaf_name
                    .strip_prefix(STAGE_PREFIX)
                    .and_then(|s| s.strip_suffix(".tmp"))
                    .ok_or(FileError::InvalidPath)?;
                trash_name(stage_id)?;
                let stage = self.parent(&path)?.join(leaf_name);
                // Random name and content hash identify owned cleanup. Never sweep
                // uncertain, linked or merely prefix-matching files.
                if require_absent(&stage).is_err() {
                    require_hash(&stage, intent.effect.as_deref())?;
                    native::remove_private_stage(&stage)?;
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
            intent.stage_cleaned = true;
            let json = serde_json::to_string(&intent).map_err(|_| FileError::Unavailable)?;
            self.db.write_with(|tx| {
                tx.connection().execute("UPDATE file_intents SET details_json=? WHERE server_id=? AND id=? AND status IN ('complete','aborted')", params![json,self.db.server_id(),id])?;
                Ok::<_,FileError>(())
            }).map_err(FileError::from)?;
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
pub(super) fn interrupt_at(point: &'static str) {
    CRASH_AT.with(|at| at.set(Some(point)));
}
