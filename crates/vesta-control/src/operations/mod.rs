//! Durable operations over current canonical server-local policy.
//!
//! `Subject` values are trusted application lookup keys, NOT transport authentication.
//! Only the concrete game lifecycle composition can execute the closed production
//! bindings. There is no public handler/authority registry, local-proof issuer or
//! network route. Private ownership is retained through genuine execution lifetime.
mod audit_view;
pub use audit_view::AuditNotice;
pub use vesta_game_runtime::console::ConsoleStream;
pub use vesta_game_runtime::process::{GenerationId, ProcessPhase};
mod bindings;
mod confirmation;
mod confirmation_repository;
mod properties_confirmation;
pub(crate) use properties_confirmation::{
    PropertiesApproval, accept_properties_confirmation, check_properties_intent,
    prepare_properties_confirmation, refuse_properties_confirmation,
};
mod domain;
mod game;
mod input;
mod players;
mod queue;
pub mod readiness;
mod replay;
mod repository;
mod runner;

pub use bindings::GameCommand;
pub(crate) use bindings::SourceRevision;
pub use confirmation::{ConfirmationToken, IssuedConfirmation, MAX_CONFIRMATION_TTL};
pub use domain::Admission;
#[cfg(test)]
pub(crate) use domain::PurgeReport;
pub use domain::{
    CancelOutcome, MAX_PAGE, OperationError, OperationErrorCode, OperationId, OperationPage,
    OperationState, OperationSummary, OperationsResult,
};
pub use players::{
    MAX_PLAYER_QUERY_BYTES, MAX_PLAYER_QUERY_LINES, MAX_PLAYER_QUERY_WAIT, PlayerObservation,
    PlayerQueryError, PlayerQueryResult,
};
pub(crate) use queue::{Claim, FinishOutcome, OwnerSession};
pub use replay::{
    MAX_REPLAY_RETURN, MAX_REPLAY_SCAN, ReplayCursor, ReplayEvent, ReplayEventKind, ReplayPage,
    ReplayStatus,
};
pub use runner::{
    LocalAction, LocalRuntime, OperationRuntime, RunnerStatus, RuntimeHandle, ShutdownOutcome,
};

use crate::identity::{self, Capability, PolicySnapshot, Subject};
use domain::{ENGINE_SOURCE, text};
use domain::{MAX_REQUIREMENTS, REQUEST_SOURCE, hash_text, invalid};
use input::PreparedOperation;
use repository::{Repository, StoredOperation};
use serde::Serialize;
use vesta_state::audit::{AuditActor, AuditLog, AuditRecord};
use vesta_state::events::EventLog;
use vesta_state::{StateDatabase, TransactionError};

/// An in-process application facade, not transport authentication. Every command
/// obtains current server-local policy in its own admitted transaction.
#[derive(Clone)]
pub struct Operations {
    db: StateDatabase,
    audit: AuditLog,
    events: EventLog,
}
impl std::fmt::Debug for Operations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Operations").finish_non_exhaustive()
    }
}
impl Operations {
    pub fn new(db: StateDatabase) -> OperationsResult<Self> {
        let audit = AuditLog::new(db.clone())?;
        let events = EventLog::new(db.clone());
        Ok(Self { db, audit, events })
    }

    /// Fixed lifecycle facts, never stored input/result/free-form progress text.
    /// Exact current tasks.view is checked in this SAME read snapshot.
    pub fn get(
        &self,
        subject: &Subject,
        id: &OperationId,
    ) -> OperationsResult<Option<OperationSummary>> {
        self.db
            .read_with(|snapshot| {
                let now = self.db.clock().millis();
                let repo = Repository::new(snapshot.connection(), self.db.server_id(), now);
                self.task_access(&repo, subject)?;
                repo.summary(id)
            })
            .map_err(OperationError::from)
    }

    /// One bounded snapshot page. This is not event replay, a fixed-horizon
    /// subscription cursor, or a state-and-journal synchronization contract.
    pub fn list(
        &self,
        subject: &Subject,
        after_seq: i64,
        limit: usize,
    ) -> OperationsResult<OperationPage> {
        if after_seq < 0 || !(1..=MAX_PAGE).contains(&limit) {
            return Err(invalid());
        }
        self.db
            .read_with(|snapshot| {
                let now = self.db.clock().millis();
                let repo = Repository::new(snapshot.connection(), self.db.server_id(), now);
                self.task_access(&repo, subject)?;
                let items = repo.summaries(after_seq, limit)?;
                let next_after_seq = items.last().map_or(after_seq, |item| item.seq);
                Ok(OperationPage {
                    items,
                    next_after_seq,
                })
            })
            .map_err(OperationError::from)
    }

    /// tasks.view is ONLY task access, not mutation authority. A nonterminal
    /// cancellation also requires the exact initiating principal/device and ALL
    /// current requirements of the verified canonical operation binding.
    /// Unknown production bindings fail closed. Claimed work retains token/locks;
    /// this method cannot acknowledge a running worker's exit or safe point.
    pub fn request_cancel(
        &self,
        subject: &Subject,
        id: &OperationId,
    ) -> OperationsResult<CancelOutcome> {
        let result = self.db.write_with(|transaction| {
            let repo = Repository::new(
                transaction.connection(),
                self.db.server_id(),
                self.db.clock().millis(),
            );
            self.task_access(&repo, subject)?;
            let op = repo.require(id)?;
            if op.subject != *subject {
                return Err(OperationError::rejected(OperationErrorCode::ActorMismatch));
            }
            repo.check_locks(&op)?;
            if op.summary.state.is_terminal() {
                return Ok(CancelOutcome::AlreadyTerminal(op.summary));
            }
            if op.summary.state == OperationState::RecoveryRequired {
                return Err(OperationError::rejected(
                    OperationErrorCode::RecoveryRequired,
                ));
            }
            self.verify_binding(&repo, &op)?;
            let actor = request_actor(subject)?;
            match op.summary.state {
                OperationState::Queued | OperationState::AwaitingConfirmation => {
                    if op.effect_started {
                        return Err(domain::corrupt());
                    }
                    let after = repo.cancel_unclaimed(&op)?;
                    self.record_state(
                        &repo,
                        actor,
                        "operation.cancel_request",
                        &after,
                        op.summary.state,
                    )?;
                    Ok(CancelOutcome::Cancelled(after.summary))
                }
                OperationState::Validating | OperationState::Running => {
                    let after = repo.request_cancel(&op)?;
                    self.record_state(
                        &repo,
                        actor,
                        "operation.cancel_request",
                        &after,
                        op.summary.state,
                    )?;
                    Ok(CancelOutcome::Requested(after.summary))
                }
                OperationState::Cancelling => {
                    if !op.summary.cancel_requested || op.owner_token.is_none() {
                        return Err(domain::corrupt());
                    }
                    Ok(CancelOutcome::AlreadyRequested(op.summary))
                }
                _ => Err(OperationError::rejected(
                    OperationErrorCode::InvalidTransition,
                )),
            }
        });
        self.finish_request(result, subject, "operation.cancel_request", Some(id))
    }

    /// Typed request boundary for the closed lifecycle inventory. `Subject` is a
    /// trusted lookup key, not authentication. Launch configuration and all security
    /// declarations remain server-owned; current policy is checked before retries.
    pub fn submit_game(
        &self,
        subject: &Subject,
        command: GameCommand,
        idempotency_key: Option<&str>,
    ) -> OperationsResult<Admission> {
        self.admit(
            subject,
            command.operation_type(),
            &command.canonical_input()?,
            idempotency_key,
        )
    }

    /// Private canonical admission shared with non-executing unit fixtures. There
    /// is no transport-selected capability/target/locks/source/safety declaration.
    fn admit(
        &self,
        subject: &Subject,
        operation_type: &str,
        raw_input: &str,
        idempotency_key: Option<&str>,
    ) -> OperationsResult<Admission> {
        self.admit_with_view(subject, operation_type, raw_input, idempotency_key, None)
    }
    fn admit_with_view(
        &self,
        subject: &Subject,
        operation_type: &str,
        raw_input: &str,
        idempotency_key: Option<&str>,
        view: Option<&game::LiveGameView>,
    ) -> OperationsResult<Admission> {
        if idempotency_key.is_some_and(|key| !text(key, 128)) {
            let error = invalid();
            return Err(self.refusal(subject, "operation.submit", None, error));
        }
        let plan = match input::derive(operation_type, raw_input) {
            Ok(plan) => plan,
            Err(error @ OperationError::Rejected { .. }) => {
                return Err(self.refusal(subject, "operation.submit", None, error));
            }
            Err(error) => return Err(error),
        };
        let result = self.db.write_with(|transaction| {
            let repo = Repository::new(
                transaction.connection(),
                self.db.server_id(),
                self.db.clock().millis(),
            );
            // Resolve current authority BEFORE idempotent retrieval, not merely
            // before inserting a new row. Time is sampled after writer admission.
            self.authorize_plan(&repo, subject, &plan)?;
            if let Some(key) = idempotency_key
                && let Some(existing) = repo.find_key(&subject.principal, key)?
            {
                let resources = repo.check_locks(&existing)?;
                if existing.subject.device != subject.device
                    || existing.source != REQUEST_SOURCE
                    || existing.operation_type != plan.operation_type
                    || existing.target != plan.target
                    || existing.payload_hash != plan.payload_hash
                    || resources != plan.resources
                    || existing.effect_idempotent != plan.effect_idempotent
                    || existing.requires_confirmation != plan.requires_confirmation
                    || existing.idempotency_key.as_deref() != Some(key)
                {
                    return Err(OperationError::rejected(
                        OperationErrorCode::IdempotencyConflict,
                    ));
                }
                // Terminal retention is allowed to remove input. Nonterminal
                // retrieval must still reject a corrupted persisted binding.
                if !existing.summary.state.is_terminal() {
                    self.verify_binding(&repo, &existing)?;
                }
                return Ok(Admission::Existing(existing.summary));
            }
            if let Some(view) = view {
                queue::check_revision(
                    plan.command.as_ref().ok_or_else(invalid)?,
                    &view.revision()?,
                )?;
                if matches!(plan.command, Some(GameCommand::Console { .. }))
                    && view.snapshot()?.phase != ProcessPhase::Running
                {
                    return Err(OperationError::rejected(
                        OperationErrorCode::BindingMismatch,
                    ));
                }
            }
            let op = repo.insert(subject, &plan, idempotency_key)?;
            self.audit.append(
                repo.connection(),
                &AuditRecord::new(
                    request_actor(subject)?,
                    "operation.submit".to_owned(),
                    operation_target(&op.summary.id),
                    "accepted".to_owned(),
                    Some(op.summary.id.as_str().to_owned()),
                    [
                        (
                            "state".to_owned(),
                            Some(op.summary.state.as_str().to_owned()),
                        ),
                        ("payloadHash".to_owned(), Some(plan.payload_hash.clone())),
                    ],
                )?,
            )?;
            self.append_event(&repo, "operation.created", &op.summary)?;
            Ok(Admission::Created(op.summary))
        });
        self.finish_request(result, subject, "operation.submit", None)
    }

    /// Only concrete crate-owned composition may acquire native execution ownership.
    /// Claims and native guards retain THIS database directory's lock and lease.
    pub(crate) fn open_owner(&self) -> OperationsResult<OwnerSession> {
        OwnerSession::open(self)
    }

    /// Trusted retention, bounded per call. Recovery/other nonterminal inputs are
    /// never purged; summaries and audit are not deleted or given a 90-day policy.
    #[cfg(test)]
    pub(crate) fn purge_terminal_inputs(
        &self,
        finished_before_ms: i64,
        limit: usize,
    ) -> OperationsResult<PurgeReport> {
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(invalid());
        }
        self.db
            .write_with(|transaction| {
                let repo = Repository::new(
                    transaction.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                );
                let ids = repo.purge_page(finished_before_ms, limit)?;
                for id in &ids {
                    repo.purge(id, finished_before_ms)?;
                }
                if !ids.is_empty() {
                    self.audit.append(
                        repo.connection(),
                        &AuditRecord::new(
                            engine_actor()?,
                            "operation.inputs_purged".to_owned(),
                            "operations".to_owned(),
                            "ok".to_owned(),
                            None,
                            [
                                ("count".to_owned(), Some(ids.len().to_string())),
                                (
                                    "finished_before_ms".to_owned(),
                                    Some(finished_before_ms.to_string()),
                                ),
                            ],
                        )?,
                    )?;
                }
                Ok(PurgeReport { purged: ids.len() })
            })
            .map_err(OperationError::from)
    }

    fn policy(&self, repo: &Repository<'_>, subject: &Subject) -> OperationsResult<PolicySnapshot> {
        Ok(identity::policy_in(
            repo.connection(),
            self.db.server_id(),
            repo.now,
            Some(subject),
        )?)
    }
    fn task_access(&self, repo: &Repository<'_>, subject: &Subject) -> OperationsResult<()> {
        let decision = self
            .policy(repo, subject)?
            .check(Capability::TasksView, None);
        match decision {
            identity::Decision::Allow => Ok(()),
            identity::Decision::Deny(reason) => Err(OperationError::forbidden(reason)),
        }
    }
    fn authorize_plan(
        &self,
        repo: &Repository<'_>,
        subject: &Subject,
        plan: &PreparedOperation,
    ) -> OperationsResult<()> {
        if plan.requirements.is_empty() || plan.requirements.len() > MAX_REQUIREMENTS {
            return Err(OperationError::rejected(
                OperationErrorCode::BindingMismatch,
            ));
        }
        let policy = self.policy(repo, subject)?;
        for requirement in &plan.requirements {
            match policy.check(requirement.capability, requirement.resource.as_ref()) {
                identity::Decision::Allow => {}
                identity::Decision::Deny(reason) => return Err(OperationError::forbidden(reason)),
            }
        }
        Ok(())
    }
    fn verify_binding(
        &self,
        repo: &Repository<'_>,
        op: &StoredOperation,
    ) -> OperationsResult<PreparedOperation> {
        let plan = self.verified_plan(repo, op)?;
        self.authorize_plan(repo, &op.subject, &plan)?;
        Ok(plan)
    }

    /// Canonical integrity is distinct from authority. Used only for exact owner
    /// bookkeeping after a worker has returned; it NEVER grants an effect permit.
    fn verified_plan(
        &self,
        repo: &Repository<'_>,
        op: &StoredOperation,
    ) -> OperationsResult<PreparedOperation> {
        let canonical = repo
            .input(op)?
            .ok_or_else(|| OperationError::rejected(OperationErrorCode::InputUnavailable))?;
        if !hash_text(&op.payload_hash) || input::digest(&canonical) != op.payload_hash {
            return Err(OperationError::rejected(
                OperationErrorCode::InputUnavailable,
            ));
        }
        let plan = input::derive(&op.operation_type, &canonical)?;
        if plan.canonical_input != canonical
            || plan.payload_hash != op.payload_hash
            || op.source != REQUEST_SOURCE
            || plan.target != op.target
            || plan.resources != repo.resources(&op.summary.id)?
            || plan.effect_idempotent != op.effect_idempotent
            || plan.requires_confirmation != op.requires_confirmation
        {
            return Err(OperationError::rejected(
                OperationErrorCode::BindingMismatch,
            ));
        }
        Ok(plan)
    }
    fn record_state(
        &self,
        repo: &Repository<'_>,
        actor: AuditActor,
        action: &'static str,
        after: &StoredOperation,
        before: OperationState,
    ) -> OperationsResult<()> {
        self.audit.append(
            repo.connection(),
            &AuditRecord::new(
                actor,
                action.to_owned(),
                operation_target(&after.summary.id),
                "ok".to_owned(),
                Some(after.summary.id.as_str().to_owned()),
                [
                    ("from".to_owned(), Some(before.as_str().to_owned())),
                    (
                        "to".to_owned(),
                        Some(after.summary.state.as_str().to_owned()),
                    ),
                ],
            )?,
        )?;
        self.append_event(repo, "operation.state", &after.summary)
    }
    /// Exact-owner returned-outcome bookkeeping, not an effect authorization.
    /// Current denial is retained only as a fixed policy reason, never actor input.
    fn record_finish(
        &self,
        repo: &Repository<'_>,
        after: &StoredOperation,
        before: OperationState,
        authority_denial: Option<&'static str>,
    ) -> OperationsResult<()> {
        if after.operation_type == "game.console" {
            self.audit.append(
                repo.connection(),
                &AuditRecord::of(
                    engine_actor()?,
                    "console.command".into(),
                    operation_target(&after.summary.id),
                    after.summary.state.as_str().into(),
                    Some(after.summary.id.as_str().into()),
                )?,
            )?;
        }
        self.audit.append(
            repo.connection(),
            &AuditRecord::new(
                engine_actor()?,
                "operation.finish".to_owned(),
                operation_target(&after.summary.id),
                "ok".to_owned(),
                Some(after.summary.id.as_str().to_owned()),
                [
                    ("from".to_owned(), Some(before.as_str().to_owned())),
                    (
                        "to".to_owned(),
                        Some(after.summary.state.as_str().to_owned()),
                    ),
                    (
                        "current_authority".to_owned(),
                        Some(
                            if authority_denial.is_some() {
                                "denied"
                            } else {
                                "allowed"
                            }
                            .to_owned(),
                        ),
                    ),
                    (
                        "authority_denial".to_owned(),
                        authority_denial.map(str::to_owned),
                    ),
                ],
            )?,
        )?;
        self.append_event(repo, "operation.state", &after.summary)
    }
    #[cfg(test)]
    fn record_progress(
        &self,
        repo: &Repository<'_>,
        after: &StoredOperation,
    ) -> OperationsResult<()> {
        self.audit.append(
            repo.connection(),
            &AuditRecord::new(
                engine_actor()?,
                "operation.progress".to_owned(),
                operation_target(&after.summary.id),
                "ok".to_owned(),
                Some(after.summary.id.as_str().to_owned()),
                [(
                    "progress".to_owned(),
                    Some(after.summary.progress_percent.to_string()),
                )],
            )?,
        )?;
        self.append_event(repo, "operation.progress", &after.summary)
    }
    fn append_event(
        &self,
        repo: &Repository<'_>,
        event_type: &'static str,
        summary: &OperationSummary,
    ) -> OperationsResult<()> {
        let payload = serde_json::to_string(&LifecycleEvent {
            operation_id: summary.id.as_str(),
            state: summary.state.as_str(),
            progress: summary.progress_percent,
        })
        .map_err(|_| domain::corrupt())?;
        self.events.append(
            repo.connection(),
            event_type,
            Some(summary.id.as_str()),
            Some(&payload),
        )?;
        Ok(())
    }
    fn finish_request<T>(
        &self,
        result: std::result::Result<T, TransactionError<OperationError>>,
        subject: &Subject,
        action: &'static str,
        id: Option<&OperationId>,
    ) -> OperationsResult<T> {
        match result {
            Ok(value) => Ok(value),
            Err(TransactionError::Work(error @ OperationError::Rejected { .. })) => {
                Err(self.refusal(subject, action, id, error))
            }
            Err(other) => Err(other.into()),
        }
    }
    fn refusal(
        &self,
        subject: &Subject,
        action: &'static str,
        id: Option<&OperationId>,
        primary: OperationError,
    ) -> OperationError {
        let (code, reason) = match &primary {
            OperationError::Rejected { code, reason } => (*code, *reason),
            _ => return primary,
        };
        let cause = reason.map_or(code.code(), identity::DenyReason::code);
        let record = request_actor(subject).and_then(|actor| {
            AuditRecord::new(
                actor,
                action.to_owned(),
                id.map_or_else(|| "operations".to_owned(), operation_target),
                "rejected".to_owned(),
                id.map(|id| id.as_str().to_owned()),
                [("reason".to_owned(), Some(cause.to_owned()))],
            )
            .map_err(OperationError::from)
        });
        let audit = record.and_then(|record| {
            self.db
                .write_with(|transaction| {
                    self.audit
                        .append(transaction.connection(), &record)
                        .map_err(OperationError::from)
                })
                .map_err(OperationError::from)
        });
        match audit {
            Ok(_) => primary,
            Err(audit_failure) => OperationError::AuditUnavailable {
                primary: Box::new(primary),
                audit_failure: Box::new(audit_failure),
            },
        }
    }
}

#[derive(Serialize)]
struct LifecycleEvent<'a> {
    #[serde(rename = "operationId")]
    operation_id: &'a str,
    state: &'a str,
    progress: u8,
}
fn operation_target(id: &OperationId) -> String {
    format!("operation:{}", id.as_str())
}
fn request_actor(subject: &Subject) -> OperationsResult<AuditActor> {
    Ok(AuditActor::new(
        subject.principal.as_str().to_owned(),
        subject.device.as_str().to_owned(),
        REQUEST_SOURCE.to_owned(),
    )?)
}
fn engine_actor() -> OperationsResult<AuditActor> {
    Ok(AuditActor::system(ENGINE_SOURCE.to_owned())?)
}

#[cfg(test)]
mod tests;
