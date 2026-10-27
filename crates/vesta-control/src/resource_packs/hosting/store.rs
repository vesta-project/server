use super::*;
use crate::identity::{self, BuiltinRole, Capability, Subject};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use vesta_state::audit::{AuditActor, AuditLog, AuditRecord};

pub(super) fn encode<T: Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(|_| Error::Unavailable)
}
pub(super) fn decode<T: DeserializeOwned>(v: &str) -> Result<T> {
    serde_json::from_str(v).map_err(|_| Error::Unavailable)
}
pub(super) fn authorize(c: &Connection, db: &StateDatabase, actor: &Subject) -> Result<()> {
    let policy = identity::policy_in(c, db.server_id(), db.clock().millis(), Some(actor))
        .map_err(|_| Error::Unavailable)?;
    if policy.holds_builtin(BuiltinRole::Owner)
        && policy.check(Capability::PackEdit, None).allowed()
        && policy.check(Capability::ConfigEdit, None).allowed()
    {
        Ok(())
    } else {
        Err(Error::Forbidden)
    }
}
pub(super) fn draft(c: &Connection) -> Result<Draft> {
    let (rev, id, text): (i64, String, Option<String>) = c.query_row(
        "SELECT revision,pack_id,settings FROM hosted_pack_state WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(Draft {
        revision: rev.to_string(),
        pack_id: id,
        settings: text.map(|s| decode(&s)).transpose()?,
    })
}
pub(super) fn release(c: &Connection, id: &str) -> Result<Release> {
    let body: Option<String> = c
        .query_row(
            "SELECT body FROM hosted_pack_releases WHERE id=?",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    decode(&body.ok_or(Error::NotFound)?)
}
pub(super) fn jobs(c: &Connection, limit: usize) -> Result<Vec<Job>> {
    let mut query = c.prepare("SELECT body FROM hosted_pack_jobs ORDER BY rowid DESC LIMIT ?")?;
    query
        .query_map([limit.min(64) as i64], |r| r.get::<_, String>(0))?
        .map(|r| decode(&r?))
        .collect()
}
pub(super) fn put_job(c: &Connection, job: &Job) -> Result<()> {
    c.execute("INSERT INTO hosted_pack_jobs(id,body) VALUES(?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",params![job.id,encode(job)?])?;
    Ok(())
}
pub(super) fn clear_pending(c: &Connection) -> Result<()> {
    c.execute(
        "UPDATE hosted_pack_state SET pending=NULL,recovery=0 WHERE singleton=1",
        [],
    )?;
    Ok(())
}
pub(super) fn audit(c: &Connection, log: &AuditLog, actor: &Subject, action: &str) -> Result<()> {
    log.append(
        c,
        &AuditRecord::new(
            AuditActor::new(
                actor.principal.as_str().into(),
                actor.device.as_str().into(),
                "resource-packs".into(),
            )?,
            action.into(),
            "resource-packs".into(),
            "success".into(),
            None,
            [],
        )?,
    )?;
    Ok(())
}
impl Handle {
    pub(super) fn replay<T: Serialize, R: DeserializeOwned>(
        &self,
        actor: &Subject,
        key: &str,
        input: &T,
    ) -> Result<Option<R>> {
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Invalid);
        }
        let digest = hash(encode(input)?.as_bytes());
        self.inner
            .db
            .read_with(|tx| -> Result<_> {
                authorize(tx.connection(), &self.inner.db, actor)?;
                receipt(tx.connection(), actor, key, &digest)
            })
            .map_err(Into::into)
    }
    pub(super) fn mutate<T: Serialize, R: Serialize + DeserializeOwned>(
        &self,
        actor: &Subject,
        key: &str,
        input: &T,
        work: impl FnOnce(&Connection) -> Result<R>,
    ) -> Result<R> {
        if self.inner.cancel.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Invalid);
        }
        let digest = hash(encode(input)?.as_bytes());
        self.inner
            .db
            .write_with(|tx| -> Result<R> {
                let c = tx.connection();
                authorize(c, &self.inner.db, actor)?;
                if let Some(result) = receipt(c, actor, key, &digest)? {
                    return Ok(result);
                }
                let count: i64 =
                    c.query_row("SELECT count(*) FROM hosted_pack_receipts", [], |r| {
                        r.get(0)
                    })?;
                if count >= 4096 {
                    return Err(Error::Limit);
                }
                let result = work(c)?;
                c.execute(
                    "INSERT INTO hosted_pack_receipts VALUES(?,?,?,?,?)",
                    params![
                        actor.principal.as_str(),
                        actor.device.as_str(),
                        key,
                        digest,
                        encode(&result)?
                    ],
                )?;
                audit(c, &self.inner.audit, actor, "resource_pack.edit")?;
                Ok(result)
            })
            .map_err(Into::into)
    }
    pub(super) fn authorize(&self, actor: &Subject) -> Result<()> {
        if self.inner.cancel.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        self.inner
            .db
            .read_with(|tx| authorize(tx.connection(), &self.inner.db, actor))
            .map_err(Into::into)
    }
}
fn receipt<R: DeserializeOwned>(
    c: &Connection,
    actor: &Subject,
    key: &str,
    digest: &str,
) -> Result<Option<R>> {
    let row: Option<(String,String)> = c.query_row("SELECT digest,outcome FROM hosted_pack_receipts WHERE principal=? AND device=? AND request_key=?",params![actor.principal.as_str(),actor.device.as_str(),key],|r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    row.map(|(old, body)| {
        if old == digest {
            decode(&body)
        } else {
            Err(Error::IdempotencyConflict)
        }
    })
    .transpose()
}
