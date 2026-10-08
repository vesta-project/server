//! Staged private kernel, compiled only by the parent module's `cfg(test)` gate.
//! This implementation is exercised only in crate unit-test builds; it is absent
//! from ordinary/integration-test library builds until trusted production
//! composition is reviewed. It is not a completed production operation runtime.
//! Synchronous, OS-lock-owned scheduling/reconciliation ONLY. A claim is not an
//! execution/effect permit. There are no worker threads/futures or drop-based
//! termination claims; future real workers need a separately reviewed supervisor.
use super::{
    Operations,
    domain::{
        OperationError, OperationErrorCode, OperationId, OperationState, OperationSummary,
        OperationsResult, corrupt, hash_text,
    },
    engine_actor, input,
    repository::{InputStatus, Repository},
};
use std::{fmt, marker::PhantomData, rc::Rc};
use vesta_state::{RunnerLock, StateError};

/// Crate-only and deliberately !Send/!Sync. Holds this database's OS lock through
/// all synchronous calls. Drop makes NO durable state transition. There are no
/// executing workers here whose lifetime could outlast this session.
pub(crate) struct OwnerSession<'a> {
    ops: &'a Operations,
    _lock: RunnerLock,
    nonce: String,
    healthy: bool,
    _same_thread: PhantomData<Rc<()>>,
}
impl fmt::Debug for OwnerSession<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnerSession")
            .field("healthy", &self.healthy)
            .finish_non_exhaustive()
    }
}

/// Opaque, non-Clone, nonserializable ownership for UNSTARTED work. Token/session
/// values never reach public summaries, audit, events or error formatting.
pub(crate) struct Claim {
    id: OperationId,
    token: String,
    session: String,
}
impl fmt::Debug for Claim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Claim[<redacted>]")
    }
}
impl<'a> OwnerSession<'a> {
    pub(super) fn open(ops: &'a Operations) -> OperationsResult<Self> {
        // No separately selected directory can be used to reconcile this DB.
        let lock = ops.db.directory().acquire_runner_lock()?;
        let nonce = random_token()?;
        reconcile(ops)?;
        Ok(OwnerSession {
            ops,
            _lock: lock,
            nonce,
            healthy: true,
            _same_thread: PhantomData,
        })
    }

    pub(crate) fn claim_next(&mut self) -> OperationsResult<Option<Claim>> {
        self.require_healthy()?;
        let result = self
            .ops
            .db
            .write_with(|transaction| {
                let repo = Repository::new(
                    transaction.connection(),
                    self.ops.db.server_id(),
                    self.ops.db.clock().millis(),
                );
                let mut after_seq = 0;
                loop {
                    let page = repo.queued_page(after_seq)?;
                    if page.is_empty() {
                        return Ok(None);
                    }
                    for (id, seq) in page {
                        after_seq = seq;
                        let op = repo.require(&id)?;
                        let resources = repo.check_locks(&op)?;
                        if op.summary.state != OperationState::Queued
                            || op.owner_token.is_some()
                            || op.effect_started
                            || op.summary.cancel_requested
                        {
                            return Err(corrupt());
                        }
                        let verified = self.ops.verify_binding(&repo, &op).and_then(|plan| {
                            if plan.requires_confirmation {
                                // No confirmation consumer exists in this increment;
                                // merely changing an awaiting row to queued is not proof.
                                Err(OperationError::rejected(
                                    OperationErrorCode::InvalidTransition,
                                ))
                            } else {
                                Ok(plan)
                            }
                        });
                        match verified {
                            Ok(_) => {}
                            Err(OperationError::Rejected { code, .. }) => {
                                let failed = repo.finish_unstarted(
                                    &op,
                                    OperationState::Failed,
                                    Some(code.code()),
                                )?;
                                self.ops.record_state(
                                    &repo,
                                    engine_actor()?,
                                    "operation.claim_refused",
                                    &failed,
                                    op.summary.state,
                                )?;
                                continue;
                            }
                            Err(error) => return Err(error),
                        }
                        // Every earlier row still queued was already checked in this
                        // transaction and is blocked. Its entire resource set reserves
                        // against later conflicting work, including transitive conflicts.
                        if repo.conflicting(op.summary.seq, &resources)? {
                            continue;
                        }
                        let token = random_token()?;
                        let claimed = repo.claim(&op, &resources, &token)?;
                        self.ops.record_state(
                            &repo,
                            engine_actor()?,
                            "operation.claim",
                            &claimed,
                            op.summary.state,
                        )?;
                        return Ok(Some(Claim {
                            id,
                            token,
                            session: self.nonce.clone(),
                        }));
                    }
                }
            })
            .map_err(OperationError::from);
        self.observe_failure(&result);
        result
    }

    /// Release only an exact claim whose durable marker says NO effect started.
    /// It cannot acknowledge safe cancellation/exit of a real executing worker.
    pub(crate) fn release_unstarted(&mut self, claim: Claim) -> OperationsResult<OperationSummary> {
        self.require_healthy()?;
        if claim.session != self.nonce {
            return Err(OperationError::rejected(OperationErrorCode::NotOwner));
        }
        let result = self
            .ops
            .db
            .write_with(|transaction| {
                let repo = Repository::new(
                    transaction.connection(),
                    self.ops.db.server_id(),
                    self.ops.db.clock().millis(),
                );
                let op = repo.require(&claim.id)?;
                if !op.summary.state.is_claimed()
                    || op.owner_token.as_deref() != Some(claim.token.as_str())
                {
                    return Err(OperationError::rejected(OperationErrorCode::NotOwner));
                }
                repo.check_locks(&op)?;
                if op.effect_started {
                    return Err(OperationError::rejected(
                        OperationErrorCode::RecoveryRequired,
                    ));
                }
                let after = if op.summary.cancel_requested {
                    if op.summary.state != OperationState::Cancelling {
                        return Err(corrupt());
                    }
                    repo.finish_unstarted(&op, OperationState::Cancelled, None)?
                } else {
                    if op.summary.state == OperationState::Cancelling {
                        return Err(corrupt());
                    }
                    repo.requeue_unstarted(&op)?
                };
                self.ops.record_state(
                    &repo,
                    engine_actor()?,
                    "operation.release_unstarted",
                    &after,
                    op.summary.state,
                )?;
                Ok(after.summary)
            })
            .map_err(OperationError::from);
        self.observe_failure(&result);
        result
    }
    fn require_healthy(&self) -> OperationsResult<()> {
        if !self.healthy {
            return Err(StateError::new("state_operation_owner_unhealthy").into());
        }
        Ok(())
    }
    fn observe_failure<T>(&mut self, result: &OperationsResult<T>) {
        if matches!(result, Err(error) if !matches!(error, OperationError::Rejected { .. })) {
            // No additional claim follows a state/audit/commit/policy infrastructure
            // failure. Leave durable rows/locks for a subsequent owned reconciliation.
            self.healthy = false;
        }
    }
}

fn reconcile(ops: &Operations) -> OperationsResult<()> {
    ops.db
        .write_with(|transaction| {
            let repo = Repository::new(
                transaction.connection(),
                ops.db.server_id(),
                ops.db.clock().millis(),
            );
            repo.check_foreign_or_terminal_locks()?;
            let mut after_seq = 0;
            loop {
                let page = repo.nonterminal_page(after_seq)?;
                if page.is_empty() {
                    return Ok(());
                }
                for (id, seq) in page {
                    after_seq = seq;
                    let op = repo.require(&id)?;
                    repo.check_locks(&op)?;
                    if op.summary.state == OperationState::RecoveryRequired {
                        // Never replay, terminalize, clear locks/input or consume a
                        // confirmation while reconciling a retained recovery row.
                        continue;
                    }
                    let after = if op.effect_started {
                        // In this no-handler increment even an idempotent stored bit
                        // is NOT proof of replay safety. All started uncertain work is
                        // preserved for explicit future recovery, never blindly replayed.
                        repo.recovery_required(&op)?
                    } else if op.summary.state == OperationState::Cancelling {
                        if !op.summary.cancel_requested {
                            return Err(corrupt());
                        }
                        repo.finish_unstarted(&op, OperationState::Cancelled, None)?
                    } else if op.summary.state.is_claimed() {
                        if op.summary.cancel_requested {
                            return Err(corrupt());
                        }
                        match ops.verify_binding(&repo, &op) {
                            Ok(plan) if !plan.requires_confirmation => {
                                repo.requeue_unstarted(&op)?
                            }
                            Ok(_) => repo.finish_unstarted(
                                &op,
                                OperationState::Failed,
                                Some("confirmation_unavailable"),
                            )?,
                            Err(OperationError::Rejected { code, .. }) => repo.finish_unstarted(
                                &op,
                                OperationState::Failed,
                                Some(code.code()),
                            )?,
                            Err(error) => return Err(error),
                        }
                    } else {
                        if op.owner_token.is_some() || op.summary.cancel_requested {
                            return Err(corrupt());
                        }
                        let available = if op.input_status == InputStatus::Available {
                            repo.input(&op)?.is_some_and(|canonical| {
                                hash_text(&op.payload_hash)
                                    && input::digest(&canonical) == op.payload_hash
                            })
                        } else {
                            false
                        };
                        if available {
                            continue;
                        }
                        repo.finish_unstarted(
                            &op,
                            OperationState::Failed,
                            Some("input_unavailable"),
                        )?
                    };
                    ops.record_state(
                        &repo,
                        engine_actor()?,
                        "operation.recover",
                        &after,
                        op.summary.state,
                    )?;
                }
            }
        })
        .map_err(OperationError::from)
}

fn random_token() -> OperationsResult<String> {
    let mut raw = [0_u8; 32];
    getrandom::fill(&mut raw).map_err(|_| StateError::new("state_random_unavailable"))?;
    Ok(hex::encode(raw))
}

#[cfg(test)]
pub(super) fn claim_id(claim: &Claim) -> &OperationId {
    &claim.id
}
#[cfg(test)]
pub(super) fn claim_token(claim: &Claim) -> &str {
    &claim.token
}
#[cfg(test)]
pub(super) fn forge_token(claim: &Claim) -> Claim {
    let mut token = claim.token.clone();
    let replacement = if token.starts_with('0') { "1" } else { "0" };
    token.replace_range(0..1, replacement);
    Claim {
        id: claim.id.clone(),
        token,
        session: claim.session.clone(),
    }
}
