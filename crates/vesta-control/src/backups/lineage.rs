use super::*;
use sha2::{Digest, Sha256};
use vesta_state::audit::{AuditActor, AuditLog, AuditRecord};

fn engine_error(e: EngineError) -> BackupError {
    match e {
        EngineError::Missing => BackupError::UnknownSnapshot,
        EngineError::Invalid => BackupError::Invalid,
        _ => BackupError::Unavailable,
    }
}
impl Backups {
    pub fn history(&self) -> Result<engine::BackupHistory> {
        let _guard = self
            .inner
            .repository
            .try_lock()
            .map_err(|_| BackupError::Busy)?;
        self.inner.engine.history().map_err(engine_error)
    }
    pub fn point_update(
        &self,
        subject: &Subject,
        id: &str,
        update: &engine::PointUpdate,
        key: &str,
    ) -> Result<engine::BackupPoint> {
        if key.is_empty() || key.len() > 200 {
            return Err(BackupError::Invalid);
        }
        let inner = &self.inner;
        let _guard = inner.repository.try_lock().map_err(|_| BackupError::Busy)?;
        let db = &inner.db;
        let key_hash = Sha256::digest(
            serde_json::to_vec(&(
                "backup-point-v1",
                db.server_id(),
                subject.principal.as_str(),
                subject.device.as_str(),
                key,
            ))
            .map_err(|_| BackupError::Invalid)?,
        );
        let intent_hash =
            Sha256::digest(serde_json::to_vec(&(id, update)).map_err(|_| BackupError::Invalid)?);
        let audit = AuditLog::new(db.clone())?;
        let now = db.clock().millis();
        db.write(|c| {
            if !allowed(c, db, subject, Capability::BackupsRestore)? { return Ok(Err(BackupError::Forbidden)); }
            let prior: Option<(Vec<u8>, String)> = c.query_row("SELECT intent_hash,result_json FROM management_permission_receipts WHERE key_hash=?", [key_hash.as_slice()], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            if let Some((hash, json)) = prior {
                if hash != intent_hash.as_slice() { return Ok(Err(BackupError::IdempotencyConflict)); }
                return Ok(serde_json::from_str(&json).map_err(|_| BackupError::State));
            }
            c.execute("DELETE FROM management_permission_receipts WHERE completed_at<?", [now.saturating_sub(90 * 24 * 60 * 60 * 1000)])?;
            let count: i64 = c.query_row("SELECT count(*) FROM management_permission_receipts", [], |r| r.get(0))?;
            if count >= 4096 { return Ok(Err(BackupError::Busy)); }
            let point = match inner.engine.point_update(id, update) { Ok(p) => p, Err(e) => return Ok(Err(engine_error(e))) };
            let json = serde_json::to_string(&point).map_err(|_| vesta_state::StateError::new("backup_receipt_invalid"))?;
            audit.append(c, &AuditRecord::new(AuditActor::new(subject.principal.as_str().into(), subject.device.as_str().into(), "backups".into())?, "backups.point.update".into(), id.into(), "accepted".into(), None, [])?)?;
            c.execute("INSERT INTO management_permission_receipts(key_hash,intent_hash,result_json,completed_at) VALUES(?,?,?,?)", params![key_hash.as_slice(),intent_hash.as_slice(),json,now])?;
            Ok(Ok(point))
        })?
    }
}
