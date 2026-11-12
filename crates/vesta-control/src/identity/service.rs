//! Trusted in-process identity mutation service, not enrollment or transport authentication.
//!
//! Each command owns one immediate transaction. Policy resolution, mutation and the
//! success audit share that transaction, so an audit failure rolls the authority change
//! back instead of reporting a mutation that was not recorded. A clean domain rejection
//! is rolled back first and only then recorded, in a separate transaction, as a refusal.
//! Infrastructure, rollback and refusal-audit failures stay errors; they are never
//! reported as a successful denial.
use super::authorization;
use super::local_authority::{Attestation, LocalAuthority, UnavailableAuthority};
use super::policy::{DenyReason, PolicySnapshot};
use super::repository::{GrantTable, Repository};
use super::{
    AuthorityChange, BuiltinRole, CodeLifetime, CustomRole, Decision, DeviceId, Effect,
    EpochMillis, Expiry, LocalCommand, LocalOutcome, LocalProof, OwnerAdded, PrincipalId, Purpose,
    RecoveryAction, RedeemOwner, RemoteCommand, RoleGrant, RoleId, Subject, SubjectRecord,
};
use super::{domain::MAX_PROOF_LIFETIME, domain::PROOF_CLOCK_SKEW, domain::valid_label};
use crate::{ControlError, IdentityErrorCode, Result};
use std::sync::Arc;
use vesta_state::StateDatabase;
use vesta_state::TransactionError;
use vesta_state::audit::{AuditActor, AuditLog, AuditRecord};

const SOURCE_LOCAL: &str = "local-authority";
const SOURCE_CODE: &str = "bootstrap-code";
const SOURCE_REMOTE: &str = "remote";
const SOURCE_EXPIRY: &str = "authority-expiry";

const ACTION_BOOTSTRAP_ISSUE: &str = "identity.bootstrap.issue";
const ACTION_RECOVERY_ISSUE: &str = "identity.recovery.issue";
const ACTION_OWNER_PAIRED: &str = "identity.owner.paired";
const ACTION_ROLE_ASSIGN: &str = "authority.role.assign";
const ACTION_OVERRIDE_SET: &str = "authority.override.set";
const ACTION_ROLE_CREATE: &str = "authority.role.create";
const ACTION_DEVICE_REVOKE: &str = "identity.device.revoke";
const ACTION_EXPIRED: &str = "authority.expired";

/// Epoch milliseconds at the persistence boundary. Audit metadata records this
/// integer rather than a reconstructed JDK `Instant` rendering.
const NONCE_MIN_BYTES: usize = 16;
const NONCE_MAX_BYTES: usize = 256;
const OS_ACTOR_MAX_SCALARS: usize = 128;

#[derive(Clone)]
pub struct IdentityService {
    db: StateDatabase,
    audit: AuditLog,
    local_authority: Arc<dyn LocalAuthority>,
}

impl IdentityService {
    /// Always verifies an AuditLog over this SAME database, and always selects
    /// unavailable local authority. There is no production verifier-injection seam,
    /// privileged flag or proof deserializer to bypass OS authentication.
    pub fn new(db: StateDatabase) -> Result<Self> {
        let audit = AuditLog::new(db.clone())?;
        Ok(Self {
            db,
            audit,
            local_authority: Arc::new(UnavailableAuthority),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_test_local_authority(
        db: StateDatabase,
        authority: Arc<dyn LocalAuthority>,
    ) -> Result<Self> {
        let mut service = Self::new(db)?;
        service.local_authority = authority;
        Ok(service)
    }

    // ------------------------------------------------------------------ observations

    /// Bootstrap is refused once any current server-bound active Owner assignment exists.
    pub fn owner_exists(&self) -> Result<bool> {
        self.observe(|repo| repo.owner_exists())
    }

    pub fn subject_record(&self, principal: &PrincipalId) -> Result<Option<SubjectRecord>> {
        self.observe(|repo| repo.record(principal))
    }

    /// A private-field observation of current authority for lookup keys. It cannot be
    /// granted, constructed by a caller or passed back into a command; every mutation
    /// reloads current policy inside its own transaction.
    pub fn current_policy(&self, subject: Option<&Subject>) -> Result<PolicySnapshot> {
        let subject = subject.cloned();
        self.observe(move |repo| repo.policy(subject.as_ref()))
    }

    fn observe<T>(&self, work: impl FnOnce(&Repository<'_>) -> Result<T>) -> Result<T> {
        self.db
            .read_with(|snapshot| {
                work(&Repository::new(
                    snapshot.connection(),
                    self.server(),
                    self.now(),
                ))
            })
            .map_err(ControlError::from)
    }

    // ------------------------------------------------------------------ local commands

    /// The only service path to a locally granted Owner or Administrator role, and the
    /// only way bootstrap or recovery is opened. The command value is already validated,
    /// so an out-of-range lifetime cannot consume a nonce or rotate a code.
    pub fn apply_local(
        &self,
        proof: Option<&LocalProof>,
        command: LocalCommand,
    ) -> Result<LocalOutcome> {
        match command {
            LocalCommand::Bootstrap(lifetime) => self.bootstrap(proof, lifetime),
            LocalCommand::Recover { lifetime, action } => self.recover(proof, lifetime, action),
            LocalCommand::AssignRole {
                principal,
                role,
                expiry,
            } => self.assign_role_locally(proof, principal, role, expiry),
        }
    }

    fn bootstrap(
        &self,
        proof: Option<&LocalProof>,
        lifetime: CodeLifetime,
    ) -> Result<LocalOutcome> {
        self.run(
            |repo| {
                let attestation = self.consume_proof(repo, proof, Purpose::OwnerBootstrap)?;
                if repo.owner_exists()? {
                    return Err(rejected(IdentityErrorCode::BootstrapClosed));
                }
                let code = repo.issue_code(Purpose::OwnerBootstrap, lifetime)?;
                self.audit_success(
                    repo,
                    local_actor()?,
                    ACTION_BOOTSTRAP_ISSUE,
                    "owner",
                    &[
                        ("os_actor", Some(attestation.os_actor)),
                        ("purpose", Some(Purpose::OwnerBootstrap.id().to_owned())),
                        ("expires_at_ms", Some(millis_text(code.expires_at_ms()))),
                    ],
                )?;
                Ok(LocalOutcome::Code(code))
            },
            Refusal {
                action: ACTION_BOOTSTRAP_ISSUE,
                target: "owner".to_owned(),
                actor: RefusalActor::Local,
            },
        )
    }

    /// Outstanding codes are invalidated either way. With revocation selected, every
    /// unrevoked Owner/Administrator assignment and every unrevoked device of a
    /// principal holding an active administrative assignment is revoked atomically.
    fn recover(
        &self,
        proof: Option<&LocalProof>,
        lifetime: CodeLifetime,
        action: RecoveryAction,
    ) -> Result<LocalOutcome> {
        self.run(
            |repo| {
                let attestation = self.consume_proof(repo, proof, Purpose::OwnerRecovery)?;
                let (assignments, devices) = match action {
                    RecoveryAction::KeepAccess => (0, 0),
                    RecoveryAction::RevokeAdministrativeAccess => repo.recover_access()?,
                };
                let code = repo.issue_code(Purpose::OwnerRecovery, lifetime)?;
                self.audit_success(
                    repo,
                    local_actor()?,
                    ACTION_RECOVERY_ISSUE,
                    "owner",
                    &[
                        ("os_actor", Some(attestation.os_actor)),
                        ("purpose", Some(Purpose::OwnerRecovery.id().to_owned())),
                        (
                            "revoke_admin_access",
                            Some(
                                matches!(action, RecoveryAction::RevokeAdministrativeAccess)
                                    .to_string(),
                            ),
                        ),
                        ("revoked_assignments", Some(assignments.to_string())),
                        ("revoked_devices", Some(devices.to_string())),
                        ("expires_at_ms", Some(millis_text(code.expires_at_ms()))),
                    ],
                )?;
                Ok(LocalOutcome::Code(code))
            },
            Refusal {
                action: ACTION_RECOVERY_ISSUE,
                target: "owner".to_owned(),
                actor: RefusalActor::Local,
            },
        )
    }

    /// Replaces every existing unrevoked primary-role assignment, including expired ones.
    fn assign_role_locally(
        &self,
        proof: Option<&LocalProof>,
        principal: PrincipalId,
        role: RoleId,
        expiry: Expiry,
    ) -> Result<LocalOutcome> {
        let target = principal_target(&principal);
        let success_target = target.clone();
        self.run(
            move |repo| {
                let attestation = self.consume_proof(repo, proof, Purpose::LocalRoleGrant)?;
                repo.active_principal(&principal)?;
                let role = repo.role(&role)?;
                let expires_at = expiry.deadline(repo.now)?;
                let assignment =
                    repo.replace_role(&principal, &role.id, expires_at, SOURCE_LOCAL)?;
                self.audit_success(
                    repo,
                    local_actor()?,
                    ACTION_ROLE_ASSIGN,
                    &success_target,
                    &[
                        ("os_actor", Some(attestation.os_actor)),
                        ("role", Some(role.id.as_str().to_owned())),
                        ("assignment", Some(assignment.as_str().to_owned())),
                        ("expires_at_ms", expires_at.map(millis_text)),
                    ],
                )?;
                Ok(LocalOutcome::Assignment(assignment))
            },
            Refusal {
                action: ACTION_ROLE_ASSIGN,
                target,
                actor: RefusalActor::Local,
            },
        )
    }

    /// Records a new Owner principal and device after a valid single-use code. This does
    /// NOT prove possession of the key: a future pairing transport must do that,
    /// rate-limit attempts and authenticate subsequent device requests.
    pub fn redeem_owner(&self, request: RedeemOwner) -> Result<OwnerAdded> {
        self.run(
            move |repo| {
                let RedeemOwner {
                    code,
                    name,
                    device_label,
                    fingerprint,
                } = request;
                let (code_id, purpose) = repo.consume_code(code.as_str())?;
                if purpose == Purpose::OwnerBootstrap && repo.owner_exists()? {
                    return Err(rejected(IdentityErrorCode::BootstrapClosed));
                }
                let principal = repo.insert_principal(&name)?;
                let device = repo.insert_device(&principal, &device_label, &fingerprint)?;
                let owner: RoleId = BuiltinRole::Owner.into();
                let assignment = repo.replace_role(
                    &principal,
                    &owner,
                    None,
                    &format!("{SOURCE_CODE}:{code_id}"),
                )?;
                self.audit_success(
                    repo,
                    AuditActor::new(
                        principal.as_str().to_owned(),
                        device.as_str().to_owned(),
                        SOURCE_CODE.to_owned(),
                    )?,
                    ACTION_OWNER_PAIRED,
                    &principal_target(&principal),
                    &[
                        ("purpose", Some(purpose.id().to_owned())),
                        ("code_id", Some(code_id)),
                        ("assignment", Some(assignment.as_str().to_owned())),
                    ],
                )?;
                Ok(OwnerAdded {
                    principal,
                    device,
                    assignment,
                })
            },
            Refusal {
                action: ACTION_OWNER_PAIRED,
                target: "owner".to_owned(),
                actor: RefusalActor::Code,
            },
        )
    }

    // ------------------------------------------------------------------ remote commands

    /// Transport-supplied identifiers are lookup keys only. Current authority is
    /// reloaded here, inside this transaction, before any effect.
    pub fn apply_remote(
        &self,
        subject: &Subject,
        command: RemoteCommand,
    ) -> Result<AuthorityChange> {
        match command {
            RemoteCommand::AssignRole {
                principal,
                role,
                expiry,
            } => self.assign_role(subject, principal, role, expiry),
            RemoteCommand::SetOverride {
                principal,
                grant,
                effect,
                expiry,
            } => self.set_override(subject, principal, grant, effect, expiry),
            RemoteCommand::RevokeAssignment(id) => {
                self.revoke_grant(subject, GrantTable::Assignments, id.as_str())
            }
            RemoteCommand::RevokeOverride(id) => {
                self.revoke_grant(subject, GrantTable::Overrides, id.as_str())
            }
            RemoteCommand::RevokeDevice(id) => self.revoke_device(subject, id),
            RemoteCommand::CreateRole(role) => self.create_role(subject, role),
        }
    }

    /// Strictly lower role and target rank, exact roles.assign, no self-target, reserved
    /// roles local only, and every granted capability contained in the actor.
    fn assign_role(
        &self,
        subject: &Subject,
        principal: PrincipalId,
        role: RoleId,
        expiry: Expiry,
    ) -> Result<AuthorityChange> {
        let target = principal_target(&principal);
        self.remote(
            subject,
            ACTION_ROLE_ASSIGN,
            target.clone(),
            move |repo, policy| {
                repo.active_principal(&principal)?;
                let role = repo.role(&role)?;
                let target_rank = repo.rank(&principal)?;
                enforce(authorization::assign(
                    policy,
                    &principal,
                    target_rank,
                    &role,
                ))?;
                let expires_at = expiry.deadline(repo.now)?;
                let assignment = repo.replace_role(
                    &principal,
                    &role.id,
                    expires_at,
                    authenticated_principal(policy)?,
                )?;
                self.audit_success(
                    repo,
                    remote_actor(policy)?,
                    ACTION_ROLE_ASSIGN,
                    &target,
                    &[
                        ("role", Some(role.id.as_str().to_owned())),
                        ("assignment", Some(assignment.as_str().to_owned())),
                        ("expires_at_ms", expires_at.map(millis_text)),
                    ],
                )?;
                Ok(AuthorityChange::Assignment(assignment))
            },
        )
    }

    fn set_override(
        &self,
        subject: &Subject,
        principal: PrincipalId,
        grant: RoleGrant,
        effect: Effect,
        expiry: Expiry,
    ) -> Result<AuthorityChange> {
        let target = principal_target(&principal);
        self.remote(
            subject,
            ACTION_OVERRIDE_SET,
            target.clone(),
            move |repo, policy| {
                repo.active_principal(&principal)?;
                let target_rank = repo.rank(&principal)?;
                enforce(authorization::override_grant(
                    policy,
                    &principal,
                    target_rank,
                    &grant,
                    effect,
                ))?;
                let expires_at = expiry.deadline(repo.now)?;
                let id = repo.insert_override(
                    &principal,
                    &grant,
                    effect,
                    expires_at,
                    authenticated_principal(policy)?,
                )?;
                self.audit_success(
                    repo,
                    remote_actor(policy)?,
                    ACTION_OVERRIDE_SET,
                    &target,
                    &[
                        ("override", Some(id.as_str().to_owned())),
                        ("capability", Some(grant.capability().id().to_owned())),
                        ("scope", Some(grant.scope().text())),
                        ("effect", Some(effect.stored().to_owned())),
                        ("expires_at_ms", expires_at.map(millis_text)),
                    ],
                )?;
                Ok(AuthorityChange::Override(id))
            },
        )
    }

    fn revoke_grant(
        &self,
        subject: &Subject,
        table: GrantTable,
        id: &str,
    ) -> Result<AuthorityChange> {
        let action = table.action();
        self.remote(
            subject,
            action,
            format!("grant:{id}"),
            move |repo, policy| {
                let principal = repo.grant_principal(table, id)?;
                let target_rank = repo.rank(&principal)?;
                enforce(authorization::revoke(policy, &principal, target_rank))?;
                if matches!(table, GrantTable::Assignments) {
                    let role: String = repo.connection().query_row(
                        "SELECT role_id FROM role_assignments WHERE id=?",
                        [id],
                        |r| r.get(0),
                    )?;
                    let role = repo.role(&RoleId::parse(&role)?)?;
                    if role.rank >= policy.rank() || role.builtin == Some(BuiltinRole::Owner) {
                        return Err(crate::ControlError::forbidden(
                            super::DenyReason::LocalGrantOnly,
                        ));
                    }
                }
                repo.revoke_grant(table, id, authenticated_principal(policy)?)?;
                self.audit_success(
                    repo,
                    remote_actor(policy)?,
                    action,
                    &principal_target(&principal),
                    &[("grant", Some(id.to_owned()))],
                )?;
                Ok(AuthorityChange::Revoked)
            },
        )
    }

    /// Exact devices.revoke is required even for Owner. All actors may revoke their
    /// own devices; other users must have strictly lower rank.
    fn revoke_device(&self, subject: &Subject, id: DeviceId) -> Result<AuthorityChange> {
        let refusal_target = format!("device:{}", id.as_str());
        self.remote(
            subject,
            ACTION_DEVICE_REVOKE,
            refusal_target,
            move |repo, policy| {
                let principal = repo.device_principal(&id)?;
                let target_rank = repo.rank(&principal)?;
                enforce(authorization::revoke_device(
                    policy,
                    &principal,
                    target_rank,
                ))?;
                repo.revoke_device(&id, authenticated_principal(policy)?)?;
                self.audit_success(
                    repo,
                    remote_actor(policy)?,
                    ACTION_DEVICE_REVOKE,
                    &principal_target(&principal),
                    &[("device", Some(id.as_str().to_owned()))],
                )?;
                Ok(AuthorityChange::Revoked)
            },
        )
    }

    /// Custom roles are immutable once created; there is no role-edit command. Rank is
    /// already bounded below the locally granted administrator tier by `CustomRank`.
    fn create_role(&self, subject: &Subject, role: CustomRole) -> Result<AuthorityChange> {
        let refusal_target = format!("role:{}", role.name().as_str());
        self.remote(
            subject,
            ACTION_ROLE_CREATE,
            refusal_target,
            move |repo, policy| {
                enforce(authorization::create(policy, &role))?;
                let id = repo.create_role(&role, authenticated_principal(policy)?)?;
                self.audit_success(
                    repo,
                    remote_actor(policy)?,
                    ACTION_ROLE_CREATE,
                    &format!("role:{}", id.as_str()),
                    &[
                        ("name", Some(role.name().as_str().to_owned())),
                        ("rank", Some(role.rank().value().to_string())),
                        ("capabilities", Some(role.grants().len().to_string())),
                    ],
                )?;
                Ok(AuthorityChange::Role(id))
            },
        )
    }

    /// Records audit evidence and markers for authority that has already stopped being
    /// effective. Evaluation never depends on this sweep. A failure leaves every marker
    /// unrecorded, so the same expiry is reported again rather than silently lost.
    pub fn sweep_expired(&self) -> Result<usize> {
        self.db
            .write_with(|transaction| {
                let repo = Repository::new(transaction.connection(), self.server(), self.now());
                let actor = AuditActor::system(SOURCE_EXPIRY.to_owned())?;
                let mut recorded = 0_usize;
                for table in [GrantTable::Assignments, GrantTable::Overrides] {
                    for (id, principal) in repo.expired(table)? {
                        repo.mark_expired(table, &id)?;
                        self.audit_success(
                            &repo,
                            actor.clone(),
                            ACTION_EXPIRED,
                            &principal_target(&principal),
                            &[("grant", Some(id)), ("kind", Some(table.name().to_owned()))],
                        )?;
                        recorded += 1;
                    }
                }
                Ok(recorded)
            })
            .map_err(ControlError::from)
    }

    // ------------------------------------------------------------------ boundaries

    fn server(&self) -> &str {
        self.db.server_id()
    }

    fn now(&self) -> EpochMillis {
        self.db.clock().millis()
    }

    fn run<T>(
        &self,
        work: impl FnOnce(&Repository<'_>) -> Result<T>,
        refusal: Refusal,
    ) -> Result<T> {
        let outcome = self.db.write_with(|transaction| {
            // Admission and BEGIN IMMEDIATE can wait; expiry starts at the owned transaction.
            work(&Repository::new(
                transaction.connection(),
                self.server(),
                self.now(),
            ))
        });
        self.finish(outcome, refusal)
    }

    fn remote<T>(
        &self,
        subject: &Subject,
        action: &'static str,
        refusal_target: String,
        work: impl FnOnce(&Repository<'_>, &PolicySnapshot) -> Result<T>,
    ) -> Result<T> {
        let claimed = subject.clone();
        let refusal = Refusal {
            action,
            target: refusal_target,
            actor: RefusalActor::Subject(subject.clone()),
        };
        self.run(
            move |repo| {
                let policy = repo.policy(Some(&claimed))?;
                if !policy.has_current_identity() {
                    return Err(ControlError::forbidden(DenyReason::NoIdentity));
                }
                work(repo, &policy)
            },
            refusal,
        )
    }

    /// A returned domain rejection rolls the transaction back. Only that clean case is
    /// then recorded, in its own transaction. State, rollback and nested-call failures
    /// describe infrastructure, not a decision the server made, so they never produce a
    /// refusal record and are never reported as a denial.
    fn finish<T>(
        &self,
        outcome: std::result::Result<T, TransactionError<ControlError>>,
        refusal: Refusal,
    ) -> Result<T> {
        match outcome {
            Ok(value) => Ok(value),
            Err(TransactionError::Work(error @ ControlError::Rejected { code, reason })) => {
                Err(self.record_refusal(&refusal, code, reason, error))
            }
            Err(other) => Err(other.into()),
        }
    }

    fn record_refusal(
        &self,
        refusal: &Refusal,
        code: IdentityErrorCode,
        reason: Option<DenyReason>,
        primary: ControlError,
    ) -> ControlError {
        let outcome = if code == IdentityErrorCode::Forbidden {
            "denied"
        } else {
            "rejected"
        };
        let cause = reason.map_or_else(|| code.code(), DenyReason::code);
        let actor = match &refusal.actor {
            RefusalActor::Local => local_actor(),
            RefusalActor::Code => AuditActor::system(SOURCE_CODE.to_owned()),
            RefusalActor::Subject(subject) => AuditActor::new(
                subject.principal.as_str().to_owned(),
                subject.device.as_str().to_owned(),
                SOURCE_REMOTE.to_owned(),
            ),
        };
        // Retains both the refusal and any audit failure. A denial whose evidence could
        // not be written is an error, not a successful refused outcome.
        let audited = actor
            .and_then(|actor| {
                AuditRecord::new(
                    actor,
                    refusal.action.to_owned(),
                    refusal.target.clone(),
                    outcome.to_owned(),
                    None,
                    [("reason".to_owned(), Some(cause.to_owned()))],
                )
            })
            .and_then(|record| self.audit.append_record(&record));
        match audited {
            Ok(_) => primary,
            Err(audit_failure) => ControlError::AuditUnavailable {
                primary: Box::new(primary),
                audit_failure: Box::new(ControlError::State(audit_failure)),
            },
        }
    }

    /// Appends through the caller's unit of work, never a nested `db.write`. Propagating
    /// an append failure is what rolls the mutation back with its audit.
    fn audit_success(
        &self,
        repo: &Repository<'_>,
        actor: AuditActor,
        action: &str,
        target: &str,
        metadata: &[(&str, Option<String>)],
    ) -> Result<()> {
        let record = AuditRecord::new(
            actor,
            action.to_owned(),
            target.to_owned(),
            "success".to_owned(),
            None,
            metadata
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone())),
        )?;
        self.audit.append(repo.connection(), &record)?;
        Ok(())
    }

    /// Channel evidence, not a caller-created authorization token. Purpose, nonce shape,
    /// OS actor, freshness, lifetime and single use are validated here rather than
    /// trusted from the authority. Any failure rolls the whole transaction back, so a
    /// refused command leaves the nonce unused and can be retried.
    fn consume_proof(
        &self,
        repo: &Repository<'_>,
        proof: Option<&LocalProof>,
        purpose: Purpose,
    ) -> Result<Attestation> {
        let refused = || rejected(IdentityErrorCode::LocalAuthorityRequired);
        let attestation = proof
            .and_then(|proof| self.local_authority.verify(proof, purpose))
            .ok_or_else(refused)?;
        if attestation.purpose != purpose
            || !(NONCE_MIN_BYTES..=NONCE_MAX_BYTES).contains(&attestation.nonce.len())
            || !valid_label(&attestation.os_actor, OS_ACTOR_MAX_SCALARS)
        {
            return Err(refused());
        }
        let latest_issuance = repo.now.checked_add(proof_skew_ms()?).ok_or_else(refused)?;
        let lifetime = attestation
            .expires_at_ms
            .checked_sub(attestation.issued_at_ms)
            .ok_or_else(refused)?;
        // An exact-deadline proof is already expired; evaluation uses the same rule.
        if lifetime <= 0
            || lifetime > proof_lifetime_ms()?
            || attestation.issued_at_ms > latest_issuance
            || attestation.expires_at_ms <= repo.now
        {
            return Err(refused());
        }
        repo.use_nonce(&attestation.nonce, purpose)?;
        Ok(attestation)
    }
}

struct Refusal {
    action: &'static str,
    target: String,
    actor: RefusalActor,
}

enum RefusalActor {
    Local,
    Code,
    Subject(Subject),
}

fn principal_target(principal: &PrincipalId) -> String {
    format!("principal:{}", principal.as_str())
}

/// Audit metadata omits absent values entirely; a permanent grant must not be
/// recorded as an empty `expires_at_ms` field.
fn millis_text(millis: EpochMillis) -> String {
    millis.to_string()
}

fn local_actor() -> vesta_state::Result<AuditActor> {
    AuditActor::new(
        SOURCE_LOCAL.to_owned(),
        "local".to_owned(),
        SOURCE_LOCAL.to_owned(),
    )
}

fn remote_actor(policy: &PolicySnapshot) -> Result<AuditActor> {
    Ok(AuditActor::new(
        authenticated_principal(policy)?.to_owned(),
        policy
            .device()
            .ok_or_else(|| ControlError::forbidden(DenyReason::NoIdentity))?
            .as_str()
            .to_owned(),
        SOURCE_REMOTE.to_owned(),
    )?)
}

fn authenticated_principal(policy: &PolicySnapshot) -> Result<&str> {
    policy
        .principal()
        .map(PrincipalId::as_str)
        .ok_or_else(|| ControlError::forbidden(DenyReason::NoIdentity))
}

fn enforce(decision: Decision) -> Result<()> {
    match decision {
        Decision::Allow => Ok(()),
        Decision::Deny(reason) => Err(ControlError::forbidden(reason)),
    }
}

fn rejected(code: IdentityErrorCode) -> ControlError {
    ControlError::rejected(code)
}

/// Proof window bounds come from the same `Duration` constants the domain exposes,
/// so persistence precision and the documented limits cannot drift apart.
fn proof_lifetime_ms() -> Result<i64> {
    super::domain::duration_millis(MAX_PROOF_LIFETIME)
}

fn proof_skew_ms() -> Result<i64> {
    super::domain::duration_millis(PROOF_CLOCK_SKEW)
}
