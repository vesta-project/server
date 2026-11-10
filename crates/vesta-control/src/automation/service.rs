use super::{inbox, model::bounded_output, *};
use crate::{
    ControlError, IdentityErrorCode, Result,
    identity::{self, Capability, DenyReason, DeviceId, PolicySnapshot, PrincipalId, Subject},
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::sync::Arc;
use vesta_state::{
    StateDatabase,
    audit::{AuditActor, AuditLog, AuditRecord},
};

const MAX_WORKFLOWS: i64 = 128;
const MAX_RUNS: i64 = 2048;
const GRACE_MS: i64 = 60_000;
#[derive(Clone)]
pub struct Automation {
    pub(super) db: StateDatabase,
    port: Arc<dyn ActionPort>,
    audit: AuditLog,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub execution_principal: PrincipalId,
    pub definition: WorkflowDefinition,
    pub created_at: i64,
    pub updated_at: i64,
    pub revision: i64,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub id: String,
    pub workflow_id: String,
    pub revision: i64,
    pub execution_principal: PrincipalId,
    pub definition: WorkflowDefinition,
    pub trigger: serde_json::Value,
    pub state: String,
    pub current_step: usize,
    pub attempt: u8,
    pub next_wake_at: Option<i64>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub outcome_code: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct RunStep {
    pub run_id: String,
    pub index: usize,
    pub attempt: u8,
    pub state: String,
    pub operation_reference: Option<String>,
    pub output: Option<serde_json::Value>,
    pub error_code: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}
pub(crate) fn invalid() -> ControlError {
    ControlError::rejected(IdentityErrorCode::InvalidRequest)
}
fn not_found() -> ControlError {
    ControlError::rejected(IdentityErrorCode::NotFound)
}
fn conflict() -> ControlError {
    ControlError::rejected(IdentityErrorCode::Conflict)
}
pub(crate) fn page(limit: usize) -> Result<()> {
    if !(1..=256).contains(&limit) {
        Err(invalid())
    } else {
        Ok(())
    }
}
pub(crate) fn valid_id(id: &str) -> bool {
    PrincipalId::parse(id).is_ok()
}
fn json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|_| invalid())
}
fn stored<T: serde::de::DeserializeOwned>(value: String) -> rusqlite::Result<T> {
    serde_json::from_str(&value).map_err(|_| rusqlite::Error::InvalidQuery)
}
fn workflow_row(r: &Row<'_>) -> rusqlite::Result<Workflow> {
    let principal: String = r.get(3)?;
    Ok(Workflow {
        id: r.get(0)?,
        name: r.get(1)?,
        enabled: r.get(2)?,
        execution_principal: PrincipalId::parse(&principal)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        definition: stored(r.get(4)?)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        revision: r.get(7)?,
    })
}
fn run_row(r: &Row<'_>) -> rusqlite::Result<Run> {
    let principal: String = r.get(4)?;
    Ok(Run {
        id: r.get(0)?,
        workflow_id: r.get(1)?,
        revision: r.get(2)?,
        definition: stored(r.get(3)?)?,
        execution_principal: PrincipalId::parse(&principal)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        trigger: stored(r.get(5)?)?,
        state: r.get(6)?,
        current_step: r.get::<_, u32>(7)? as usize,
        attempt: r.get(8)?,
        next_wake_at: r.get(9)?,
        started_at: r.get(10)?,
        finished_at: r.get(11)?,
        outcome_code: r.get(12)?,
    })
}
fn step_row(r: &Row<'_>) -> rusqlite::Result<RunStep> {
    let output: Option<String> = r.get(5)?;
    Ok(RunStep {
        run_id: r.get(0)?,
        index: r.get::<_, u32>(1)? as usize,
        attempt: r.get(2)?,
        state: r.get(3)?,
        operation_reference: r.get(4)?,
        output: output.map(stored).transpose()?,
        error_code: r.get(6)?,
        started_at: r.get(7)?,
        finished_at: r.get(8)?,
    })
}
const WORKFLOW_COLUMNS: &str =
    "id,name,enabled,principal_id,definition_json,created_at,updated_at,revision";
const RUN_COLUMNS: &str = "id,workflow_id,revision,definition_json,principal_id,trigger_json,state,current_step,attempt,next_wake_at,started_at,finished_at,outcome_code";
const STEP_COLUMNS: &str =
    "run_id,index_no,attempt,state,operation_ref,output_json,error_code,started_at,finished_at";
impl Automation {
    pub(crate) fn database_for_alerts(&self) -> StateDatabase {
        self.db.clone()
    }
    pub fn new(db: StateDatabase, port: Arc<dyn ActionPort>) -> Result<Self> {
        db.write(|c| { c.execute_batch("CREATE TABLE IF NOT EXISTS automation_player_visits (uuid TEXT PRIMARY KEY NOT NULL, first_name TEXT NOT NULL, first_seen_at INTEGER NOT NULL) STRICT;")?; Ok(()) })?;
        let audit = AuditLog::new(db.clone())?;
        Ok(Self { db, port, audit })
    }
    pub(crate) fn now(&self) -> i64 {
        self.db.clock().millis()
    }
    pub(crate) fn write<T>(&self, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.db
            .write_with(|t| work(t.connection()))
            .map_err(ControlError::from)
    }
    pub(crate) fn read<T>(&self, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.db
            .read_with(|t| work(t.connection()))
            .map_err(ControlError::from)
    }
    pub(crate) fn authorize(
        &self,
        c: &Connection,
        subject: &Subject,
        capability: Capability,
    ) -> Result<PolicySnapshot> {
        let policy = identity::policy_in(c, self.db.server_id(), self.now(), Some(subject))?;
        match policy.check(capability, None) {
            identity::Decision::Allow => Ok(policy),
            identity::Decision::Deny(reason) => Err(ControlError::forbidden(reason)),
        }
    }
    // Changing another principal's automation is delegated authority: only a
    // strictly lower-ranked current principal may be targeted. Self is allowed.
    pub(super) fn target(
        &self,
        c: &Connection,
        actor: &PolicySnapshot,
        principal: &PrincipalId,
    ) -> Result<()> {
        if actor.principal() == Some(principal) {
            return Ok(());
        }
        let device: Option<String> = c.query_row("SELECT d.id FROM devices d JOIN principals p ON p.id=d.principal_id WHERE p.id=? AND p.server_id=? AND p.status='active' AND d.revoked_at IS NULL ORDER BY d.id LIMIT 1",params![principal.as_str(),self.db.server_id()],|r|r.get(0)).optional()?;
        let device = DeviceId::parse(&device.ok_or_else(not_found)?)?;
        let policy = identity::policy_in(
            c,
            self.db.server_id(),
            self.now(),
            Some(&Subject {
                principal: principal.clone(),
                device,
            }),
        )?;
        if policy.rank() >= actor.rank() {
            return Err(ControlError::forbidden(DenyReason::Rank));
        }
        Ok(())
    }
    fn audit(
        &self,
        c: &Connection,
        subject: Option<&Subject>,
        action: &str,
        target: &str,
        outcome: &str,
        run: Option<&str>,
    ) -> Result<()> {
        let actor = if let Some(s) = subject {
            AuditActor::new(
                s.principal.to_string(),
                s.device.to_string(),
                "automation".into(),
            )?
        } else {
            AuditActor::system("automation".into())?
        };
        self.audit.append(
            c,
            &AuditRecord::of(
                actor,
                action.into(),
                target.into(),
                outcome.into(),
                run.map(str::to_owned),
            )?,
        )?;
        Ok(())
    }
    pub fn create(
        &self,
        subject: &Subject,
        name: &str,
        execution_principal: &PrincipalId,
        definition: WorkflowDefinition,
        enabled: bool,
    ) -> Result<Workflow> {
        self.write(|c| self.create_in(c, subject, name, execution_principal, &definition, enabled))
    }
    pub(super) fn create_in(
        &self,
        c: &Connection,
        subject: &Subject,
        name: &str,
        execution_principal: &PrincipalId,
        definition: &WorkflowDefinition,
        enabled: bool,
    ) -> Result<Workflow> {
        definition.validate()?;
        if !super::model::bounded_text(name, 128, false) {
            return Err(invalid());
        }
        let definition_json = json(definition)?;
        let actor = self.authorize(c, subject, Capability::AutomationManage)?;
        self.target(c, &actor, execution_principal)?;
        if c.query_row("SELECT count(*) FROM workflows", [], |r| r.get::<_, i64>(0))?
            >= MAX_WORKFLOWS
        {
            return Err(conflict());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = self.now();
        if now < 0 {
            return Err(invalid());
        }
        c.execute("INSERT INTO workflows(id,name,enabled,principal_id,definition_json,created_at,updated_at,revision) VALUES (?,?,?,?,?,?,?,1)",params![id,name,enabled,execution_principal.as_str(),definition_json,now,now])?;
        c.execute(
            "INSERT INTO workflow_schedule(workflow_id,last_occurrence) VALUES (?,?)",
            params![id, now],
        )?;
        self.audit(c, Some(subject), "automation.create", &id, "allowed", None)?;
        self.load_workflow(c, &id)
    }
    /// Optimistic revision guards concurrent editors. Revisions include enable
    /// changes; run snapshots are immutable and never borrow the current version.
    pub fn update(
        &self,
        subject: &Subject,
        id: &str,
        expected_revision: i64,
        name: &str,
        execution_principal: &PrincipalId,
        definition: WorkflowDefinition,
    ) -> Result<Workflow> {
        self.write(|c| {
            self.update_in(
                c,
                subject,
                id,
                expected_revision,
                name,
                execution_principal,
                &definition,
            )
        })
    }
    // Transaction-taking twin of the public update API; the extra connection
    // keeps the mutation and HTTP receipt in the same commit.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn update_in(
        &self,
        c: &Connection,
        subject: &Subject,
        id: &str,
        expected_revision: i64,
        name: &str,
        execution_principal: &PrincipalId,
        definition: &WorkflowDefinition,
    ) -> Result<Workflow> {
        definition.validate()?;
        if !valid_id(id) || !super::model::bounded_text(name, 128, false) {
            return Err(invalid());
        }
        let definition_json = json(definition)?;
        let actor = self.authorize(c, subject, Capability::AutomationManage)?;
        let old = self.load_workflow(c, id)?;
        self.target(c, &actor, &old.execution_principal)?;
        self.target(c, &actor, execution_principal)?;
        if old.revision != expected_revision || old.revision >= 9_007_199_254_740_991 {
            return Err(conflict());
        }
        c.execute("UPDATE workflows SET name=?,principal_id=?,definition_json=?,updated_at=?,revision=revision+1 WHERE id=?",params![name,execution_principal.as_str(),definition_json,self.now(),id])?;
        // A definition change starts a new schedule epoch, not an invented backlog.
        c.execute(
            "UPDATE workflow_schedule SET last_occurrence=? WHERE workflow_id=?",
            params![self.now(), id],
        )?;
        self.audit(c, Some(subject), "automation.update", id, "allowed", None)?;
        self.load_workflow(c, id)
    }
    pub fn set_enabled(
        &self,
        subject: &Subject,
        id: &str,
        expected_revision: i64,
        enabled: bool,
    ) -> Result<Workflow> {
        self.write(|c| self.set_enabled_in(c, subject, id, expected_revision, enabled))
    }
    pub(super) fn set_enabled_in(
        &self,
        c: &Connection,
        subject: &Subject,
        id: &str,
        expected_revision: i64,
        enabled: bool,
    ) -> Result<Workflow> {
        if !valid_id(id) {
            return Err(invalid());
        }
        let actor = self.authorize(c, subject, Capability::AutomationManage)?;
        let old = self.load_workflow(c, id)?;
        self.target(c, &actor, &old.execution_principal)?;
        if old.revision != expected_revision || old.revision >= 9_007_199_254_740_991 {
            return Err(conflict());
        }
        c.execute(
            "UPDATE workflows SET enabled=?,updated_at=?,revision=revision+1 WHERE id=?",
            params![enabled, self.now(), id],
        )?;
        // Disabled time is explicitly not eligible for catch-up.
        c.execute(
            "UPDATE workflow_schedule SET last_occurrence=? WHERE workflow_id=?",
            params![self.now(), id],
        )?;
        self.audit(c, Some(subject), "automation.enable", id, "allowed", None)?;
        self.load_workflow(c, id)
    }
    /// Deletion preserves already-started run snapshots, but discards queued work.
    pub fn delete(&self, subject: &Subject, id: &str, expected_revision: i64) -> Result<()> {
        self.write(|c| self.delete_in(c, subject, id, expected_revision))
    }
    pub(super) fn delete_in(
        &self,
        c: &Connection,
        subject: &Subject,
        id: &str,
        expected_revision: i64,
    ) -> Result<()> {
        if !valid_id(id) {
            return Err(invalid());
        }
        let actor = self.authorize(c, subject, Capability::AutomationManage)?;
        let old = self.load_workflow(c, id)?;
        self.target(c, &actor, &old.execution_principal)?;
        if old.revision != expected_revision {
            return Err(conflict());
        }
        c.execute("UPDATE workflow_runs SET state='failed',finished_at=?,outcome_code='workflow_deleted' WHERE workflow_id=? AND state='queued'",params![self.now(),id])?;
        c.execute("DELETE FROM workflow_schedule WHERE workflow_id=?", [id])?;
        c.execute("DELETE FROM workflows WHERE id=?", [id])?;
        self.audit(c, Some(subject), "automation.delete", id, "allowed", None)
    }
    /// Does not cancel an already-submitted underlying operation. No resume API
    /// can bypass re-authorization; managers may discard suspended work instead.
    pub fn discard_suspended(&self, subject: &Subject, run_id: &str) -> Result<()> {
        self.write(|c| self.discard_suspended_in(c, subject, run_id))
    }
    pub(super) fn discard_suspended_in(
        &self,
        c: &Connection,
        subject: &Subject,
        run_id: &str,
    ) -> Result<()> {
        if !valid_id(run_id) {
            return Err(invalid());
        }
        let actor = self.authorize(c, subject, Capability::AutomationManage)?;
        let run = self.load_run(c, run_id)?;
        self.target(c, &actor, &run.execution_principal)?;
        if run.state != "suspended" {
            return Err(conflict());
        }
        c.execute("UPDATE workflow_runs SET state='failed',finished_at=?,outcome_code='discarded' WHERE id=?",params![self.now(),run_id])?;
        self.audit(
            c,
            Some(subject),
            "automation.discard",
            &run.workflow_id,
            "allowed",
            Some(run_id),
        )
    }
    pub fn get_workflow(&self, subject: &Subject, id: &str) -> Result<Workflow> {
        if !valid_id(id) {
            return Err(invalid());
        }
        self.read(|c| {
            self.authorize(c, subject, Capability::AutomationView)?;
            self.load_workflow(c, id)
        })
    }
    pub fn list_workflows(
        &self,
        subject: &Subject,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Workflow>> {
        page(limit)?;
        if after.is_some_and(|id| !valid_id(id)) {
            return Err(invalid());
        }
        self.read(|c| {
            self.authorize(c, subject, Capability::AutomationView)?;
            let mut q = c.prepare(&format!(
                "SELECT {WORKFLOW_COLUMNS} FROM workflows WHERE id>? ORDER BY id LIMIT ?"
            ))?;
            Ok(
                q.query_map(params![after.unwrap_or(""), limit as i64], workflow_row)?
                    .collect::<std::result::Result<_, _>>()?,
            )
        })
    }
    pub fn get_run(&self, subject: &Subject, id: &str) -> Result<Run> {
        if !valid_id(id) {
            return Err(invalid());
        }
        self.read(|c| {
            self.authorize(c, subject, Capability::AutomationView)?;
            self.load_run(c, id)
        })
    }
    pub fn list_runs(
        &self,
        subject: &Subject,
        workflow_id: Option<&str>,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Run>> {
        page(limit)?;
        if workflow_id.is_some_and(|id| !valid_id(id)) || after.is_some_and(|id| !valid_id(id)) {
            return Err(invalid());
        }
        self.read(|c| {self.authorize(c,subject,Capability::AutomationView)?;
            let mut q=c.prepare(&format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE (? IS NULL OR workflow_id=?) AND id>? ORDER BY id LIMIT ?"))?;
            Ok(q.query_map(params![workflow_id,workflow_id,after.unwrap_or(""),limit as i64],run_row)?.collect::<std::result::Result<_,_>>()?)
        })
    }
    /// Descending (started_at, id) keyset page. `before` is the last returned run ID.
    pub fn list_runs_newest(
        &self,
        subject: &Subject,
        workflow_id: Option<&str>,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Run>> {
        page(limit)?;
        if workflow_id.is_some_and(|id| !valid_id(id)) || before.is_some_and(|id| !valid_id(id)) {
            return Err(invalid());
        }
        self.read(|c| {
            self.authorize(c, subject, Capability::AutomationView)?;
            let mut q = c.prepare(&format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE (? IS NULL OR workflow_id=?) AND (? IS NULL OR (started_at,id)<(SELECT started_at,id FROM workflow_runs WHERE id=? AND (? IS NULL OR workflow_id=?))) ORDER BY started_at DESC,id DESC LIMIT ?"))?;
            Ok(q.query_map(params![workflow_id,workflow_id,before,before,workflow_id,workflow_id,limit as i64],run_row)?.collect::<std::result::Result<_,_>>()?)
        })
    }
    /// Step pages use a flattened index (`index * 6 + attempt`) as their cursor.
    pub fn list_steps(
        &self,
        subject: &Subject,
        run_id: &str,
        after: Option<u16>,
        limit: usize,
    ) -> Result<Vec<RunStep>> {
        page(limit)?;
        if !valid_id(run_id) || after.is_some_and(|a| a >= 192) {
            return Err(invalid());
        }
        self.read(|c| {self.authorize(c,subject,Capability::AutomationView)?;self.load_run(c,run_id)?;
            let mut q=c.prepare(&format!("SELECT {STEP_COLUMNS} FROM run_steps WHERE run_id=? AND index_no*6+attempt>? ORDER BY index_no,attempt LIMIT ?"))?;
            Ok(q.query_map(params![run_id,after.map_or(-1,i64::from),limit as i64],step_row)?.collect::<std::result::Result<_,_>>()?)
        })
    }
    pub fn run_manual(&self, subject: &Subject, id: &str) -> Result<Option<String>> {
        self.write(|c| self.run_manual_in(c, subject, id))
    }
    pub(super) fn run_manual_in(
        &self,
        c: &Connection,
        subject: &Subject,
        id: &str,
    ) -> Result<Option<String>> {
        if !valid_id(id) {
            return Err(invalid());
        }
        let actor = self.authorize(c, subject, Capability::AutomationManage)?;
        let workflow = self.load_workflow(c, id)?;
        self.target(c, &actor, &workflow.execution_principal)?;
        if !workflow.enabled || !matches!(workflow.definition.trigger, Trigger::Manual) {
            return Err(conflict());
        }
        self.enqueue(
            c,
            &workflow,
            &serde_json::json!({"kind":"manual"}),
            self.now(),
            false,
        )
    }
    pub fn publish(&self, event: AutomationEvent) -> Result<Vec<String>> {
        self.write(|c| {
            let mut q = c.prepare(&format!(
                "SELECT {WORKFLOW_COLUMNS} FROM workflows WHERE enabled=1 ORDER BY id"
            ))?;
            let workflows = q
                .query_map([], workflow_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut runs = Vec::new();
            for w in workflows {
                if event.matches_trigger(&w.definition.trigger)
                    && let Some(id) = self.enqueue(
                        c,
                        &w,
                        &serde_json::json!({"kind":"event","event":event}),
                        self.now(),
                        false,
                    )?
                {
                    runs.push(id);
                }
            }
            Ok(runs)
        })
    }
    /// Join observation and its matching runs commit together. Never forget visits during retention.
    pub(crate) fn seed_player_history(&self, ids: &[uuid::Uuid]) -> Result<()> {
        if ids.len() > 128 {
            return Err(invalid());
        }
        if ids.is_empty() {
            return Ok(());
        }
        self.write(|c| {
            let mut count = c.query_row("SELECT count(*) FROM automation_player_visits", [], |r| r.get::<_,i64>(0))?;
            for id in ids {
                let id = id.to_string();
                if count >= 100_000 && c.query_row("SELECT 1 FROM automation_player_visits WHERE uuid=?", [&id], |_| Ok(())).optional()?.is_none() { return Err(conflict()); }
                count += c.execute("INSERT OR IGNORE INTO automation_player_visits(uuid,first_name,first_seen_at) VALUES (?,'',?)", params![id,self.now()])? as i64;
            }
            Ok(())
        })
    }
    pub(crate) fn observe_player_join(
        &self,
        uuid: uuid::Uuid,
        player: &str,
        historical: bool,
    ) -> Result<bool> {
        if !crate::moderation::valid_player(player) {
            return Err(invalid());
        }
        let uuid = uuid.to_string();
        if self.read(|c| {
            Ok(c.query_row(
                "SELECT 1 FROM automation_player_visits WHERE uuid=?",
                [&uuid],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
        })? {
            return Ok(false);
        }
        self.write(|c| {
            if c.query_row("SELECT 1 FROM automation_player_visits WHERE uuid=?", [&uuid], |_| Ok(())).optional()?.is_some() { return Ok(false); }
            if c.query_row("SELECT count(*) FROM automation_player_visits", [], |r| r.get::<_,i64>(0))? >= 100_000 { return Err(conflict()); }
            c.execute("INSERT INTO automation_player_visits(uuid,first_name,first_seen_at) VALUES (?,?,?)", params![uuid,player,self.now()])?;
            if historical { return Ok(false); }
            let event = AutomationEvent::PlayerFirstJoined { player: player.into(), uuid };
            let mut query = c.prepare(&format!("SELECT {WORKFLOW_COLUMNS} FROM workflows WHERE enabled=1 ORDER BY id"))?;
            let workflows = query.query_map([], workflow_row)?.collect::<std::result::Result<Vec<_>,_>>()?;
            for workflow in workflows {
                if event.matches_trigger(&workflow.definition.trigger) && self.enqueue(c, &workflow, &serde_json::json!({"kind":"event","event":event}), self.now(), true)?.is_none() {
                    // Roll back both history and fan-out. The console observer retries after pressure clears.
                    return Err(conflict());
                }
            }
            Ok(true)
        })
    }
    pub(super) fn load_workflow(&self, c: &Connection, id: &str) -> Result<Workflow> {
        c.query_row(
            &format!("SELECT {WORKFLOW_COLUMNS} FROM workflows WHERE id=?"),
            [id],
            workflow_row,
        )
        .optional()?
        .ok_or_else(not_found)
    }
    pub(super) fn load_run(&self, c: &Connection, id: &str) -> Result<Run> {
        c.query_row(
            &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id=?"),
            [id],
            run_row,
        )
        .optional()?
        .ok_or_else(not_found)
    }
    fn enqueue(
        &self,
        c: &Connection,
        w: &Workflow,
        trigger: &serde_json::Value,
        now: i64,
        catch_up: bool,
    ) -> Result<Option<String>> {
        let pending:i64=c.query_row("SELECT count(*) FROM workflow_runs WHERE workflow_id=? AND state IN ('queued','running','suspended')",[&w.id],|r|r.get(0))?;
        if pending > 0 && (!catch_up || pending >= 10) {
            return Ok(None);
        }
        // History is pruned before admission; pending rows are never evicted.
        self.retain(c, now)?;
        if c.query_row("SELECT count(*) FROM workflow_runs", [], |r| {
            r.get::<_, i64>(0)
        })? >= MAX_RUNS
        {
            // Admission pressure cannot roll back progress of existing runs.
            inbox::insert(
                c,
                now,
                Severity::Warning,
                "Workflow run capacity reached",
                "This occurrence was skipped because durable run history is at its storage bound.",
                &w.id,
            )?;
            self.audit(
                c,
                None,
                "automation.trigger",
                &w.id,
                "capacity_skipped",
                None,
            )?;
            return Ok(None);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let state = if pending == 0 { "running" } else { "queued" };
        c.execute("INSERT INTO workflow_runs(id,workflow_id,revision,definition_json,principal_id,trigger_json,state,current_step,attempt,next_wake_at,started_at) VALUES (?,?,?,?,?,?,?,0,0,?,?)",params![id,w.id,w.revision,json(&w.definition)?,w.execution_principal.as_str(),json(trigger)?,state,now,now])?;
        self.audit(c, None, "automation.trigger", &w.id, "started", Some(&id))?;
        Ok(Some(id))
    }
    fn retain(&self, c: &Connection, now: i64) -> Result<()> {
        c.execute("DELETE FROM workflow_runs WHERE state IN ('succeeded','failed') AND (finished_at<? OR id IN (SELECT id FROM workflow_runs WHERE state IN ('succeeded','failed') ORDER BY finished_at DESC,id DESC LIMIT -1 OFFSET 1024))",[now.saturating_sub(inbox::HISTORY_MS)])?;
        inbox::retain(c, now)
    }
}

include!("engine.rs");
