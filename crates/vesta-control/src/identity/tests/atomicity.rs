//! Mutation and transaction atomicity: a success audit shares the mutation's single
//! transaction, a refusal audit is a SEPARATE post-rollback transaction, an infrastructure
//! rollback is never reported as a decision, and nothing about a transaction or a secret
//! escapes the service's own boundary.
//!
//! Sources: `IdentityAuthorizationTest.auditFailureCannotPartiallyReplaceRolesCreateOverridesOrRevokeDevices`
//! and `IdentityAuthorizationTest.failedRefusalAuditReportsAuditUnavailableWithoutMutatingAuthority`,
//! plus independent negative cases on the same mutation and transaction paths.
//!
//! Everything runs against real SQLite state, a real `BEFORE INSERT ... RAISE(ABORT)`
//! trigger on `audit_events`, the real audit chain and the test clock. Nothing here mocks
//! the service, the audit log, the database or the local authority.
use super::support::*;
use crate::ControlError;
use crate::identity::{
    AssignmentId, AuthorityChange, BuiltinRole, Capability, CodeInput, CustomRank, CustomRole,
    Decision, DenyReason, DeviceId, DeviceLabel, DisplayName, Effect, Expiry, IdentityService,
    LocalCommand, LocalOutcome, LocalProof, OverrideId, OwnerAdded, PolicySnapshot, PrincipalId,
    Purpose, RedeemOwner, RemoteCommand, Resource, ResourceScope, RoleGrant, RoleId, RoleName,
    Subject, SubjectRecord,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use std::error::Error;
use vesta_state::audit::AuditEntry;
use vesta_state::{ReadTransaction, StateDatabase, StateError, WriteTransaction};

// ---------------------------------------------------------------------------- helpers

fn role(builtin: BuiltinRole) -> RoleId {
    RoleId::from(builtin)
}

fn scope(text: &str) -> ResourceScope {
    ResourceScope::parse(text).expect("canonical scope")
}

fn resource(text: &str) -> Resource {
    Resource::parse(text).expect("canonical resource")
}

fn files_read_grant() -> RoleGrant {
    RoleGrant::new(Capability::FilesRead, scope("config/**")).expect("scoped grant")
}

/// A canonical 20-byte Base64URL token this database has never issued.
fn never_issued_code() -> String {
    URL_SAFE_NO_PAD.encode([0x5a_u8; 20])
}

/// A well-formed identifier no transaction can have produced: real ids are random UUIDv4s.
fn absent_assignment(tag: &str) -> AssignmentId {
    AssignmentId::parse(&format!("atomicity-absent-{tag}")).expect("assignment identifier")
}

fn audit_entries(fixture: &Fixture) -> Vec<AuditEntry> {
    fixture.audit.page(0, 500).expect("read audit page")
}

/// Success rows for one action. A refusal is recorded under the same action name with a
/// non-success outcome, so the two must always be counted separately.
fn success_rows(fixture: &Fixture, action: &str) -> Vec<AuditEntry> {
    audit_entries(fixture)
        .into_iter()
        .filter(|entry| entry.action == action && entry.outcome == "success")
        .collect()
}

/// Refusal rows for one action, oldest last.
fn refusal_rows(fixture: &Fixture, action: &str) -> Vec<AuditEntry> {
    audit_entries(fixture)
        .into_iter()
        .filter(|entry| entry.action == action && entry.outcome != "success")
        .collect()
}

/// `(id, role_id, revoked_at, expires_at)` for one principal, in insertion order.
fn assignment_rows(
    fixture: &Fixture,
    principal: &PrincipalId,
) -> Vec<(String, String, Option<i64>, Option<i64>)> {
    fixture
        .db
        .read(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, role_id, revoked_at, expires_at FROM role_assignments \
                 WHERE principal_id = ? ORDER BY rowid",
            )?;
            let rows = statement.query_map(rusqlite::params![principal.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .expect("read assignment rows")
}

/// `(id, capability, scope, effect, revoked_at)` for one principal, in insertion order.
fn override_rows(
    fixture: &Fixture,
    principal: &PrincipalId,
) -> Vec<(String, String, String, String, Option<i64>)> {
    fixture
        .db
        .read(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, capability, scope, effect, revoked_at FROM principal_overrides \
                 WHERE principal_id = ? ORDER BY rowid",
            )?;
            let rows = statement.query_map(rusqlite::params![principal.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .expect("read override rows")
}

/// `(id, revoked_at)` for one principal's devices, in insertion order.
fn device_rows(fixture: &Fixture, principal: &PrincipalId) -> Vec<(String, Option<i64>)> {
    fixture
        .db
        .read(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, revoked_at FROM devices WHERE principal_id = ? ORDER BY rowid",
            )?;
            let rows = statement.query_map(rusqlite::params![principal.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .expect("read device rows")
}

fn role_ids_by_name(fixture: &Fixture, name: &str) -> Vec<String> {
    fixture
        .db
        .read(|connection| {
            let mut statement = connection
                .prepare("SELECT id FROM roles WHERE name = ? COLLATE NOCASE ORDER BY rowid")?;
            let rows =
                statement.query_map(rusqlite::params![name], |row| row.get::<_, String>(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .expect("read role ids by name")
}

fn custom_role(name: &str, rank: i32, grants: Vec<RoleGrant>) -> CustomRole {
    CustomRole::new(
        RoleName::parse(name).expect("role name"),
        CustomRank::new(rank).expect("rank below the administrator tier"),
        grants,
    )
    .expect("custom role")
}

fn redeem(
    fixture: &Fixture,
    code: &str,
    name: &str,
    device: &str,
    key: &str,
) -> crate::Result<Subject> {
    fixture
        .identities
        .redeem_owner(RedeemOwner {
            code: CodeInput::parse(code)?,
            name: DisplayName::parse(name)?,
            device_label: DeviceLabel::parse(device)?,
            fingerprint: fingerprint(key),
        })
        .map(|added| Subject {
            principal: added.principal,
            device: added.device,
        })
}

fn policy_of(fixture: &Fixture, subject: &Subject) -> PolicySnapshot {
    fixture
        .identities
        .current_policy(Some(subject))
        .expect("current policy")
}

fn files_read(fixture: &Fixture, subject: &Subject, path: &str) -> Decision {
    policy_of(fixture, subject).check(Capability::FilesRead, Some(&resource(path)))
}

fn device_revoked(fixture: &Fixture, subject: &Subject) -> bool {
    device_rows(fixture, &subject.principal)
        .into_iter()
        .find(|row| row.0 == subject.device.as_str())
        .map(|row| row.1.is_some())
        .expect("the subject device row")
}

/// An infrastructure failure, never a decision the server made.
fn expects_state_failure(error: ControlError, context: &str) {
    assert_eq!(
        error.reason(),
        None,
        "{context}: a state error carries no decision"
    );
    assert!(
        error.code().starts_with("state_"),
        "{context}: expected a state_* code, got {}",
        error.code()
    );
    assert!(
        matches!(&error, ControlError::State(_)),
        "{context}: expected ControlError::State, got {error:?}"
    );
    assert!(
        !matches!(&error, ControlError::Rollback { .. }),
        "{context}: one failed append is not a rollback failure"
    );
    assert!(
        !matches!(&error, ControlError::AuditUnavailable { .. }),
        "{context}: an allowed mutation's own audit failure is not a refusal-audit failure"
    );
}

/// A refusal whose evidence could not be written is an error that keeps the decision.
fn expects_audit_unavailable(error: ControlError, primary: &'static str, context: &str) {
    assert_eq!(error.code(), "audit_unavailable", "{context}");
    assert_eq!(error.reason(), None, "{context}");
    let ControlError::AuditUnavailable {
        primary: kept,
        audit_failure,
    } = &error
    else {
        panic!("{context}: expected ControlError::AuditUnavailable, got {error:?}");
    };
    assert_eq!(
        kept.code(),
        primary,
        "{context}: the original domain refusal must be preserved"
    );
    assert!(
        matches!(kept.as_ref(), ControlError::Rejected { .. }),
        "{context}: the preserved primary is the original typed refusal, got {kept:?}"
    );
    assert!(
        matches!(audit_failure.as_ref(), ControlError::State(_)),
        "{context}: the retained audit failure is infrastructure, got {audit_failure:?}"
    );
    assert!(
        audit_failure.code().starts_with("state_"),
        "{context}: audit failure code was {}",
        audit_failure.code()
    );
    assert!(
        !matches!(&error, ControlError::Rejected { .. }),
        "{context}: a denial is never reported as a successful outcome"
    );
}

/// A clean domain rejection: a typed code, an optional fixed reason, and never an
/// infrastructure, refusal-audit or rollback variant.
fn expects_domain_rejection(
    error: ControlError,
    code: &'static str,
    reason: Option<DenyReason>,
    context: &str,
) {
    assert_eq!(error.code(), code, "{context}");
    assert_eq!(error.reason(), reason, "{context}");
    let ControlError::Rejected {
        code: typed,
        reason: typed_reason,
    } = &error
    else {
        panic!("{context}: expected a typed domain rejection, got {error:?}");
    };
    assert_eq!(typed.code(), code, "{context}");
    assert_eq!(*typed_reason, reason, "{context}");
    assert!(
        !matches!(&error, ControlError::State(_)),
        "{context}: a decision is never a state error"
    );
    assert!(
        !matches!(&error, ControlError::Rollback { .. }),
        "{context}: a domain rejection never reports a rollback failure"
    );
    assert!(
        !matches!(&error, ControlError::AuditUnavailable { .. }),
        "{context}: with a healthy audit a refusal is not audit_unavailable"
    );
    assert!(
        error.source().is_none(),
        "{context}: a decision retains no underlying failure"
    );
}

// ------------------------------------ Java: auditFailureCannotPartiallyReplaceRoles...

#[test]
fn a_remote_role_assignment_audit_failure_rolls_back_the_replacement_and_is_retryable() {
    let fixture = Fixture::open();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let viewer = fixture.subject(role(BuiltinRole::Viewer));

    let assignments_before = assignment_rows(&fixture, &viewer.principal);
    let total_before = fixture.count("role_assignments");
    let audit_before = fixture.count("audit_events");
    assert_eq!(assignments_before.len(), 1, "the fixture granted one role");
    let previous = assignments_before[0].0.clone();
    assert_eq!(assignments_before[0].2, None, "the previous grant is live");
    assert_eq!(success_rows(&fixture, "authority.role.assign").len(), 2);

    fixture.audit_failure(true);
    let error = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: role(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        )
        .expect_err("a failed success audit rolls the assignment back");
    expects_state_failure(error, "remote assignment audit failure");

    // Nothing changed: the previous assignment was NOT revoked and no row was inserted.
    assert_eq!(
        assignment_rows(&fixture, &viewer.principal),
        assignments_before
    );
    assert_eq!(fixture.count("role_assignments"), total_before);
    assert_eq!(fixture.count("audit_events"), audit_before);
    assert_eq!(success_rows(&fixture, "authority.role.assign").len(), 2);
    assert!(refusal_rows(&fixture, "authority.role.assign").is_empty());
    let policy = policy_of(&fixture, &viewer);
    assert_eq!(policy.roles(), [role(BuiltinRole::Viewer)].as_slice());
    assert!(policy.holds_builtin(BuiltinRole::Viewer));
    assert!(!policy.holds_builtin(BuiltinRole::Moderator));
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);

    // With the fault removed the identical command is retryable and leaves one audit row.
    fixture.audit_failure(false);
    let change = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: role(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        )
        .expect("the rolled-back assignment is retryable");
    assert!(matches!(change, AuthorityChange::Assignment(_)));
    assert_eq!(fixture.count("audit_events"), audit_before + 1);
    assert_eq!(success_rows(&fixture, "authority.role.assign").len(), 3);
    assert!(refusal_rows(&fixture, "authority.role.assign").is_empty());

    let after = assignment_rows(&fixture, &viewer.principal);
    assert_eq!(after.len(), 2);
    assert_eq!(
        after
            .iter()
            .find(|row| row.0 == previous)
            .expect("previous row")
            .2,
        Some(INITIAL_TIME_MS),
        "only on the retry is the previous assignment revoked"
    );
    let live: Vec<_> = after.iter().filter(|row| row.2.is_none()).collect();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].1, "moderator");
    let policy = policy_of(&fixture, &viewer);
    assert!(policy.holds_builtin(BuiltinRole::Moderator));
    assert!(!policy.holds_builtin(BuiltinRole::Viewer));
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_local_role_assignment_audit_failure_rolls_back_and_releases_the_proof() {
    let fixture = Fixture::open();
    let viewer = fixture.subject(role(BuiltinRole::Viewer));
    let proof = fixture.proof(Purpose::LocalRoleGrant);

    let assignments_before = assignment_rows(&fixture, &viewer.principal);
    let total_before = fixture.count("role_assignments");
    let nonces_before = fixture.count("local_authority_uses");
    let audit_before = fixture.count("audit_events");
    let previous = assignments_before[0].0.clone();

    fixture.audit_failure(true);
    let error = fixture
        .identities
        .apply_local(
            Some(&proof),
            LocalCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: role(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        )
        .expect_err("a failed success audit rolls the local assignment back");
    expects_state_failure(error, "local assignment audit failure");

    assert_eq!(
        assignment_rows(&fixture, &viewer.principal),
        assignments_before
    );
    assert_eq!(fixture.count("role_assignments"), total_before);
    assert_eq!(fixture.count("audit_events"), audit_before);
    assert_eq!(
        fixture.count("local_authority_uses"),
        nonces_before,
        "the nonce is rolled back with the mutation, so the proof is reusable"
    );
    assert_eq!(
        assignment_rows(&fixture, &viewer.principal)[0].2,
        None,
        "the previous assignment is still live"
    );
    assert!(!policy_of(&fixture, &viewer).holds_builtin(BuiltinRole::Moderator));
    assert!(refusal_rows(&fixture, "authority.role.assign").is_empty());

    // The very same proof value is still usable: nothing was consumed.
    fixture.audit_failure(false);
    let outcome = fixture
        .identities
        .apply_local(
            Some(&proof),
            LocalCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: role(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        )
        .expect("the rolled-back local assignment is retryable with the same proof");
    assert!(matches!(outcome, LocalOutcome::Assignment(_)));
    assert_eq!(fixture.count("local_authority_uses"), nonces_before + 1);
    assert_eq!(fixture.count("audit_events"), audit_before + 1);
    assert_eq!(success_rows(&fixture, "authority.role.assign").len(), 2);

    let after = assignment_rows(&fixture, &viewer.principal);
    assert_eq!(after.len(), 2);
    assert_eq!(
        after
            .iter()
            .find(|row| row.0 == previous)
            .expect("previous row")
            .2,
        Some(INITIAL_TIME_MS)
    );
    assert!(policy_of(&fixture, &viewer).holds_builtin(BuiltinRole::Moderator));
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_remote_override_audit_failure_leaves_no_override_row_and_is_retryable() {
    let fixture = Fixture::open();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let viewer = fixture.subject(role(BuiltinRole::Viewer));

    let overrides_before = fixture.count("principal_overrides");
    let audit_before = fixture.count("audit_events");
    let grant = files_read_grant();

    fixture.audit_failure(true);
    let error = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::SetOverride {
                principal: viewer.principal.clone(),
                grant: grant.clone(),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        )
        .expect_err("a failed success audit rolls the override back");
    expects_state_failure(error, "override audit failure");

    assert_eq!(fixture.count("principal_overrides"), overrides_before);
    assert!(override_rows(&fixture, &viewer.principal).is_empty());
    assert_eq!(fixture.count("audit_events"), audit_before);
    assert_eq!(
        files_read(&fixture, &viewer, "config/a.txt"),
        Decision::Deny(DenyReason::NotGranted)
    );
    assert!(
        !policy_of(&fixture, &viewer).holds_entirely(Capability::FilesRead, &scope("config/**"))
    );

    fixture.audit_failure(false);
    let change = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::SetOverride {
                principal: viewer.principal.clone(),
                grant: grant.clone(),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        )
        .expect("the rolled-back override is retryable");
    let AuthorityChange::Override(id) = change else {
        panic!("SetOverride returns an override id");
    };
    assert_eq!(fixture.count("principal_overrides"), overrides_before + 1);
    assert_eq!(fixture.count("audit_events"), audit_before + 1);
    assert_eq!(success_rows(&fixture, "authority.override.set").len(), 1);

    let rows = override_rows(&fixture, &viewer.principal);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, id.as_str());
    assert_eq!(rows[0].1, "files.read");
    assert_eq!(rows[0].2, "config/**");
    assert_eq!(rows[0].3, "allow");
    assert_eq!(rows[0].4, None);
    assert_eq!(
        files_read(&fixture, &viewer, "config/a.txt"),
        Decision::Allow
    );
    assert_eq!(
        files_read(&fixture, &viewer, "logs/latest.log"),
        Decision::Deny(DenyReason::NotGranted),
        "the recorded scope is exactly the one that was granted"
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_revoked_assignment_audit_failure_leaves_the_grant_live_and_is_retryable() {
    let fixture = Fixture::open();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let viewer = fixture.subject(role(BuiltinRole::Viewer));
    let target = assignment_rows(&fixture, &viewer.principal)[0].0.clone();

    let assignments_before = assignment_rows(&fixture, &viewer.principal);
    let audit_before = fixture.count("audit_events");

    fixture.audit_failure(true);
    let error = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::RevokeAssignment(AssignmentId::parse(&target).expect("assignment id")),
        )
        .expect_err("a failed success audit rolls the revocation back");
    expects_state_failure(error, "revoke-assignment audit failure");

    assert_eq!(
        assignment_rows(&fixture, &viewer.principal),
        assignments_before
    );
    assert_eq!(
        assignment_rows(&fixture, &viewer.principal)[0].2,
        None,
        "the target grant is still unrevoked"
    );
    assert_eq!(fixture.count("audit_events"), audit_before);
    assert!(success_rows(&fixture, "authority.role.revoke").is_empty());
    assert!(refusal_rows(&fixture, "authority.role.revoke").is_empty());
    assert!(policy_of(&fixture, &viewer).holds_builtin(BuiltinRole::Viewer));

    fixture.audit_failure(false);
    let change = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::RevokeAssignment(AssignmentId::parse(&target).expect("assignment id")),
        )
        .expect("the rolled-back revocation is retryable");
    assert!(matches!(change, AuthorityChange::Revoked));
    assert_eq!(fixture.count("audit_events"), audit_before + 1);
    assert_eq!(success_rows(&fixture, "authority.role.revoke").len(), 1);
    assert_eq!(
        assignment_rows(&fixture, &viewer.principal)[0].2,
        Some(INITIAL_TIME_MS)
    );
    assert!(!policy_of(&fixture, &viewer).holds_builtin(BuiltinRole::Viewer));
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_revoked_override_audit_failure_leaves_the_override_live_and_is_retryable() {
    let fixture = Fixture::open();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let viewer = fixture.subject(role(BuiltinRole::Viewer));
    let target = fixture.stored_override(
        &viewer,
        Capability::FilesRead,
        "config/**",
        Effect::Allow,
        None,
    );
    assert_eq!(
        files_read(&fixture, &viewer, "config/a.txt"),
        Decision::Allow,
        "the trusted fixture override is in effect"
    );

    let overrides_before = override_rows(&fixture, &viewer.principal);
    let audit_before = fixture.count("audit_events");

    fixture.audit_failure(true);
    let error = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::RevokeOverride(OverrideId::parse(&target).expect("override id")),
        )
        .expect_err("a failed success audit rolls the revocation back");
    expects_state_failure(error, "revoke-override audit failure");

    assert_eq!(override_rows(&fixture, &viewer.principal), overrides_before);
    assert_eq!(
        override_rows(&fixture, &viewer.principal)[0].4,
        None,
        "the target grant is still unrevoked"
    );
    assert_eq!(fixture.count("audit_events"), audit_before);
    assert_eq!(
        files_read(&fixture, &viewer, "config/a.txt"),
        Decision::Allow
    );

    fixture.audit_failure(false);
    let change = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::RevokeOverride(OverrideId::parse(&target).expect("override id")),
        )
        .expect("the rolled-back revocation is retryable");
    assert!(matches!(change, AuthorityChange::Revoked));
    assert_eq!(fixture.count("audit_events"), audit_before + 1);
    assert_eq!(success_rows(&fixture, "authority.override.revoke").len(), 1);
    assert_eq!(
        override_rows(&fixture, &viewer.principal)[0].4,
        Some(INITIAL_TIME_MS)
    );
    assert_eq!(
        files_read(&fixture, &viewer, "config/a.txt"),
        Decision::Deny(DenyReason::NotGranted)
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_device_revocation_audit_failure_leaves_the_device_live_and_is_retryable() {
    let fixture = Fixture::open();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let viewer = fixture.subject(role(BuiltinRole::Viewer));

    let devices_before = device_rows(&fixture, &viewer.principal);
    let audit_before = fixture.count("audit_events");

    fixture.audit_failure(true);
    let error = fixture
        .identities
        .apply_remote(&admin, RemoteCommand::RevokeDevice(viewer.device.clone()))
        .expect_err("a failed success audit rolls the revocation back");
    expects_state_failure(error, "revoke-device audit failure");

    assert_eq!(device_rows(&fixture, &viewer.principal), devices_before);
    assert!(
        !device_revoked(&fixture, &viewer),
        "the device is still unrevoked"
    );
    assert_eq!(fixture.count("audit_events"), audit_before);
    assert!(success_rows(&fixture, "identity.device.revoke").is_empty());
    assert!(
        policy_of(&fixture, &viewer).has_current_identity(),
        "authority is unchanged"
    );

    fixture.audit_failure(false);
    let change = fixture
        .identities
        .apply_remote(&admin, RemoteCommand::RevokeDevice(viewer.device.clone()))
        .expect("the rolled-back revocation is retryable");
    assert!(matches!(change, AuthorityChange::Revoked));
    assert_eq!(fixture.count("audit_events"), audit_before + 1);
    assert_eq!(success_rows(&fixture, "identity.device.revoke").len(), 1);
    assert!(device_revoked(&fixture, &viewer));
    assert!(!policy_of(&fixture, &viewer).has_current_identity());
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_custom_role_creation_audit_failure_leaves_no_role_or_capability_row() {
    let fixture = Fixture::open();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let command =
        || RemoteCommand::CreateRole(custom_role("Config reader", 15, vec![files_read_grant()]));

    let roles_before = fixture.count("roles");
    let capabilities_before = fixture.count("role_capabilities");
    let audit_before = fixture.count("audit_events");
    assert!(role_ids_by_name(&fixture, "Config reader").is_empty());

    fixture.audit_failure(true);
    let error = fixture
        .identities
        .apply_remote(&admin, command())
        .expect_err("a failed success audit rolls role creation back");
    expects_state_failure(error, "create-role audit failure");

    assert_eq!(fixture.count("roles"), roles_before, "no roles row");
    assert_eq!(
        fixture.count("role_capabilities"),
        capabilities_before,
        "no role_capabilities rows"
    );
    assert!(role_ids_by_name(&fixture, "Config reader").is_empty());
    assert_eq!(fixture.count("audit_events"), audit_before);
    assert!(success_rows(&fixture, "authority.role.create").is_empty());

    fixture.audit_failure(false);
    let change = fixture
        .identities
        .apply_remote(&admin, command())
        .expect("the rolled-back role creation is retryable");
    let AuthorityChange::Role(id) = change else {
        panic!("CreateRole returns a role id");
    };
    assert_eq!(fixture.count("roles"), roles_before + 1);
    assert_eq!(fixture.count("role_capabilities"), capabilities_before + 1);
    assert_eq!(fixture.count("audit_events"), audit_before + 1);
    assert_eq!(success_rows(&fixture, "authority.role.create").len(), 1);
    assert_eq!(
        role_ids_by_name(&fixture, "Config reader"),
        vec![id.as_str().to_owned()]
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// --------------------------------- a refusal audit is its own, later transaction

#[test]
fn a_refused_remote_delegation_is_audited_only_after_its_own_transaction_rolled_back() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let viewer = fixture.subject(role(BuiltinRole::Viewer));

    let baseline = audit_entries(&fixture);
    let assignments_before = assignment_rows(&fixture, &viewer.principal);
    let total_before = fixture.count("role_assignments");

    let error = fixture
        .identities
        .apply_remote(
            &owner,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                // Locally granted tiers are never delegable: a real domain refusal.
                role: role(BuiltinRole::Administrator),
                expiry: Expiry::Never,
            },
        )
        .expect_err("a delegable-tier request is refused");
    expects_domain_rejection(
        error,
        "forbidden",
        Some(DenyReason::LocalGrantOnly),
        "delegation",
    );

    // The mutation rolled back: nothing was persisted.
    assert_eq!(
        assignment_rows(&fixture, &viewer.principal),
        assignments_before
    );
    assert_eq!(fixture.count("role_assignments"), total_before);
    assert_eq!(assignment_rows(&fixture, &viewer.principal)[0].2, None);
    let policy = policy_of(&fixture, &viewer);
    assert_eq!(policy.roles(), [role(BuiltinRole::Viewer)].as_slice());
    assert!(!policy.holds_builtin(BuiltinRole::Administrator));

    // Exactly one refusal row was appended afterwards.
    let entries = audit_entries(&fixture);
    assert_eq!(entries.len(), baseline.len() + 1);
    let refusal = entries.last().expect("the refusal row");
    assert_eq!(refusal.action, "authority.role.assign");
    assert_eq!(refusal.outcome, "denied");
    assert_eq!(
        refusal.metadata_json, "{\"reason\":\"local_grant_only\"}",
        "the only metadata key is `reason`, holding a fixed code"
    );
    assert_eq!(refusal.source, "remote");
    assert_eq!(refusal.principal_id, owner.principal.as_str());
    assert_eq!(refusal.device_id, owner.device.as_str());
    assert_eq!(
        refusal.target,
        format!("principal:{}", viewer.principal.as_str())
    );
    assert_eq!(refusal.at_ms, INITIAL_TIME_MS);

    // The refusal is NOT inside the failed transaction: its rolled-back transaction left no
    // trace, every earlier row is byte-identical, and the new row chains onto the head that
    // survived it. A row written inside the aborted transaction could not exist at all.
    assert_eq!(&entries[..baseline.len()], &baseline[..]);
    let head = baseline.last().expect("a prior row");
    assert_eq!(refusal.seq, head.seq + 1);
    assert_eq!(refusal.prev_hash, head.hash);
    let verification = fixture.audit.verify_chain().expect("audit chain");
    assert!(verification.valid);
    assert_eq!(verification.checked as usize, entries.len());
    assert_eq!(verification.first_broken_seq, None);
    assert_eq!(success_rows(&fixture, "authority.role.assign").len(), 1);
}

#[test]
fn a_refusal_that_is_not_forbidden_is_recorded_as_rejected_not_denied() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let admin = fixture.subject(role(BuiltinRole::Administrator));

    // --- not_found: revoking a grant that does not exist.
    let before = audit_entries(&fixture).len();
    let error = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::RevokeAssignment(absent_assignment("not-found")),
        )
        .expect_err("an unknown grant is refused");
    expects_domain_rejection(error, "not_found", None, "unknown assignment");
    let entries = audit_entries(&fixture);
    assert_eq!(entries.len(), before + 1);
    let refusal = entries.last().expect("the refusal row");
    assert_eq!(refusal.action, "authority.role.revoke");
    assert_eq!(refusal.outcome, "rejected", "only `forbidden` is a denial");
    assert_eq!(refusal.metadata_json, "{\"reason\":\"not_found\"}");
    assert_eq!(
        refusal.target,
        format!("grant:{}", absent_assignment("not-found").as_str())
    );

    // --- conflict: a custom role name that already exists.
    let change = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::CreateRole(custom_role("Config reader", 15, vec![files_read_grant()])),
        )
        .expect("create the first custom role");
    assert!(matches!(change, AuthorityChange::Role(_)));
    let roles_before = fixture.count("roles");
    let before = audit_entries(&fixture).len();
    let error = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::CreateRole(custom_role("config READER", 15, vec![files_read_grant()])),
        )
        .expect_err("a duplicate role name is refused");
    expects_domain_rejection(error, "conflict", None, "duplicate role name");
    assert_eq!(fixture.count("roles"), roles_before, "nothing persisted");
    let entries = audit_entries(&fixture);
    assert_eq!(entries.len(), before + 1);
    let refusal = entries.last().expect("the refusal row");
    assert_eq!(refusal.action, "authority.role.create");
    assert_eq!(refusal.outcome, "rejected");
    assert_eq!(refusal.metadata_json, "{\"reason\":\"conflict\"}");
    assert_eq!(refusal.target, "role:config READER");

    // --- bootstrap_closed: a local bootstrap while an owner exists.
    let nonces_before = fixture.count("local_authority_uses");
    let codes_before = fixture.count("bootstrap_codes");
    let before = entries.len();
    let error = fixture
        .identities
        .apply_local(
            Some(&fixture.proof(Purpose::OwnerBootstrap)),
            LocalCommand::Bootstrap(Default::default()),
        )
        .expect_err("bootstrap is closed while an owner exists");
    expects_domain_rejection(error, "bootstrap_closed", None, "closed bootstrap");
    assert_eq!(fixture.count("local_authority_uses"), nonces_before);
    assert_eq!(fixture.count("bootstrap_codes"), codes_before);
    let entries = audit_entries(&fixture);
    assert_eq!(entries.len(), before + 1);
    let refusal = entries.last().expect("the refusal row");
    assert_eq!(refusal.action, "identity.bootstrap.issue");
    assert_eq!(refusal.outcome, "rejected");
    assert_eq!(refusal.metadata_json, "{\"reason\":\"bootstrap_closed\"}");
    assert_eq!(refusal.source, "local-authority");
    assert_eq!(refusal.target, "owner");

    // A real owner was never disturbed by any of the three refusals.
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    assert!(
        policy_of(&fixture, &owner)
            .check(Capability::BackupsRestore, None)
            .allowed()
    );
    let verification = fixture.audit.verify_chain().expect("audit chain");
    assert!(verification.valid);
    assert_eq!(verification.checked as usize, entries.len());
}

// --------------- Java: failedRefusalAuditReportsAuditUnavailableWithoutMutatingAuthority

#[test]
fn a_forbidden_remote_command_whose_refusal_audit_fails_is_audit_unavailable() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let viewer = fixture.subject(role(BuiltinRole::Viewer));

    let assignments_before = assignment_rows(&fixture, &viewer.principal);
    let total_before = fixture.count("role_assignments");
    let audit_before = fixture.count("audit_events");

    fixture.audit_failure(true);
    let error = fixture
        .identities
        .apply_remote(
            &owner,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: role(BuiltinRole::Administrator),
                expiry: Expiry::Never,
            },
        )
        .expect_err("the delegation is refused");
    expects_audit_unavailable(error, "forbidden", "remote refusal audit failure");

    // Authority is untouched and no evidence was recorded.
    assert_eq!(
        assignment_rows(&fixture, &viewer.principal),
        assignments_before
    );
    assert_eq!(fixture.count("role_assignments"), total_before);
    assert_eq!(fixture.count("audit_events"), audit_before);
    let policy = policy_of(&fixture, &viewer);
    assert_eq!(policy.roles(), [role(BuiltinRole::Viewer)].as_slice());
    assert!(!policy.holds_builtin(BuiltinRole::Administrator));

    // With the fault removed the identical command is a normally audited denial.
    fixture.audit_failure(false);
    let error = fixture
        .identities
        .apply_remote(
            &owner,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: role(BuiltinRole::Administrator),
                expiry: Expiry::Never,
            },
        )
        .expect_err("still refused");
    expects_domain_rejection(
        error,
        "forbidden",
        Some(DenyReason::LocalGrantOnly),
        "retry",
    );
    let refusals = refusal_rows(&fixture, "authority.role.assign");
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].outcome, "denied");
    assert_eq!(fixture.count("audit_events"), audit_before + 1);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_local_command_whose_refusal_audit_fails_is_audit_unavailable_and_keeps_the_primary() {
    let fixture = Fixture::open();
    let viewer = fixture.subject(role(BuiltinRole::Viewer));

    let assignments_before = assignment_rows(&fixture, &viewer.principal);
    let total_before = fixture.count("role_assignments");
    let nonces_before = fixture.count("local_authority_uses");
    let audit_before = fixture.count("audit_events");
    // ONE proof value, used for every attempt below, so "the nonce is rolled back with
    // the refusal" is directly observable rather than inferred from fresh proofs.
    let proof = fixture.proof(Purpose::LocalRoleGrant);

    fixture.audit_failure(true);
    // (a) a proof no authority issued.
    let foreign = fixture.foreign_proof();
    let error = fixture
        .identities
        .apply_local(
            Some(&foreign),
            LocalCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: role(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        )
        .expect_err("a foreign proof is refused");
    expects_audit_unavailable(
        error,
        "local_authority_required",
        "local foreign-proof refusal audit failure",
    );

    // (b) a well-formed proof against a principal that does not exist.
    let error = fixture
        .identities
        .apply_local(
            Some(&proof),
            LocalCommand::AssignRole {
                principal: parse_principal("atomicity-absent-principal"),
                role: role(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        )
        .expect_err("an unknown principal is refused");
    expects_audit_unavailable(error, "not_found", "local not-found refusal audit failure");

    // Authority and evidence are untouched by either failure.
    assert_eq!(
        assignment_rows(&fixture, &viewer.principal),
        assignments_before
    );
    assert_eq!(fixture.count("role_assignments"), total_before);
    assert_eq!(fixture.count("local_authority_uses"), nonces_before);
    assert_eq!(fixture.count("audit_events"), audit_before);
    assert!(!policy_of(&fixture, &viewer).holds_builtin(BuiltinRole::Moderator));
    assert!(refusal_rows(&fixture, "authority.role.assign").is_empty());

    // With the fault removed the same refusal is recorded normally.
    fixture.audit_failure(false);
    let error = fixture
        .identities
        .apply_local(
            Some(&proof),
            LocalCommand::AssignRole {
                principal: parse_principal("atomicity-absent-principal"),
                role: role(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        )
        .expect_err("still refused");
    expects_domain_rejection(error, "not_found", None, "local retry");
    let refusals = refusal_rows(&fixture, "authority.role.assign");
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].outcome, "rejected");
    assert_eq!(refusals[0].metadata_json, "{\"reason\":\"not_found\"}");
    assert_eq!(
        fixture.count("local_authority_uses"),
        nonces_before,
        "a refused local command rolls its nonce back, so the proof was never consumed"
    );

    // Which is directly observable: the same proof still drives a successful command.
    let outcome = fixture
        .identities
        .apply_local(
            Some(&proof),
            LocalCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: role(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        )
        .expect("the proof was never consumed");
    assert!(matches!(outcome, LocalOutcome::Assignment(_)));
    assert_eq!(fixture.count("local_authority_uses"), nonces_before + 1);
    assert!(policy_of(&fixture, &viewer).holds_builtin(BuiltinRole::Moderator));
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_remote_not_found_refusal_whose_audit_fails_is_audit_unavailable() {
    let fixture = Fixture::open();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let audit_before = fixture.count("audit_events");

    fixture.audit_failure(true);
    let error = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::RevokeOverride(
                OverrideId::parse("atomicity-absent-override").expect("override id"),
            ),
        )
        .expect_err("an unknown override is refused");
    expects_audit_unavailable(error, "not_found", "remote not-found refusal audit failure");
    assert_eq!(fixture.count("audit_events"), audit_before);
    assert_eq!(fixture.count("principal_overrides"), 0);

    fixture.audit_failure(false);
    let error = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::RevokeOverride(
                OverrideId::parse("atomicity-absent-override").expect("override id"),
            ),
        )
        .expect_err("still refused");
    expects_domain_rejection(error, "not_found", None, "remote retry");
    assert_eq!(refusal_rows(&fixture, "authority.override.revoke").len(), 1);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn an_audit_failure_on_an_allowed_mutation_is_a_state_error_not_audit_unavailable() {
    let fixture = Fixture::open();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let viewer = fixture.subject(role(BuiltinRole::Viewer));

    let assignments_before = assignment_rows(&fixture, &viewer.principal);
    let audit_before = fixture.count("audit_events");

    fixture.audit_failure(true);
    let error = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: role(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        )
        .expect_err("the allowed mutation fails");
    // The inverse of a refusal-audit failure: an ALLOWED command whose own success audit
    // fails is infrastructure, not a denial.
    expects_state_failure(error.clone(), "allowed mutation audit failure");
    assert_ne!(error.code(), "audit_unavailable");
    assert_ne!(error.code(), "rollback_failed");
    assert!(!matches!(&error, ControlError::AuditUnavailable { .. }));
    assert!(!matches!(&error, ControlError::Rollback { .. }));
    assert!(!matches!(&error, ControlError::Rejected { .. }));
    assert!(error.reason().is_none());
    assert!(error.source().is_some(), "a state error retains a cause");

    // The mutation rolled back and no refusal was attempted: an infrastructure failure
    // inside the transaction is not a decision, so there is nothing to record.
    assert_eq!(
        assignment_rows(&fixture, &viewer.principal),
        assignments_before
    );
    assert_eq!(fixture.count("audit_events"), audit_before);
    assert!(refusal_rows(&fixture, "authority.role.assign").is_empty());
    assert!(!policy_of(&fixture, &viewer).holds_builtin(BuiltinRole::Moderator));

    // And the state error's own rendering discloses nothing.
    assert_eq!(format!("{error}"), "Control operation failed");
    assert_eq!(
        format!("{error:?}"),
        "ControlError { code: \"state_io\", reason: None }"
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// ------------------------- a domain rollback is not an infrastructure rollback

#[test]
fn every_domain_rejection_is_a_typed_code_and_never_an_infrastructure_rollback() {
    /// Records one refused command and remembers which documented code it produced.
    fn record(
        observed: &mut Vec<&'static str>,
        label: &'static str,
        result: crate::Result<AuthorityChange>,
        code: &'static str,
        reason: Option<DenyReason>,
    ) {
        let error = result.expect_err("the command must be refused");
        expects_domain_rejection(error, code, reason, label);
        observed.push(code);
    }

    let fixture = Fixture::open();
    let owner = fixture.owner();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let viewer = fixture.subject(role(BuiltinRole::Viewer));

    // A custom role plus a deliberately malformed stored capability row: trusted fixture
    // damage of the same shape an external import could leave behind.
    let AuthorityChange::Role(narrow) = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::CreateRole(custom_role("Narrow reader", 15, vec![files_read_grant()])),
        )
        .expect("create the custom role")
    else {
        panic!("CreateRole returns a role id");
    };
    fixture.sql(&format!(
        "INSERT INTO role_capabilities (role_id, capability, scope) VALUES ('{}','files.read','../invalid')",
        narrow.as_str()
    ));

    let stranger = Subject {
        principal: parse_principal("atomicity-unknown-principal"),
        device: DeviceId::parse("atomicity-unknown-device").expect("device id"),
    };
    let disabled = fixture.subject(role(BuiltinRole::Viewer));
    fixture.sql(&format!(
        "UPDATE principals SET status='disabled' WHERE id='{}'",
        disabled.principal.as_str()
    ));

    let mut observed: Vec<&'static str> = Vec::new();

    // forbidden / local_grant_only
    record(
        &mut observed,
        "forbidden-local-grant-only",
        fixture.identities.apply_remote(
            &owner,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: role(BuiltinRole::Administrator),
                expiry: Expiry::Never,
            },
        ),
        "forbidden",
        Some(DenyReason::LocalGrantOnly),
    );
    // forbidden / rank
    record(
        &mut observed,
        "forbidden-rank",
        fixture.identities.apply_remote(
            &admin,
            RemoteCommand::AssignRole {
                principal: owner.principal.clone(),
                role: role(BuiltinRole::Viewer),
                expiry: Expiry::Never,
            },
        ),
        "forbidden",
        Some(DenyReason::Rank),
    );
    // forbidden / no_identity: unknown lookup keys resolve to no authority at all.
    record(
        &mut observed,
        "forbidden-no-identity",
        fixture.identities.apply_remote(
            &stranger,
            RemoteCommand::RevokeDevice(viewer.device.clone()),
        ),
        "forbidden",
        Some(DenyReason::NoIdentity),
    );
    // not_found, on both the grant lookup and the principal lookup
    record(
        &mut observed,
        "not-found-grant",
        fixture.identities.apply_remote(
            &admin,
            RemoteCommand::RevokeAssignment(absent_assignment("nf-grant")),
        ),
        "not_found",
        None,
    );
    record(
        &mut observed,
        "not-found-principal",
        fixture.identities.apply_remote(
            &admin,
            RemoteCommand::AssignRole {
                principal: parse_principal("atomicity-absent-principal"),
                role: role(BuiltinRole::Viewer),
                expiry: Expiry::Never,
            },
        ),
        "not_found",
        None,
    );
    // conflict
    record(
        &mut observed,
        "conflict-role-name",
        fixture.identities.apply_remote(
            &admin,
            RemoteCommand::CreateRole(custom_role("Narrow READER", 15, vec![files_read_grant()])),
        ),
        "conflict",
        None,
    );
    // invalid_request: the target principal is disabled.
    record(
        &mut observed,
        "invalid-request-disabled-principal",
        fixture.identities.apply_remote(
            &admin,
            RemoteCommand::AssignRole {
                principal: disabled.principal.clone(),
                role: role(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        ),
        "invalid_request",
        None,
    );
    // code_invalid: a canonical token this database never issued.
    let error = redeem(
        &fixture,
        &never_issued_code(),
        "Ghost",
        "Ghost device",
        "atomicity-never-issued",
    )
    .expect_err("a never-issued code is refused");
    expects_domain_rejection(error, "code_invalid", None, "code-invalid");
    observed.push("code_invalid");
    // bootstrap_closed
    let error = fixture
        .identities
        .apply_local(
            Some(&fixture.proof(Purpose::OwnerBootstrap)),
            LocalCommand::Bootstrap(Default::default()),
        )
        .expect_err("bootstrap is closed");
    expects_domain_rejection(error, "bootstrap_closed", None, "bootstrap-closed");
    observed.push("bootstrap_closed");
    // role_invalid: a stored capability row that cannot be read strictly.
    record(
        &mut observed,
        "role-invalid-stored-row",
        fixture.identities.apply_remote(
            &admin,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: narrow.clone(),
                expiry: Expiry::Never,
            },
        ),
        "role_invalid",
        None,
    );

    // Every documented domain code on the mutation and transaction paths was reached.
    let mut expected = vec![
        // forbidden is reachable through three distinct reasons.
        "forbidden",
        "forbidden",
        "forbidden",
        "not_found",
        "not_found",
        "conflict",
        "invalid_request",
        "code_invalid",
        "bootstrap_closed",
        "role_invalid",
    ];
    expected.sort_unstable();
    observed.sort_unstable();
    assert_eq!(observed, expected);

    // Each one rolled back and left exactly one refusal record behind.
    assert_eq!(
        fixture.count("principals"),
        4,
        "owner + three fixture subjects"
    );
    assert_eq!(fixture.count("role_assignments"), 4);
    assert_eq!(
        fixture.count("roles"),
        7,
        "six built-ins plus one custom role"
    );
    assert_eq!(fixture.count("devices"), 4);
    assert!(fixture.identities.owner_exists().expect("owner lookup"));
    assert!(policy_of(&fixture, &viewer).holds_builtin(BuiltinRole::Viewer));
    assert!(!policy_of(&fixture, &disabled).has_current_identity());
    let entries = audit_entries(&fixture);
    let refusals: Vec<&AuditEntry> = entries
        .iter()
        .filter(|entry| entry.outcome != "success")
        .collect();
    assert_eq!(refusals.len(), 10, "one refusal per refused command");
    for refusal in &refusals {
        assert!(
            matches!(refusal.outcome.as_str(), "denied" | "rejected"),
            "a refusal outcome is a fixed word, got {}",
            refusal.outcome
        );
        assert!(
            refusal.metadata_json.starts_with("{\"reason\":\"")
                && refusal.metadata_json.ends_with("\"}"),
            "the only metadata key is `reason`, got {}",
            refusal.metadata_json
        );
        assert!(
            !refusal.metadata_json.contains("SELECT") && !refusal.metadata_json.contains("sqlite"),
            "a refusal never carries SQL text"
        );
    }
    let verification = fixture.audit.verify_chain().expect("audit chain");
    assert!(verification.valid);
    assert_eq!(verification.checked as usize, entries.len());
}

#[test]
fn an_infrastructure_failure_inside_the_transaction_rolls_back_and_is_never_a_refusal() {
    let fixture = Fixture::open();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let viewer = fixture.subject(role(BuiltinRole::Viewer));

    let assignments_before = assignment_rows(&fixture, &viewer.principal);
    let audit_before = fixture.count("audit_events");

    // A real aborting trigger on a table that is NOT the audit log: this failure cannot be
    // mistaken for an audit failure and cannot be reported as a denial.
    fixture.sql(
        "CREATE TRIGGER block_assignment BEFORE INSERT ON role_assignments \
         BEGIN SELECT RAISE(ABORT,'injected'); END",
    );
    let error = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: role(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        )
        .expect_err("the insert is aborted");
    expects_state_failure(error.clone(), "blocked assignment insert");
    assert_eq!(error.code(), "state_io");

    // The revocation that precedes the insert inside the same transaction was rolled back.
    assert_eq!(
        assignment_rows(&fixture, &viewer.principal),
        assignments_before
    );
    assert_eq!(fixture.count("role_assignments"), 2);
    assert_eq!(assignments_before[0].2, None);
    // And NO refusal was recorded, even though the audit table is perfectly healthy: an
    // infrastructure failure is not a decision, so there is nothing to record.
    assert_eq!(fixture.count("audit_events"), audit_before);
    assert!(refusal_rows(&fixture, "authority.role.assign").is_empty());
    assert_eq!(format!("{error}"), "Control operation failed");

    fixture.sql("DROP TRIGGER block_assignment");
    // Retryable once the cause is gone, and exactly one audit row.
    let change = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: role(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        )
        .expect("the command is retryable");
    assert!(matches!(change, AuthorityChange::Assignment(_)));
    assert_eq!(fixture.count("audit_events"), audit_before + 1);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_rollback_failure_is_unreachable_so_the_observable_infrastructure_code_is_state() {
    let fixture = Fixture::open();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let viewer = fixture.subject(role(BuiltinRole::Viewer));

    // `ControlError::Rollback` is built only from `TransactionError::Rollback`, which
    // `vesta-state` raises when `Transaction::rollback()` itself fails. Nothing on the public
    // surface can make SQLite refuse a ROLLBACK of a live immediate transaction: no schema
    // object runs inside a rollback, and the service owns the only connection to the file.
    // This test pins what IS observable instead: every rollback the service performs
    // succeeds, so a rolled-back command always surfaces either its typed domain code or a
    // plain `State` error, and the two are never confused.
    for (label, result, expected) in [
        (
            "forbidden",
            fixture.identities.apply_remote(
                &admin,
                RemoteCommand::AssignRole {
                    principal: viewer.principal.clone(),
                    role: role(BuiltinRole::Owner),
                    expiry: Expiry::Never,
                },
            ),
            "forbidden",
        ),
        (
            "not-found",
            fixture.identities.apply_remote(
                &admin,
                RemoteCommand::RevokeAssignment(absent_assignment("rb")),
            ),
            "not_found",
        ),
    ] {
        let error = result.expect_err("the command must be refused");
        assert!(
            !matches!(&error, ControlError::Rollback { .. }),
            "{label}: a rollback failure is not reachable"
        );
        assert!(
            !matches!(&error, ControlError::AuditUnavailable { .. }),
            "{label}: with a healthy audit this is a plain refusal"
        );
        assert_eq!(error.code(), expected, "{label}");
    }

    // The three failure shapes reachable on these paths are disjoint and never merged.
    fixture.audit_failure(true);
    let allowed = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::SetOverride {
                principal: viewer.principal.clone(),
                grant: files_read_grant(),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        )
        .expect_err("the success audit is aborted");
    let refused = fixture
        .identities
        .apply_remote(
            &admin,
            RemoteCommand::RevokeAssignment(absent_assignment("rb")),
        )
        .expect_err("still refused");
    assert!(matches!(&allowed, ControlError::State(_)));
    assert_eq!(allowed.code(), "state_io");
    assert!(matches!(&refused, ControlError::AuditUnavailable { .. }));
    assert_eq!(refused.code(), "audit_unavailable");
    assert_ne!(allowed.code(), refused.code());
    assert_eq!(refused.reason(), None);
    fixture.audit_failure(false);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// ------------------------------------------------------- E: nothing escapes a callback

/// Pins the only inherent accessor of a write handle: a borrowed connection. There is no
/// `commit`, `rollback`, `finish`, `into_inner` or `Drop`-escape, so a callback cannot end
/// the transaction the database owns, and the borrow prevents the handle outliving it.
fn only_write_accessor<'c>(handle: &'c WriteTransaction<'_>) -> &'c rusqlite::Connection {
    handle.connection()
}

/// The same for a read handle.
fn only_read_accessor<'c>(handle: &'c ReadTransaction<'_>) -> &'c rusqlite::Connection {
    handle.connection()
}

#[test]
fn the_public_identity_surface_exposes_no_transaction_and_no_callback() {
    // Compile-level property. Pinning the exact signature of every public method means
    // adding a `ReadTransaction`, `WriteTransaction`, `&Connection`, `StateDatabase` or
    // closure parameter (or return value) to any of them breaks the build of this test.
    let _: fn(StateDatabase) -> crate::Result<IdentityService> = IdentityService::new;
    let _: fn(&IdentityService, Option<&LocalProof>, LocalCommand) -> crate::Result<LocalOutcome> =
        IdentityService::apply_local;
    let _: fn(&IdentityService, RedeemOwner) -> crate::Result<OwnerAdded> =
        IdentityService::redeem_owner;
    let _: fn(&IdentityService, &Subject, RemoteCommand) -> crate::Result<AuthorityChange> =
        IdentityService::apply_remote;
    let _: fn(&IdentityService) -> crate::Result<bool> = IdentityService::owner_exists;
    let _: fn(&IdentityService, &PrincipalId) -> crate::Result<Option<SubjectRecord>> =
        IdentityService::subject_record;
    let _: fn(&IdentityService, Option<&Subject>) -> crate::Result<PolicySnapshot> =
        IdentityService::current_policy;
    let _: fn(&IdentityService) -> crate::Result<usize> = IdentityService::sweep_expired;

    // Runtime counterpart: `IdentityService::new` consumes the handle and the service keeps
    // it private, so a caller holding a service has no `StateDatabase` to commit, roll back
    // or nest through. What the service does own, it releases.
    let fixture = Fixture::open();
    let owner = fixture.owner();
    assert!(matches!(
        fixture
            .identities
            .apply_remote(&owner, RemoteCommand::RevokeDevice(owner.device.clone()))
            .expect("a real command completes"),
        AuthorityChange::Revoked
    ));
    assert_eq!(
        fixture
            .db
            .read_with::<String, StateError>(|tx| Ok(format!(
                "{tx:?} / {}",
                only_read_accessor(tx).is_autocommit()
            )))
            .expect("read snapshot"),
        "ReadTransaction { .. } / false",
        "a transaction handle renders no fields and its connection is still open"
    );
    assert_eq!(
        fixture
            .db
            .write_with::<String, StateError>(|tx| Ok(format!(
                "{tx:?} / {}",
                only_write_accessor(tx).is_autocommit()
            )))
            .expect("write transaction"),
        "WriteTransaction { .. } / false",
        "a transaction handle renders no fields and its connection is still open"
    );
    // The caller's own independent handle writes immediately after a command: if the
    // service had kept a transaction or an admission open, this would block for
    // BUSY_TIMEOUT_MS and then fail.
    fixture
        .db
        .write(|connection| {
            connection.execute_batch("SELECT 1")?;
            Ok(())
        })
        .expect("the caller's own handle is usable immediately after a command");
    // Closing from the caller's side succeeds, which it could not if a callback were still
    // admitted. `Fixture::drop` closes again, which is a documented no-op.
    fixture
        .db
        .clone()
        .close()
        .expect("no callback is still admitted");
}

#[test]
fn no_control_error_rendering_discloses_sql_paths_codes_nonces_or_proof_text() {
    let fixture = Fixture::open();

    // Real secrets in play for this database.
    let secret = fixture.bootstrap_code();
    let nonce = "atomicity-secret-nonce-0000000000";
    let os_actor = "atomicity-secret-os-actor";
    let owned_proof = fixture.proof_from(Attestation {
        nonce: nonce.to_owned(),
        purpose: Purpose::LocalRoleGrant,
        os_actor: os_actor.to_owned(),
        issued_at_ms: INITIAL_TIME_MS,
        expires_at_ms: INITIAL_TIME_MS + 60_000,
    });
    let database_path = fixture.db.directory().database_file().display().to_string();

    let owner = fixture.owner();
    let admin = fixture.subject(role(BuiltinRole::Administrator));
    let viewer = fixture.subject(role(BuiltinRole::Viewer));
    let foreign = fixture.foreign_proof();
    let production = IdentityService::new(fixture.db.clone()).expect("production service");

    // 1-4: domain rejections, with a healthy audit log.
    let mut collected: Vec<(&str, ControlError)> = vec![
        (
            "forbidden",
            fixture
                .identities
                .apply_remote(
                    &owner,
                    RemoteCommand::AssignRole {
                        principal: viewer.principal.clone(),
                        role: role(BuiltinRole::Administrator),
                        expiry: Expiry::Never,
                    },
                )
                .expect_err("refused"),
        ),
        (
            "not-found",
            fixture
                .identities
                .apply_remote(
                    &admin,
                    RemoteCommand::RevokeAssignment(absent_assignment("leak")),
                )
                .expect_err("refused"),
        ),
        (
            "local-authority-required",
            fixture
                .identities
                .apply_local(
                    Some(&foreign),
                    LocalCommand::AssignRole {
                        principal: viewer.principal.clone(),
                        role: role(BuiltinRole::Viewer),
                        expiry: Expiry::Never,
                    },
                )
                .expect_err("refused"),
        ),
        (
            "production-unavailable",
            production
                .apply_local(
                    Some(&owned_proof),
                    LocalCommand::Bootstrap(Default::default()),
                )
                .expect_err("refused"),
        ),
    ];

    // 5: unreadable stored data is infrastructure, not a decision.
    fixture.sql(&format!(
        "INSERT INTO principals (id, server_id, display_name, status, created_at, updated_at) \
         VALUES ('atomicity-corrupt-principal','{}','','active',{},{})",
        fixture.db.server_id(),
        INITIAL_TIME_MS,
        INITIAL_TIME_MS
    ));
    collected.push((
        "corrupt-stored-identity",
        fixture
            .identities
            .subject_record(&parse_principal("atomicity-corrupt-principal"))
            .expect_err("unreadable stored name"),
    ));

    // 6: an infrastructure failure on a table that is not the audit log.
    fixture.sql(
        "CREATE TRIGGER block_assignment BEFORE INSERT ON role_assignments \
         BEGIN SELECT RAISE(ABORT,'injected'); END",
    );
    collected.push((
        "blocked-insert",
        fixture
            .identities
            .apply_remote(
                &admin,
                RemoteCommand::AssignRole {
                    principal: viewer.principal.clone(),
                    role: role(BuiltinRole::Moderator),
                    expiry: Expiry::Never,
                },
            )
            .expect_err("aborted"),
    ));
    fixture.sql("DROP TRIGGER block_assignment");

    // 7-9: the two audit-driven shapes, with the fault installed.
    fixture.audit_failure(true);
    collected.push((
        "success-audit-failure",
        fixture
            .identities
            .apply_remote(
                &admin,
                RemoteCommand::SetOverride {
                    principal: viewer.principal.clone(),
                    grant: files_read_grant(),
                    effect: Effect::Allow,
                    expiry: Expiry::Never,
                },
            )
            .expect_err("aborted"),
    ));
    collected.push((
        "refusal-audit-failure",
        fixture
            .identities
            .apply_remote(
                &owner,
                RemoteCommand::AssignRole {
                    principal: viewer.principal.clone(),
                    role: role(BuiltinRole::Administrator),
                    expiry: Expiry::Never,
                },
            )
            .expect_err("aborted"),
    ));
    collected.push((
        "production-refusal-audit-failure",
        production
            .apply_local(
                Some(&owned_proof),
                LocalCommand::Bootstrap(Default::default()),
            )
            .expect_err("aborted"),
    ));
    fixture.audit_failure(false);

    assert_eq!(collected.len(), 9);

    // The exact rendering of a plain decision carries nothing but fixed codes and reasons.
    assert_eq!(
        format!("{:?}", collected[0].1),
        "ControlError { code: \"forbidden\", reason: Some(LocalGrantOnly) }"
    );
    assert_eq!(
        format!("{:?}", collected[1].1),
        "ControlError { code: \"not_found\", reason: None }"
    );
    assert_eq!(
        format!("{:?}", collected[4].1),
        "ControlError { code: \"state_identity_invalid\", reason: None }"
    );
    assert_eq!(
        format!("{:?}", collected[5].1),
        "ControlError { code: \"state_io\", reason: None }"
    );
    assert_eq!(
        format!("{:?}", collected[6].1),
        "ControlError { code: \"state_io\", reason: None }"
    );
    assert_eq!(
        format!("{:?}", collected[7].1),
        "ControlError { code: \"audit_unavailable\", reason: None, \
         primary: ControlError { code: \"forbidden\", reason: Some(LocalGrantOnly) }, \
         audit_failure: ControlError { code: \"state_io\", reason: None } }"
    );

    let needles = [
        secret.as_str(),
        nonce,
        os_actor,
        database_path.as_str(),
        "select",
        "insert",
        "update",
        "delete from",
        "sqlite",
        "audit_events",
        "role_assignments",
        "principal_overrides",
        "bootstrap_codes",
        "raise",
        "aborted",
        "injected",
        "trigger",
    ];

    let mut codes: Vec<&str> = Vec::new();
    for (label, error) in &collected {
        assert_eq!(
            format!("{error}"),
            "Control operation failed",
            "{label}: Display is a constant"
        );
        // The whole rendered error chain, not just the top level.
        let mut rendered = format!("{error:?}");
        let mut source = error.source();
        while let Some(cause) = source {
            rendered.push('\n');
            rendered.push_str(&format!("{cause}\n{cause:?}"));
            source = cause.source();
        }
        let lowered = rendered.to_lowercase();
        for needle in needles {
            assert!(
                !lowered.contains(&needle.to_lowercase()),
                "{label}: the rendering leaked {needle:?}: {rendered}"
            );
        }
        // The error never echoes the code, the nonce or the OS actor as a value.
        assert!(
            !rendered.contains(&secret)
                && !rendered.contains(nonce)
                && !rendered.contains(os_actor),
            "{label}: a secret reached the rendering: {rendered}"
        );
        codes.push(error.code());
    }

    // Every reachable shape is one of the documented fixed codes, never SQL text.
    assert_eq!(
        codes,
        vec![
            "forbidden",
            "not_found",
            "local_authority_required",
            "local_authority_required",
            "state_identity_invalid",
            "state_io",
            "state_io",
            "audit_unavailable",
            "audit_unavailable",
        ]
    );
}

// ------------------------------------------------------------------- F: nesting

#[test]
fn the_identity_service_never_nests_a_database_call() {
    // Statically: the signature pinning above shows no public identity method accepts a
    // closure, and `IdentityService::new` takes ownership of the `StateDatabase` and keeps
    // it private, so a caller has no handle with which to re-enter a command from inside
    // another command's callback. There is no public nesting seam to use.
    //
    // Dynamically: `finish` appends the refusal only after the command's own transaction
    // has been rolled back and its admission released. Had the service nested a second
    // `db.write_with` inside the callback, `vesta-state` would have refused it with
    // `state_nested_transaction` and the refusal row below could not exist at all.
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let viewer = fixture.subject(role(BuiltinRole::Viewer));

    let error = fixture
        .identities
        .apply_remote(
            &owner,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: role(BuiltinRole::Administrator),
                expiry: Expiry::Never,
            },
        )
        .expect_err("refused");
    expects_domain_rejection(
        error,
        "forbidden",
        Some(DenyReason::LocalGrantOnly),
        "refusal",
    );

    // The refusal persisted, so the append really did run outside the failed transaction.
    let refusals = refusal_rows(&fixture, "authority.role.assign");
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].outcome, "denied");
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);

    // The nesting codes the identity crate relies on never appearing on any of the paths
    // that would have to nest in order to record a refusal.
    let mut codes: Vec<String> = vec![
        fixture
            .identities
            .apply_remote(
                &owner,
                RemoteCommand::RevokeAssignment(absent_assignment("nest")),
            )
            .expect_err("refused")
            .code()
            .to_owned(),
        fixture
            .identities
            .apply_remote(
                &owner,
                RemoteCommand::AssignRole {
                    principal: viewer.principal.clone(),
                    role: role(BuiltinRole::Administrator),
                    expiry: Expiry::Never,
                },
            )
            .expect_err("refused")
            .code()
            .to_owned(),
    ];
    fixture.audit_failure(true);
    codes.push(
        fixture
            .identities
            .apply_remote(
                &owner,
                RemoteCommand::AssignRole {
                    principal: viewer.principal.clone(),
                    role: role(BuiltinRole::Administrator),
                    expiry: Expiry::Never,
                },
            )
            .expect_err("refused")
            .code()
            .to_owned(),
    );
    fixture.audit_failure(false);
    assert_eq!(
        codes,
        vec![
            "not_found".to_owned(),
            "forbidden".to_owned(),
            "audit_unavailable".to_owned()
        ]
    );
    // The identity service must never reach a nested-call refusal: every error it can
    // return is either a typed domain rejection or a plain infrastructure `State` code.
    // These are the real `vesta-state` codes, so this leg is a live assertion rather than
    // a comparison against strings that do not exist.
    for code in &codes {
        assert_ne!(code, "state_nested_transaction", "{code}");
        assert_ne!(code, "state_nested_call", "{code}");
        assert_ne!(code, "state_close_in_callback", "{code}");
        assert_ne!(code, "state_invalid_request", "{code}");
        assert_ne!(code, "invalid_request", "{code}");
    }
}
