//! Atomic, bounded HTTP mutation receipts; deliberately separate domains from
//! permission administration. Results contain identifiers, never definitions or
//! console/preset inputs. Replays check current capability and principal ranks.
use super::{
    Automation, WorkflowDefinition,
    model::bounded_text,
    service::{invalid, valid_id},
};
use crate::{
    ControlError, IdentityErrorCode,
    identity::{Capability, PrincipalId, Subject},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vesta_state::{StateError, TransactionError};

const MAX_RECEIPTS: i64 = 4096;
const MAX_DOMAIN_RECEIPTS: i64 = 256;
const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const MAX_REVISION: i64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mutation {
    Create {
        name: String,
        execution_principal: String,
        definition: WorkflowDefinition,
        enabled: bool,
    },
    Update {
        id: String,
        expected_revision: i64,
        name: String,
        execution_principal: String,
        definition: WorkflowDefinition,
    },
    SetEnabled {
        id: String,
        expected_revision: i64,
        enabled: bool,
    },
    Delete {
        id: String,
        expected_revision: i64,
    },
    RunManual {
        id: String,
    },
    Discard {
        id: String,
    },
    MarkRead {
        ids: Vec<String>,
    },
    Acknowledge {
        ids: Vec<String>,
    },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MutationOutcome {
    Workflow {
        workflow_id: String,
        revision: i64,
    },
    Deleted {
        workflow_id: String,
        deleted: bool,
    },
    Run {
        workflow_id: String,
        run_id: Option<String>,
        skipped: bool,
    },
    Discarded {
        run_id: String,
        discarded: bool,
        operation_cancelled: bool,
    },
    NotificationsRead {
        marked_read: bool,
    },
    NotificationsAcknowledged {
        marked_read: bool,
        acknowledged: bool,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MutationError {
    Control(ControlError),
    IdempotencyConflict,
    Saturated,
    Rollback {
        primary: Box<Self>,
        failure: StateError,
    },
}
impl MutationError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Control(e) => e.code(),
            Self::IdempotencyConflict => "automation_idempotency_conflict",
            Self::Saturated => "automation_receipts_saturated",
            Self::Rollback { .. } => "rollback_failed",
        }
    }
}
impl std::fmt::Display for MutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for MutationError {}
impl From<ControlError> for MutationError {
    fn from(e: ControlError) -> Self {
        Self::Control(e)
    }
}
impl From<rusqlite::Error> for MutationError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Control(e.into())
    }
}
impl From<TransactionError<MutationError>> for MutationError {
    fn from(e: TransactionError<MutationError>) -> Self {
        match e {
            TransactionError::State(e) => Self::Control(e.into()),
            TransactionError::Work(e) => e,
            TransactionError::Rollback { primary, failure } => Self::Rollback {
                primary: Box::new((*primary).into()),
                failure,
            },
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    // Order is stable so bounded domain accounting need not parse unrelated receipts.
    domain: String,
    principal_id: String,
    targets: Vec<String>,
    outcome: MutationOutcome,
}
fn corrupt() -> MutationError {
    MutationError::Control(StateError::new("state_automation_receipt_corrupt").into())
}
fn hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    hash.finalize().into()
}
impl Mutation {
    fn domain(&self) -> &'static str {
        if matches!(self, Self::MarkRead { .. } | Self::Acknowledge { .. }) {
            "notification-v1"
        } else {
            "automation-v1"
        }
    }
    fn capability(&self) -> Capability {
        if matches!(self, Self::MarkRead { .. } | Self::Acknowledge { .. }) {
            Capability::NotificationsView
        } else {
            Capability::AutomationManage
        }
    }
    fn validate(&self) -> crate::Result<()> {
        match self {
            Self::Create {
                name,
                execution_principal,
                definition,
                ..
            }
            | Self::Update {
                name,
                execution_principal,
                definition,
                ..
            } => {
                if !bounded_text(name, 128, false) {
                    return Err(invalid());
                }
                PrincipalId::parse(execution_principal)?;
                definition.validate()?;
            }
            Self::MarkRead { ids } | Self::Acknowledge { ids }
                if ids.is_empty() || ids.len() > 256 || ids.iter().any(|id| !valid_id(id)) =>
            {
                return Err(invalid());
            }
            _ => {}
        }
        match self {
            Self::Update {
                id,
                expected_revision,
                ..
            }
            | Self::SetEnabled {
                id,
                expected_revision,
                ..
            }
            | Self::Delete {
                id,
                expected_revision,
            } => {
                if !valid_id(id) || !(1..=MAX_REVISION).contains(expected_revision) {
                    return Err(invalid());
                }
            }
            Self::RunManual { id } | Self::Discard { id } if !valid_id(id) => return Err(invalid()),
            _ => {}
        }
        Ok(())
    }
}
impl MutationOutcome {
    fn valid(&self) -> bool {
        match self {
            Self::Workflow {
                workflow_id,
                revision,
            } => valid_id(workflow_id) && (1..=MAX_REVISION).contains(revision),
            Self::Deleted {
                workflow_id,
                deleted,
            } => valid_id(workflow_id) && *deleted,
            Self::Run {
                workflow_id,
                run_id,
                skipped,
            } => {
                valid_id(workflow_id)
                    && run_id.as_ref().is_none_or(|id| valid_id(id))
                    && *skipped == run_id.is_none()
            }
            Self::Discarded {
                run_id,
                discarded,
                operation_cancelled,
            } => valid_id(run_id) && *discarded && !*operation_cancelled,
            Self::NotificationsRead { marked_read } => *marked_read,
            Self::NotificationsAcknowledged {
                marked_read,
                acknowledged,
            } => *marked_read && *acknowledged,
        }
    }
}
impl Automation {
    /// Atomically authorize, apply and retain an HTTP mutation's small result.
    /// Keys are scoped to server, principal, device and domain; the transport
    /// additionally scopes them to method/path. Same key with changed intent is
    /// rejected. Workflow and notification receipt quotas are independent so a
    /// notifications-only principal cannot consume the automation receipt pool.
    /// Retention follows permission receipts: seven days, global ceiling 4,096,
    /// with at most 256 records in either new domain. Beyond retention callers
    /// must not assume that a key still deduplicates a historical mutation.
    pub fn apply_idempotent(
        &self,
        subject: &Subject,
        key: &str,
        mutation: Mutation,
    ) -> std::result::Result<MutationOutcome, MutationError> {
        mutation.validate()?;
        if !(16..=128).contains(&key.len()) || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(invalid().into());
        }
        let input = serde_json::to_vec(&mutation).map_err(|_| MutationError::Control(invalid()))?;
        if input.len() > 70 * 1024 {
            return Err(invalid().into());
        }
        let domain = mutation.domain();
        let key_hash = hash(&[
            b"vesta-http-mutation-receipt-v1",
            domain.as_bytes(),
            self.db.server_id().as_bytes(),
            subject.principal.as_str().as_bytes(),
            subject.device.as_str().as_bytes(),
            key.as_bytes(),
        ]);
        let intent_hash = hash(&[b"vesta-http-mutation-intent-v1", domain.as_bytes(), &input]);
        self.db.write_with(|t| {
            let c=t.connection();
            let actor=self.authorize(c,subject,mutation.capability())?;
            let previous:Option<(Vec<u8>,String)>=c.query_row("SELECT intent_hash,result_json FROM management_permission_receipts WHERE key_hash=?",[key_hash.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((previous_hash,result))=previous {
                if previous_hash.as_slice()!=intent_hash {return Err(MutationError::IdempotencyConflict);}
                let receipt:Receipt=serde_json::from_str(&result).map_err(|_|corrupt())?;
                if receipt.domain!=domain||receipt.principal_id!=subject.principal.as_str()||receipt.targets.len()>2||!receipt.outcome.valid() {return Err(corrupt());}
                for target in &receipt.targets {self.target(c,&actor,&PrincipalId::parse(target).map_err(|_|corrupt())?)?;}
                // Also check present-day ownership after an edit changed a
                // workflow's execution principal. Missing deleted/pruned targets
                // are covered by the principal facts retained in the receipt.
                for target in self.mutation_targets(c,&mutation,true)? {self.target(c,&actor,&target)?;}
                // Creation has no workflow ID in its input; its receipt does.
                if let MutationOutcome::Workflow { workflow_id, .. } = &receipt.outcome
                    && let Some(workflow) = replay_missing(self.load_workflow(c, workflow_id), true)?
                {
                    self.target(c, &actor, &workflow.execution_principal)?;
                }
                return Ok(receipt.outcome);
            }
            let targets=self.mutation_targets(c,&mutation,false)?;
            for target in &targets {self.target(c,&actor,target)?;}
            c.execute("DELETE FROM management_permission_receipts WHERE key_hash IN (SELECT key_hash FROM management_permission_receipts WHERE completed_at<? ORDER BY completed_at LIMIT 128)",[self.now().saturating_sub(RETENTION_MS)])?;
            let count:i64=c.query_row("SELECT count(*) FROM management_permission_receipts",[],|r|r.get(0))?;
            let prefix=format!("{{\"domain\":\"{domain}\",*");
            let domain_count:i64=c.query_row("SELECT count(*) FROM management_permission_receipts WHERE result_json GLOB ?",[prefix],|r|r.get(0))?;
            if count>=MAX_RECEIPTS||domain_count>=MAX_DOMAIN_RECEIPTS {return Err(MutationError::Saturated);}
            let outcome=self.mutate_in(c,subject,&mutation)?;
            let receipt=Receipt{domain:domain.into(),principal_id:subject.principal.to_string(),targets:targets.iter().map(ToString::to_string).collect(),outcome:outcome.clone()};
            let result=serde_json::to_string(&receipt).map_err(|_|corrupt())?;
            c.execute("INSERT INTO management_permission_receipts(key_hash,intent_hash,result_json,completed_at) VALUES (?,?,?,?)",params![key_hash.as_slice(),intent_hash.as_slice(),result,self.now()])?;
            Ok(outcome)
        }).map_err(MutationError::from)
    }
    fn mutation_targets(
        &self,
        c: &Connection,
        mutation: &Mutation,
        replay: bool,
    ) -> crate::Result<Vec<PrincipalId>> {
        let mut targets = Vec::new();
        match mutation {
            Mutation::Create {
                execution_principal,
                ..
            } => targets.push(PrincipalId::parse(execution_principal)?),
            Mutation::Update {
                id,
                execution_principal,
                ..
            } => {
                if let Some(w) = replay_missing(self.load_workflow(c, id), replay)? {
                    targets.push(w.execution_principal);
                }
                targets.push(PrincipalId::parse(execution_principal)?);
            }
            Mutation::SetEnabled { id, .. }
            | Mutation::Delete { id, .. }
            | Mutation::RunManual { id } => {
                if let Some(w) = replay_missing(self.load_workflow(c, id), replay)? {
                    targets.push(w.execution_principal);
                }
            }
            Mutation::Discard { id } => {
                if let Some(run) = replay_missing(self.load_run(c, id), replay)? {
                    targets.push(run.execution_principal);
                }
            }
            Mutation::MarkRead { .. } | Mutation::Acknowledge { .. } => {}
        }
        targets.sort();
        targets.dedup();
        Ok(targets)
    }
    fn mutate_in(
        &self,
        c: &Connection,
        subject: &Subject,
        mutation: &Mutation,
    ) -> crate::Result<MutationOutcome> {
        let outcome = match mutation {
            Mutation::Create {
                name,
                execution_principal,
                definition,
                enabled,
            } => {
                let w = self.create_in(
                    c,
                    subject,
                    name,
                    &PrincipalId::parse(execution_principal)?,
                    definition,
                    *enabled,
                )?;
                MutationOutcome::Workflow {
                    workflow_id: w.id,
                    revision: w.revision,
                }
            }
            Mutation::Update {
                id,
                expected_revision,
                name,
                execution_principal,
                definition,
            } => {
                let w = self.update_in(
                    c,
                    subject,
                    id,
                    *expected_revision,
                    name,
                    &PrincipalId::parse(execution_principal)?,
                    definition,
                )?;
                MutationOutcome::Workflow {
                    workflow_id: w.id,
                    revision: w.revision,
                }
            }
            Mutation::SetEnabled {
                id,
                expected_revision,
                enabled,
            } => {
                let w = self.set_enabled_in(c, subject, id, *expected_revision, *enabled)?;
                MutationOutcome::Workflow {
                    workflow_id: w.id,
                    revision: w.revision,
                }
            }
            Mutation::Delete {
                id,
                expected_revision,
            } => {
                self.delete_in(c, subject, id, *expected_revision)?;
                MutationOutcome::Deleted {
                    workflow_id: id.clone(),
                    deleted: true,
                }
            }
            Mutation::RunManual { id } => {
                let run_id = self.run_manual_in(c, subject, id)?;
                let skipped = run_id.is_none();
                MutationOutcome::Run {
                    workflow_id: id.clone(),
                    run_id,
                    skipped,
                }
            }
            Mutation::Discard { id } => {
                self.discard_suspended_in(c, subject, id)?;
                MutationOutcome::Discarded {
                    run_id: id.clone(),
                    discarded: true,
                    operation_cancelled: false,
                }
            }
            Mutation::MarkRead { ids } => {
                self.mark_in(c, subject, ids, false)?;
                MutationOutcome::NotificationsRead { marked_read: true }
            }
            Mutation::Acknowledge { ids } => {
                self.mark_in(c, subject, ids, true)?;
                MutationOutcome::NotificationsAcknowledged {
                    marked_read: true,
                    acknowledged: true,
                }
            }
        };
        Ok(outcome)
    }
}
fn replay_missing<T>(value: crate::Result<T>, replay: bool) -> crate::Result<Option<T>> {
    match value {
        Ok(value) => Ok(Some(value)),
        Err(ControlError::Rejected {
            code: IdentityErrorCode::NotFound,
            ..
        }) if replay => Ok(None),
        Err(e) => Err(e),
    }
}
