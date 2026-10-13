//! OS-owned scheduling and exact private execution ownership.
//! A claim is ownership, never cached authorization. Every run/native effect
//! resolves the current row, canonical input, policy and accepted receipt anew.
//! Claims and delayed native gates retain the database lease and actual directory
//! lock; dropping a session never fabricates a worker/native-write termination.
use super::{
    GameCommand, Operations, SourceRevision,
    confirmation::verify_receipt_in,
    domain::{
        MAX_RESULT_BYTES, OperationError, OperationErrorCode, OperationId, OperationState,
        OperationSummary, OperationsResult, corrupt, hash_text, identifier,
    },
    engine_actor, input,
    repository::{InputStatus, Repository, StoredOperation},
};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use vesta_state::{RunnerLock, RuntimeLease, StateError};

/// One owned concrete runtime. No public constructor or injected authority.
/// The core remains alive through actual claims and native request ownership.
pub(crate) struct OwnerSession {
    core: Arc<OwnerCore>,
}
struct OwnerCore {
    ops: Operations,
    _lock: RunnerLock,
    _lease: RuntimeLease,
    nonce: String,
    healthy: AtomicBool,
    stopping: Arc<AtomicBool>,
}
impl fmt::Debug for OwnerSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnerSession")
            .field("healthy", &self.healthy())
            .finish_non_exhaustive()
    }
}
impl Drop for OwnerSession {
    fn drop(&mut self) {
        // Delayed requests retain the lock/lease, but cannot start an effect once
        // their concrete owner is gone. There is NO durable transition on drop.
        self.stop_claiming();
    }
}

/// Opaque, non-Clone, nonserializable exact ownership. No token/session reaches
/// summaries, audit/events, errors or public APIs. Ownership is not execution authority.
pub(crate) struct Claim {
    core: Arc<OwnerCore>,
    id: OperationId,
    token: String,
    session: String,
}
impl Claim {
    pub(crate) fn id(&self) -> &OperationId {
        &self.id
    }
}
impl fmt::Debug for Claim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Claim[<redacted>]")
    }
}

/// A delayed native request owns this exact guard, not a cached permit. Its fresh
/// check is called by the native writer immediately before the actual write, with
/// a revision sampled from the concrete owner's current publication.
pub(crate) struct NativeEffectGuard {
    core: Arc<OwnerCore>,
    id: OperationId,
    token: String,
    session: String,
}
impl fmt::Debug for NativeEffectGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeEffectGuard[<redacted>]")
    }
}
impl NativeEffectGuard {
    pub(crate) fn begin_effect(&self, revision: &SourceRevision) -> OperationsResult<()> {
        self.core
            .begin_effect(&self.id, &self.token, &self.session, revision)
    }
}

/// Known results are distinct from uncertain effects. Only the concrete runtime
/// creates these fixed outcomes, after its worker/native receipt has actually
/// returned; neither a grace period nor dropping a handle is such a result.
pub(crate) enum FinishOutcome {
    Succeeded { result_json: Option<String> },
    Failed { code: &'static str },
    Cancelled,
    Uncertain { code: &'static str },
}

impl OwnerSession {
    pub(super) fn open(ops: &Operations) -> OperationsResult<Self> {
        // Retain persistence BEFORE reconciliation. Acquiring this lease neither
        // selects a different directory nor installs any authority.
        let lease = ops.db.retain_runtime()?;
        let lock = ops.db.directory().acquire_runner_lock()?;
        let nonce = random_token()?;
        reconcile(ops)?;
        Ok(Self {
            core: Arc::new(OwnerCore {
                ops: ops.clone(),
                _lock: lock,
                _lease: lease,
                nonce,
                healthy: AtomicBool::new(true),
                stopping: Arc::new(AtomicBool::new(false)),
            }),
        })
    }
    pub(crate) fn operations(&self) -> &Operations {
        &self.core.ops
    }
    pub(crate) fn healthy(&self) -> bool {
        self.core.healthy.load(Ordering::Acquire)
    }
    pub(crate) fn stop_claiming(&self) {
        self.core.stopping.store(true, Ordering::Release);
    }
    /// Shared with the autonomous driver's shutdown request so queued writers see
    /// stopping synchronously, rather than waiting for another driver iteration.
    pub(crate) fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.core.stopping)
    }

    #[cfg(test)]
    pub(crate) fn claim_next(&mut self) -> OperationsResult<Option<Claim>> {
        self.claim_next_with_revision(&SourceRevision::for_game(None))
    }
    pub(crate) fn claim_next_with_revision(
        &mut self,
        revision: &SourceRevision,
    ) -> OperationsResult<Option<Claim>> {
        self.core.require_dispatch()?;
        self.core.write(|repo| {
            self.core.require_dispatch()?;
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
                    let verified = self.core.ops.verify_binding(repo, &op).and_then(|plan| {
                        receipt(repo, &op, revision)?;
                        Ok(plan)
                    });
                    match verified {
                        Ok(_) => {}
                        Err(OperationError::Rejected { code, .. }) => {
                            let failed = repo.finish_unstarted(
                                &op,
                                OperationState::Failed,
                                Some(code.code()),
                            )?;
                            self.core.ops.record_state(
                                repo,
                                engine_actor()?,
                                "operation.claim_refused",
                                &failed,
                                op.summary.state,
                            )?;
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                    // Every earlier queued row was checked in this transaction.
                    // Its full lockset reserves against later/transitive conflicts;
                    // independent work can pass, even across bounded queue pages.
                    if repo.conflicting(op.summary.seq, &resources)? {
                        continue;
                    }
                    self.core.require_dispatch()?;
                    let token = random_token()?;
                    let claimed = repo.claim(&op, &resources, &token)?;
                    self.core.ops.record_state(
                        repo,
                        engine_actor()?,
                        "operation.claim",
                        &claimed,
                        op.summary.state,
                    )?;
                    return Ok(Some(Claim {
                        core: Arc::clone(&self.core),
                        id,
                        token,
                        session: self.core.nonce.clone(),
                    }));
                }
            }
        })
    }

    pub(crate) fn begin_run(
        &mut self,
        claim: &Claim,
        revision: &SourceRevision,
    ) -> OperationsResult<GameCommand> {
        self.core.check_claim(claim)?;
        self.core.require_dispatch()?;
        self.core.write(|repo| {
            self.core.require_dispatch()?;
            let op = self
                .core
                .owned(repo, &claim.id, &claim.token, &claim.session)?;
            if op.summary.cancel_requested {
                return Err(rejected(OperationErrorCode::CancellationRequested));
            }
            if op.summary.state != OperationState::Validating || op.effect_started {
                return Err(rejected(OperationErrorCode::InvalidTransition));
            }
            let plan = self.core.ops.verify_binding(repo, &op)?;
            receipt(repo, &op, revision)?;
            let command = plan
                .command
                .ok_or_else(|| rejected(OperationErrorCode::UnknownType))?;
            check_revision(&command, revision)?;
            let after = repo.mark_running(&op)?;
            self.core.ops.record_state(
                repo,
                engine_actor()?,
                "operation.run",
                &after,
                op.summary.state,
            )?;
            Ok(command)
        })
    }
    /// Second native step of one explicit restart, under its original exact claim.
    pub(crate) fn continue_restart(
        &mut self,
        claim: &Claim,
        revision: &SourceRevision,
    ) -> OperationsResult<()> {
        self.core.check_claim(claim)?;
        self.core.require_dispatch()?;
        self.core.write(|repo| {
            self.core.require_dispatch()?;
            let op = self
                .core
                .owned(repo, &claim.id, &claim.token, &claim.session)?;
            if op.summary.cancel_requested
                || op.summary.state != OperationState::Running
                || !op.effect_started
                || *revision != SourceRevision::for_game(None)
            {
                return Err(rejected(OperationErrorCode::InvalidTransition));
            }
            let plan = self.core.ops.verify_binding(repo, &op)?;
            if !matches!(plan.command, Some(GameCommand::Restart { .. })) {
                return Err(rejected(OperationErrorCode::BindingMismatch));
            }
            self.core.ops.record_state(
                repo,
                engine_actor()?,
                "operation.restart_start",
                &op,
                op.summary.state,
            )?;
            Ok(())
        })
    }
    pub(crate) fn begin_effect(
        &mut self,
        claim: &Claim,
        revision: &SourceRevision,
    ) -> OperationsResult<()> {
        self.core.check_claim(claim)?;
        self.core
            .begin_effect(&claim.id, &claim.token, &claim.session, revision)
    }
    pub(crate) fn native_effect_guard(&self, claim: &Claim) -> OperationsResult<NativeEffectGuard> {
        self.core.check_claim(claim)?;
        self.core.require_dispatch()?;
        Ok(NativeEffectGuard {
            core: Arc::clone(&self.core),
            id: claim.id.clone(),
            token: claim.token.clone(),
            session: claim.session.clone(),
        })
    }

    #[cfg(test)]
    pub(crate) fn report_progress(
        &mut self,
        claim: &Claim,
        percent: u8,
    ) -> OperationsResult<OperationSummary> {
        self.core.check_claim(claim)?;
        if percent > 100 {
            return Err(super::domain::invalid());
        }
        self.core.write(|repo| {
            let op = self
                .core
                .owned(repo, &claim.id, &claim.token, &claim.session)?;
            self.core.ops.verify_binding(repo, &op)?;
            let after = repo.progress(&op, percent)?;
            self.core.ops.record_progress(repo, &after)?;
            Ok(after.summary)
        })
    }

    /// Safe unstarted release is allowed during shutdown, but never for a started
    /// effect. Current canonical/policy refusals cannot requeue unauthorized work.
    pub(crate) fn release_unstarted(&mut self, claim: Claim) -> OperationsResult<OperationSummary> {
        self.core.check_claim(&claim)?;
        self.core.write(|repo| {
            let op = self
                .core
                .owned(repo, &claim.id, &claim.token, &claim.session)?;
            if op.effect_started {
                return Err(rejected(OperationErrorCode::RecoveryRequired));
            }
            let verified = self.core.ops.verify_binding(repo, &op);
            let after = match verified {
                Err(OperationError::Rejected { code, .. }) => {
                    repo.finish_unstarted(&op, OperationState::Failed, Some(code.code()))?
                }
                Err(error) => return Err(error),
                Ok(_) if op.summary.cancel_requested => {
                    if op.summary.state != OperationState::Cancelling {
                        return Err(corrupt());
                    }
                    repo.finish_unstarted(&op, OperationState::Cancelled, None)?
                }
                Ok(_) => {
                    if op.summary.state == OperationState::Cancelling {
                        return Err(corrupt());
                    }
                    repo.requeue_unstarted(&op)?
                }
            };
            self.core.ops.record_state(
                repo,
                engine_actor()?,
                "operation.release_unstarted",
                &after,
                op.summary.state,
            )?;
            Ok(after.summary)
        })
    }

    pub(crate) fn finish(
        &mut self,
        claim: Claim,
        outcome: FinishOutcome,
    ) -> OperationsResult<OperationSummary> {
        self.core.check_claim(&claim)?;
        // Reject oversized/free-form result material before any transaction or
        // formatting. Public summaries/replay never project this stored payload.
        let (terminal, code, result) = match &outcome {
            FinishOutcome::Succeeded { result_json } => {
                if let Some(json) = result_json {
                    if json.len() > MAX_RESULT_BYTES {
                        return Err(rejected(OperationErrorCode::ResultTooLarge));
                    }
                    if json.contains('\0')
                        || serde_json::from_str::<serde_json::Value>(json).is_err()
                    {
                        return Err(super::domain::invalid());
                    }
                }
                (
                    Some(OperationState::Succeeded),
                    None,
                    result_json.as_deref(),
                )
            }
            FinishOutcome::Failed { code } => (Some(OperationState::Failed), Some(*code), None),
            FinishOutcome::Cancelled => (Some(OperationState::Cancelled), None, None),
            FinishOutcome::Uncertain { code } => (None, Some(*code), None),
        };
        if code.is_some_and(|code| !identifier(code, 64)) {
            return Err(super::domain::invalid());
        }
        self.core.write(|repo| {
            let op = self
                .core
                .owned(repo, &claim.id, &claim.token, &claim.session)?;
            let plan = self.core.ops.verified_plan(repo, &op)?;
            // This fresh resolution cannot authorize any new effect. A clean
            // revocation cannot erase a known returned native result; resolver/
            // infrastructure faults still roll back and stop the owner normally.
            let authority_denial = match self.core.ops.authorize_plan(repo, &op.subject, &plan) {
                Ok(()) => None,
                Err(OperationError::Rejected { code, reason }) => Some((
                    code,
                    reason.map_or(code.code(), crate::identity::DenyReason::code),
                )),
                Err(error) => return Err(error),
            };
            let after = match &outcome {
                FinishOutcome::Cancelled if op.effect_started => {
                    return Err(rejected(OperationErrorCode::RecoveryRequired));
                }
                FinishOutcome::Succeeded { .. } if !op.effect_started => {
                    return Err(rejected(OperationErrorCode::InvalidTransition));
                }
                _ if !op.effect_started && authority_denial.is_some() => {
                    let denial_code = authority_denial.ok_or_else(corrupt)?.0;
                    repo.finish_owned(&op, OperationState::Failed, Some(denial_code.code()), None)?
                }
                FinishOutcome::Uncertain { .. } if op.effect_started => {
                    repo.recovery_required(&op)?
                }
                FinishOutcome::Uncertain { .. } => {
                    repo.finish_unstarted(&op, OperationState::Failed, code)?
                }
                _ => repo.finish_owned(&op, terminal.ok_or_else(corrupt)?, code, result)?,
            };
            self.core.ops.record_finish(
                repo,
                &after,
                op.summary.state,
                authority_denial.map(|(_, reason)| reason),
            )?;
            Ok(after.summary)
        })
    }
}

impl OwnerCore {
    fn require_healthy(&self) -> OperationsResult<()> {
        if !self.healthy.load(Ordering::Acquire) {
            return Err(StateError::new("state_operation_owner_unhealthy").into());
        }
        Ok(())
    }
    fn require_dispatch(&self) -> OperationsResult<()> {
        self.require_healthy()?;
        if self.stopping.load(Ordering::Acquire) {
            return Err(rejected(OperationErrorCode::OwnerStopping));
        }
        Ok(())
    }
    fn check_claim(self: &Arc<Self>, claim: &Claim) -> OperationsResult<()> {
        if !Arc::ptr_eq(self, &claim.core) || claim.session != self.nonce {
            return Err(rejected(OperationErrorCode::NotOwner));
        }
        self.require_healthy()
    }
    fn owned(
        &self,
        repo: &Repository<'_>,
        id: &OperationId,
        token: &str,
        session: &str,
    ) -> OperationsResult<StoredOperation> {
        if session != self.nonce {
            return Err(rejected(OperationErrorCode::NotOwner));
        }
        let op = repo.require(id)?;
        if !op.summary.state.is_claimed() || op.owner_token.as_deref() != Some(token) {
            return Err(rejected(OperationErrorCode::NotOwner));
        }
        repo.check_locks(&op)?;
        Ok(op)
    }
    fn write<T>(
        &self,
        work: impl FnOnce(&Repository<'_>) -> OperationsResult<T>,
    ) -> OperationsResult<T> {
        self.require_healthy()?;
        let result = self
            .ops
            .db
            .write_with(|transaction| {
                self.require_healthy()?;
                let repo = Repository::new(
                    transaction.connection(),
                    self.ops.db.server_id(),
                    self.ops.db.clock().millis(),
                );
                work(&repo)
            })
            .map_err(OperationError::from);
        if matches!(&result, Err(error) if !matches!(error, OperationError::Rejected { .. })) {
            // State/audit/commit/policy infrastructure faults stop ALL future
            // dispatch/native guards. No rollback failure becomes normal denial.
            self.healthy.store(false, Ordering::Release);
            self.stopping.store(true, Ordering::Release);
        }
        result
    }
    fn begin_effect(
        &self,
        id: &OperationId,
        token: &str,
        session: &str,
        revision: &SourceRevision,
    ) -> OperationsResult<()> {
        self.require_dispatch()?;
        self.write(|repo| {
            self.require_dispatch()?;
            let op = self.owned(repo, id, token, session)?;
            if op.summary.cancel_requested {
                return Err(rejected(OperationErrorCode::CancellationRequested));
            }
            if op.summary.state != OperationState::Running || op.effect_started {
                return Err(rejected(OperationErrorCode::InvalidTransition));
            }
            let plan = self.ops.verify_binding(repo, &op)?;
            receipt(repo, &op, revision)?;
            let command = plan
                .command
                .ok_or_else(|| rejected(OperationErrorCode::UnknownType))?;
            check_revision(&command, revision)?;
            self.require_dispatch()?;
            let after = repo.mark_effect(&op)?;
            self.ops.record_state(
                repo,
                engine_actor()?,
                "operation.effect_start",
                &after,
                op.summary.state,
            )?;
            Ok(())
        })
    }
}

fn receipt(
    repo: &Repository<'_>,
    op: &StoredOperation,
    revision: &SourceRevision,
) -> OperationsResult<()> {
    if !op.requires_confirmation {
        return Ok(());
    }
    // Preserve the old negative fixture: changing a fixture's awaiting row to
    // queued cannot make it executable. There is no fixture token issuer/effect.
    #[cfg(test)]
    if op.operation_type.starts_with("fixture.") {
        return Err(rejected(OperationErrorCode::InvalidTransition));
    }
    verify_receipt_in(repo.connection(), repo.server, op, revision, repo.now)
}
pub(super) fn check_revision(
    command: &GameCommand,
    revision: &SourceRevision,
) -> OperationsResult<()> {
    let expected = match command {
        GameCommand::Start => SourceRevision::for_game(None),
        GameCommand::Stop {
            expected_generation,
        }
        | GameCommand::Restart {
            expected_generation,
        }
        | GameCommand::Moderate {
            expected_generation,
            ..
        }
        | GameCommand::Console {
            expected_generation,
            ..
        }
        | GameCommand::ForceKill {
            expected_generation,
        } => SourceRevision::for_game(Some(*expected_generation)),
    };
    if *revision != expected {
        return Err(rejected(OperationErrorCode::BindingMismatch));
    }
    Ok(())
}
fn rejected(code: OperationErrorCode) -> OperationError {
    OperationError::rejected(code)
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
                        continue;
                    }
                    let after = if op.effect_started {
                        // A stored idempotency bit is NOT proof that native workers
                        // returned. On restart all uncertain started work is retained.
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
                            Ok(plan) if !plan.requires_confirmation || plan.command.is_some() => {
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
        core: Arc::clone(&claim.core),
        id: claim.id.clone(),
        token,
        session: claim.session.clone(),
    }
}
/// A non-authoritative negative witness retains no runtime core or OS lock.
#[cfg(test)]
pub(super) struct StaleWitness {
    id: OperationId,
    token: String,
    session: String,
}
#[cfg(test)]
pub(super) fn stale_witness(claim: &Claim) -> StaleWitness {
    StaleWitness {
        id: claim.id.clone(),
        token: claim.token.clone(),
        session: claim.session.clone(),
    }
}
#[cfg(test)]
pub(super) fn forge_stale(current: &Claim, stale: StaleWitness) -> Claim {
    assert_ne!(
        current.session, stale.session,
        "negative witness must be from another session"
    );
    Claim {
        core: Arc::clone(&current.core),
        id: stale.id,
        token: stale.token,
        session: stale.session,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Admission, game::GameLifecycle};
    use super::*;
    use crate::identity::{Capability, DeviceId, PrincipalId, Subject};
    use rusqlite::params;
    use sha2::{Digest, Sha256};
    use std::time::{Duration, Instant};
    use tempfile::TempDir;
    use vesta_game_runtime::process::{GracefulStop, LaunchSpec};
    use vesta_state::{Clock, StateDatabase, StateDirectory};

    struct FixedClock;
    impl Clock for FixedClock {
        fn millis(&self) -> i64 {
            1_791_115_200_000
        }
    }
    struct Fixture {
        root: TempDir,
        db: StateDatabase,
        ops: Operations,
        subject: Subject,
    }
    impl Fixture {
        fn open() -> Self {
            let root = tempfile::tempdir().expect("native kernel test root");
            let directory =
                StateDirectory::new(root.path().join("state"), None).expect("protect state");
            let db = StateDatabase::open(directory, Arc::new(FixedClock)).expect("actual database");
            let subject = Subject {
                principal: PrincipalId::parse(&uuid::Uuid::new_v4().to_string())
                    .expect("principal lookup"),
                device: DeviceId::parse(&uuid::Uuid::new_v4().to_string()).expect("device lookup"),
            };
            // Test-only protected database facts. Exact named grants, no role/rank
            // promotions, production proof issuer or injected authority provider.
            db.write(|connection| {
                connection.execute(
                    "INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Native kernel fixture','active',?,?)",
                    params![subject.principal.as_str(), db.server_id(), db.clock().millis(), db.clock().millis()],
                )?;
                connection.execute(
                    "INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Native kernel fixture',?,?)",
                    params![subject.device.as_str(), db.server_id(), subject.principal.as_str(), hex::encode(Sha256::digest(subject.device.as_str().as_bytes())), db.clock().millis()],
                )?;
                for capability in [Capability::LifecycleStart, Capability::TasksView] {
                    connection.execute(
                        "INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?,?,'*','allow','fixture',?)",
                        params![uuid::Uuid::new_v4().to_string(), subject.principal.as_str(), capability.id(), db.clock().millis()],
                    )?;
                }
                Ok(())
            }).expect("isolated exact test grants");
            let ops = Operations::new(db.clone()).expect("real audit chain");
            Self {
                root,
                db,
                ops,
                subject,
            }
        }
        fn game(&self) -> GameLifecycle {
            let spec = LaunchSpec::new(
                std::env::current_exe().expect("test executable"),
                vec![
                    "--exact".to_owned(),
                    "operations::queue::tests::native_child_fixture".to_owned(),
                    "--nocapture".to_owned(),
                    "--test-threads=1".to_owned(),
                ],
                self.root.path().to_owned(),
                GracefulStop::Unsupported,
            )
            .expect("trusted literal bounded subprocess fixture");
            GameLifecycle::new(spec)
        }
        fn start_request(&self) -> OperationSummary {
            match self
                .ops
                .submit_game(&self.subject, GameCommand::Start, Some("start"))
                .expect("current exact Start grant")
            {
                Admission::Created(summary) => summary,
                _ => panic!("new request"),
            }
        }
        fn revoke_start(&self) {
            self.db.write(|connection| {
                connection.execute(
                    "INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?,?,'*','deny','fixture',?)",
                    params![uuid::Uuid::new_v4().to_string(), self.subject.principal.as_str(), Capability::LifecycleStart.id(), self.db.clock().millis()],
                )?;
                Ok(())
            }).expect("current exact capability revoked");
        }
        fn check_returned_fact(&self, id: &OperationId, state: OperationState, started: bool) {
            self.db.read(|connection| {
                let stored: (String, bool, Option<String>) = connection.query_row(
                    "SELECT state,external_effect_started,owner_token FROM operations WHERE id=?",
                    [id.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                assert_eq!(stored, (state.as_str().to_owned(), started, None));
                let held: i64 = connection.query_row("SELECT count(*) FROM resource_locks WHERE operation_id=?",
                    [id.as_str()], |row| row.get(0))?;
                assert_eq!(held, 0);
                let metadata: String = connection.query_row(
                    "SELECT metadata_json FROM audit_events WHERE operation_id=? AND action='operation.finish' ORDER BY seq DESC LIMIT 1",
                    [id.as_str()], |row| row.get(0),
                )?;
                let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("fixed audit JSON");
                assert_eq!(metadata["current_authority"], "denied");
                assert_eq!(metadata["authority_denial"], "explicit_deny");
                let payload: String = connection.query_row(
                    "SELECT payload_json FROM events WHERE operation_id=? AND type='operation.state' ORDER BY cursor DESC LIMIT 1",
                    [id.as_str()], |row| row.get(0),
                )?;
                let payload: serde_json::Value = serde_json::from_str(&payload).expect("sanitized lifecycle fact");
                assert_eq!(payload["state"], state.as_str());
                Ok(())
            }).expect("facts, released locks, fixed denial audit and event committed together");
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let closed = self.db.close();
            if !std::thread::panicking() {
                closed.expect("actual database drains after owned workers");
            }
        }
    }

    /// Real bounded native child, not an executable operation fixture/handler.
    #[test]
    fn native_child_fixture() {
        std::thread::sleep(Duration::from_millis(100));
    }

    #[test]
    fn lost_returned_native_result_remains_recovery_after_current_revocation() {
        let fixture = Fixture::open();
        let game = fixture.game();
        let row = fixture.start_request();
        let mut owner = fixture.ops.open_owner().expect("real OS ownership");
        let claim = owner
            .claim_next_with_revision(&game.revision())
            .expect("claim")
            .expect("request");
        owner
            .begin_run(&claim, &game.revision())
            .expect("authorized run");
        let native = owner
            .native_effect_guard(&claim)
            .expect("exact native request core");
        let (returned, receipt) =
            std::sync::mpsc::channel::<vesta_game_runtime::process::GenerationId>();
        let worker = std::thread::spawn(move || {
            let mut game = game;
            let _generation = game
                .start(|revision| native.begin_effect(revision))
                .expect("actual guarded native spawn, not a fabricated marker");
            // Fault the result handoff, not the binding/effect. Controller receives
            // no typed generation result despite actual native mutation returning.
            drop(returned);
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                game.observe().expect("actual child/IO completion");
                if !game.has_owned_generation() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "bounded native fixture terminates"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        receipt
            .recv()
            .expect_err("actual worker result channel was lost");
        worker
            .join()
            .expect("worker returned and actual owned child/IO joined");
        fixture.revoke_start();
        assert_eq!(
            owner
                .finish(
                    claim,
                    FinishOutcome::Uncertain {
                        code: "native_result_lost"
                    }
                )
                .expect("current denial cannot fabricate known terminal effect outcome")
                .state,
            OperationState::RecoveryRequired
        );
        fixture.db.read(|connection| {
            let stored: (String, bool, Option<String>, Option<String>, String) = connection.query_row(
                "SELECT state,external_effect_started,owner_token,input_json,input_status FROM operations WHERE id=?",
                [row.id.as_str()], |record| Ok((record.get(0)?, record.get(1)?, record.get(2)?, record.get(3)?, record.get(4)?)),
            )?;
            assert_eq!(stored.0, "recovery_required");
            assert!(stored.1 && stored.2.is_none());
            assert_eq!(stored.3.as_deref(), Some("{}"));
            assert_eq!(stored.4, "available");
            let held: Vec<String> = connection.prepare("SELECT resource FROM resource_locks WHERE operation_id=? ORDER BY resource")?
                .query_map([row.id.as_str()], |record| record.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(held, vec![super::super::bindings::GAME_LOCK.to_owned()]);
            let metadata: String = connection.query_row(
                "SELECT metadata_json FROM audit_events WHERE operation_id=? AND action='operation.finish' ORDER BY seq DESC LIMIT 1",
                [row.id.as_str()], |record| record.get(0),
            )?;
            let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("fixed audit facts");
            assert_eq!(metadata["current_authority"], "denied");
            assert_eq!(metadata["authority_denial"], "explicit_deny");
            assert_eq!(metadata["to"], "recovery_required");
            Ok(())
        }).expect("uncertainty retains exact locks/input without owner or terminal fiction");
        assert_eq!(
            fixture
                .ops
                .purge_terminal_inputs(fixture.db.clock().millis() + 1, 10)
                .expect("bounded staged retention")
                .purged,
            0
        );
        drop(owner);
        let owner = fixture
            .ops
            .open_owner()
            .expect("next actual owner only reconciles");
        assert_eq!(
            fixture
                .ops
                .get(&fixture.subject, &row.id)
                .expect("TasksView")
                .expect("retained row")
                .state,
            OperationState::RecoveryRequired
        );
        drop(owner);
    }

    #[test]
    fn staged_progress_is_bounded_current_and_emits_only_fixed_facts() {
        let fixture = Fixture::open();
        let game = fixture.game();
        let row = fixture.start_request();
        let mut owner = fixture.ops.open_owner().expect("real OS ownership");
        let claim = owner
            .claim_next_with_revision(&game.revision())
            .expect("claim")
            .expect("request");
        owner
            .begin_run(&claim, &game.revision())
            .expect("authorized run");
        assert_eq!(
            owner
                .report_progress(&claim, 34)
                .expect("bounded stage fact")
                .progress_percent,
            34
        );
        assert_eq!(
            owner
                .report_progress(&claim, 101)
                .expect_err("bound")
                .code(),
            "invalid_request"
        );
        let wrong = forge_token(&claim);
        assert_eq!(
            owner
                .report_progress(&wrong, 35)
                .expect_err("exact token")
                .code(),
            "not_owner"
        );
        fixture.revoke_start();
        assert_eq!(
            owner
                .report_progress(&claim, 35)
                .expect_err("current policy")
                .code(),
            "forbidden"
        );
        fixture.db.read(|connection| {
            let payloads: Vec<String> = connection.prepare(
                "SELECT payload_json FROM events WHERE operation_id=? AND type='operation.progress' ORDER BY cursor",
            )?.query_map([row.id.as_str()], |record| record.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(payloads.len(), 1);
            let payload: serde_json::Value = serde_json::from_str(&payloads[0]).expect("fixed facts");
            assert_eq!(payload.as_object().expect("object facts").len(), 3);
            assert_eq!(payload["operationId"], row.id.as_str());
            assert_eq!(payload["state"], "running");
            assert_eq!(payload["progress"], 34);
            let stored: u8 = connection.query_row("SELECT progress_percent FROM operations WHERE id=?",
                [row.id.as_str()], |record| record.get(0))?;
            assert_eq!(stored, 34);
            Ok(())
        }).expect("rejected updates produce no durable/journal mutation");
        owner
            .finish(claim, FinishOutcome::Failed { code: "forbidden" })
            .expect("safe returned refusal");
    }

    #[test]
    fn revocation_at_delayed_native_gate_records_clean_unstarted_failure() {
        let fixture = Fixture::open();
        let game = fixture.game();
        let row = fixture.start_request();
        let mut owner = fixture.ops.open_owner().expect("real OS ownership");
        let claim = owner
            .claim_next_with_revision(&game.revision())
            .expect("claim")
            .expect("request");
        owner
            .begin_run(&claim, &game.revision())
            .expect("authorized run");
        owner
            .report_progress(&claim, 7)
            .expect("current exact authority for staged bounded progress");
        let native = owner
            .native_effect_guard(&claim)
            .expect("exact queued request ownership");
        fixture.revoke_start();
        assert_eq!(
            owner
                .report_progress(&claim, 8)
                .expect_err("progress is not retained authority")
                .code(),
            "forbidden"
        );
        let error = native
            .begin_effect(&game.revision())
            .expect_err("fresh native-time policy refuses");
        assert_eq!(error.code(), "forbidden");
        let finished = owner
            .finish(claim, FinishOutcome::Failed { code: error.code() })
            .expect("ordinary refusal bookkeeping does not require an effect permit");
        assert_eq!(finished.state, OperationState::Failed);
        fixture.check_returned_fact(&row.id, OperationState::Failed, false);
        assert_eq!(
            native
                .begin_effect(&game.revision())
                .expect_err("terminalized request has no cached effect permit")
                .code(),
            "not_owner"
        );
        assert!(!game.has_owned_generation());
    }

    #[test]
    fn revocation_after_returned_native_start_preserves_fact_but_grants_no_new_effect() {
        let fixture = Fixture::open();
        let mut game = fixture.game();
        let row = fixture.start_request();
        let mut owner = fixture.ops.open_owner().expect("real OS ownership");
        let claim = owner
            .claim_next_with_revision(&game.revision())
            .expect("claim")
            .expect("request");
        owner
            .begin_run(&claim, &game.revision())
            .expect("authorized run");
        // This is an actual concrete process spawn with the production guarded
        // effect callback, not a manually fabricated marker or fake handler.
        let generation = game
            .start(|revision| owner.begin_effect(&claim, revision))
            .expect("native spawn returned with retained generation/IO");
        fixture.revoke_start();
        let finished = owner
            .finish(
                claim,
                FinishOutcome::Succeeded {
                    result_json: Some(format!("{{\"generation\":\"{}\"}}", generation.to_hex())),
                },
            )
            .expect("known returned effect cannot be retroactively undone by policy");
        assert_eq!(finished.state, OperationState::Succeeded);
        fixture.check_returned_fact(&row.id, OperationState::Succeeded, true);
        assert_eq!(
            fixture
                .ops
                .submit_game(&fixture.subject, GameCommand::Start, Some("new-start"))
                .expect_err("bookkeeping granted no authority")
                .code(),
            "forbidden"
        );
        assert_eq!(
            fixture
                .db
                .close()
                .expect_err("actual process owner retains database")
                .code(),
            "state_runtime_busy"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            game.observe().expect("observe real child and joined IO");
            if !game.has_owned_generation() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "bounded fixture exited and joined"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
