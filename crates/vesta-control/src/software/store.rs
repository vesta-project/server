use super::{Job, Result, SoftwareError, model::StoredLaunch};
use crate::identity::{BuiltinRole, Capability, Subject};
use rusqlite::{Connection, OptionalExtension, params};
use vesta_state::{
    StateDatabase, TransactionError,
    audit::{AuditActor, AuditLog, AuditRecord},
};

impl From<TransactionError<SoftwareError>> for SoftwareError {
    fn from(e: TransactionError<SoftwareError>) -> Self {
        match e {
            TransactionError::Work(e) => e,
            _ => Self::StateUnavailable,
        }
    }
}

pub(super) fn authorize(c: &Connection, db: &StateDatabase, subject: &Subject) -> Result<()> {
    let policy = crate::identity::policy_in(c, db.server_id(), db.clock().millis(), Some(subject))
        .map_err(|_| SoftwareError::StateUnavailable)?;
    if policy.holds_builtin(BuiltinRole::Owner)
        && policy.check(Capability::ContentInstall, None).allowed()
    {
        Ok(())
    } else {
        Err(SoftwareError::Forbidden)
    }
}

pub(super) fn audit(
    c: &Connection,
    log: &AuditLog,
    subject: &Subject,
    action: &str,
    id: &str,
    outcome: &str,
) -> Result<()> {
    let actor = AuditActor::new(
        subject.principal.as_str().into(),
        subject.device.as_str().into(),
        "software".into(),
    )?;
    let record = AuditRecord::new(
        actor,
        action.into(),
        format!("software:{id}"),
        outcome.into(),
        None,
        [],
    )?;
    log.append(c, &record)?;
    Ok(())
}

pub(super) struct State {
    pub revision: u64,
    pub current: StoredLaunch,
    pub previous: Option<StoredLaunch>,
    pub recovery: bool,
}
pub(super) fn state(c: &Connection) -> Result<State> {
    let (revision, current, previous, recovery): (i64, String, Option<String>, bool) = c.query_row(
        "SELECT revision,current_launch,previous_launch,recovery_required FROM software_state WHERE singleton=1",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    Ok(State {
        revision: u64::try_from(revision).map_err(|_| SoftwareError::StateUnavailable)?,
        current: decode(&current)?,
        previous: previous.as_deref().map(decode).transpose()?,
        recovery,
    })
}
pub(super) fn encode(value: &StoredLaunch) -> Result<String> {
    serde_json::to_string(value).map_err(|_| SoftwareError::StateUnavailable)
}
pub(super) fn decode(text: &str) -> Result<StoredLaunch> {
    if text.len() > 1024 * 1024 {
        return Err(SoftwareError::StateUnavailable);
    }
    serde_json::from_str(text).map_err(|_| SoftwareError::StateUnavailable)
}
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let encoded: Option<String> = r.get(6)?;
    let warnings = encoded
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?
        .unwrap_or_default();
    Ok(Job {
        error_context: r.get::<_, Option<String>>(7)?.map(|raw| serde_json::from_str(&raw).map_err(|error|rusqlite::Error::FromSqlConversionFailure(7,rusqlite::types::Type::Text,Box::new(error)))).transpose()?,
        id: r.get(0)?,
        kind: r.get(1)?,
        state: r.get(2)?,
        created_at_ms: r.get(3)?,
        updated_at_ms: r.get(4)?,
        error: r.get(5)?,
        warnings,
    })
}
const COLUMNS: &str = "SELECT id,kind,state,created_at_ms,updated_at_ms,error,(SELECT warnings_json FROM software_job_results WHERE job_id=software_jobs.id),(SELECT context_json FROM software_job_failures WHERE job_id=software_jobs.id) FROM software_jobs";
pub(super) fn job(c: &Connection, id: &str) -> Result<Job> {
    c.query_row(&format!("{COLUMNS} WHERE id=?"), [id], row)
        .map_err(Into::into)
}
pub(super) fn jobs(c: &Connection) -> Result<Vec<Job>> {
    let mut q = c.prepare(&format!(
        "{COLUMNS} ORDER BY created_at_ms DESC,id DESC LIMIT 100"
    ))?;
    q.query_map([], row)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}
pub(super) fn existing(
    c: &Connection,
    subject: &Subject,
    key: &str,
    hash: &str,
) -> Result<Option<Job>> {
    let found: Option<(String,String)> = c.query_row(
        "SELECT id,request_hash FROM software_jobs WHERE principal=? AND device=? AND idempotency_key=?",
        params![subject.principal.as_str(),subject.device.as_str(),key],
        |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    found
        .map(|(id, stored)| {
            if stored == hash {
                job(c, &id)
            } else {
                Err(SoftwareError::IdempotencyConflict)
            }
        })
        .transpose()
}

// Keep the durable actor, request binding and revision explicit at admission.
#[allow(clippy::too_many_arguments)]
pub(super) fn admit(
    c: &Connection,
    db: &StateDatabase,
    log: &AuditLog,
    subject: &Subject,
    kind: &str,
    key: &str,
    hash: &str,
    revision: u64,
) -> Result<Job> {
    let revision = i64::try_from(revision).map_err(|_| SoftwareError::StateUnavailable)?;
    let (active,total): (i64,i64) = c.query_row(
        "SELECT COALESCE(sum(state IN ('queued','staging','activating')),0),count(*) FROM software_jobs", [],
        |r| Ok((r.get(0)?,r.get(1)?)))?;
    // Retain durable keys rather than silently forgetting idempotency history.
    if active != 0 || total >= 4096 {
        return Err(SoftwareError::Busy);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = db.clock().millis();
    c.execute("INSERT INTO software_jobs(id,kind,state,principal,device,idempotency_key,request_hash,expected_revision,created_at_ms,updated_at_ms) VALUES(?,?,'queued',?,?,?,?,?,?,?)",
        params![id,kind,subject.principal.as_str(),subject.device.as_str(),key,hash,revision,now,now])?;
    audit(c, log, subject, "software.submit", &id, "queued")?;
    job(c, &id)
}

pub(super) fn initialize(db: &StateDatabase, original: &StoredLaunch) -> Result<bool> {
    db.write_with(|tx| -> Result<bool> {
        let c=tx.connection();
        c.execute_batch(include_str!("schema.sql"))?;
        let schema:String=c.query_row("SELECT sql FROM sqlite_master WHERE name='software_jobs'",[],|r|r.get(0))?;
        if !schema.contains("'custom_jar'") {
            c.execute_batch("ALTER TABLE software_jobs RENAME TO software_jobs_legacy")?;
            c.execute_batch(include_str!("schema.sql"))?;
            c.execute_batch("INSERT INTO software_jobs SELECT * FROM software_jobs_legacy; DROP TABLE software_jobs_legacy;")?;
        }
        c.execute_batch("CREATE TABLE IF NOT EXISTS software_managed_launch(singleton INTEGER PRIMARY KEY,launch_json TEXT NOT NULL)")?;
        c.execute("INSERT OR IGNORE INTO software_state(singleton,revision,current_launch) VALUES(1,0,?)",[encode(original)?])?;
        let now=db.clock().millis();
        // Never replay a process/activation step with an ambiguous admitted result.
        c.execute("UPDATE software_jobs SET state='recovery_required',updated_at_ms=?,error='software_activation_interrupted' WHERE state='activating'",[now])?;
        c.execute("UPDATE software_state SET recovery_required=1 WHERE EXISTS(SELECT 1 FROM software_jobs WHERE state='recovery_required')",[])?;
        c.execute("UPDATE software_jobs SET state='failed',updated_at_ms=?,error='software_staging_interrupted' WHERE state IN ('queued','staging')",[now])?;
        let mut s=state(c)?;
        // The host passed its actual current launch document. A mismatch means an
        // interrupted external edit/activation; startup must refuse silent adoption.
        if s.current.document != original.document {
            c.execute("UPDATE software_state SET recovery_required=1 WHERE singleton=1",[])?;
            s.recovery=true;
        }
        Ok(s.recovery)
    }).map_err(Into::into)
}
