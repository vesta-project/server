//! Policy evaluation, deny precedence, fail-closed stored rows, tier reservation,
//! primary-role replacement, expiry immediacy and built-in-role immutability.
//!
//! Every assertion runs against the real service, real SQLite state, real audit rows
//! and the fixture clock. Nothing here inspects private state or stubs an authority.
use super::support::{Fixture, INITIAL_TIME_MS, assignment, parse_principal};
use crate::identity::{
    AssignmentId, AuthorityChange, BuiltinRole, Capability, CustomRank, CustomRole, Decision,
    DenyReason, DeviceId, Effect, Expiry, LocalCommand, LocalOutcome, OverrideId, PolicySnapshot,
    PrincipalId, PrincipalStatus, Purpose, RemoteCommand, Resource, ResourceScope, RoleGrant,
    RoleId, RoleName, Subject,
};
use std::time::Duration;
use vesta_state::Clock;

// ---------------------------------------------------------------- shared helpers

fn builtin(role: BuiltinRole) -> RoleId {
    RoleId::from(role)
}

fn snapshot_of(fixture: &Fixture, subject: &Subject) -> PolicySnapshot {
    fixture
        .identities
        .current_policy(Some(subject))
        .expect("current policy")
}

fn resource(text: &str) -> Resource {
    Resource::parse(text).unwrap_or_else(|_| panic!("{text:?} must be a canonical resource"))
}

fn scope(text: &str) -> ResourceScope {
    ResourceScope::parse(text).unwrap_or_else(|_| panic!("{text:?} must be a canonical scope"))
}

fn grant(capability: Capability, text: &str) -> RoleGrant {
    RoleGrant::new(capability, scope(text)).expect("canonical role grant")
}

/// One scalar from a fixture query. `table`/`id` are test literals or fixture
/// generated identifiers, never caller input.
fn scalar<T: rusqlite::types::FromSql>(fixture: &Fixture, statement: &str) -> T {
    fixture
        .db
        .read(|connection| Ok(connection.query_row(statement, [], |row| row.get(0))?))
        .unwrap_or_else(|error| panic!("{statement}: {}", error.code()))
}

fn text_column(fixture: &Fixture, statement: &str) -> Vec<String> {
    fixture
        .db
        .read(|connection| {
            let mut statement = connection.prepare(statement)?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .unwrap_or_else(|error| panic!("{statement}: {}", error.code()))
}

fn unrevoked_assignment_ids(fixture: &Fixture, principal: &PrincipalId) -> Vec<String> {
    text_column(
        fixture,
        &format!(
            "SELECT id FROM role_assignments WHERE principal_id = '{}' AND revoked_at IS NULL ORDER BY id",
            principal.as_str()
        ),
    )
}

/// Unrevoked rows, expired or not: the row-level history the service must never
/// resurrect.
fn unrevoked_role_ids(fixture: &Fixture, principal: &PrincipalId) -> Vec<String> {
    text_column(
        fixture,
        &format!(
            "SELECT role_id FROM role_assignments WHERE principal_id = '{}' AND revoked_at IS NULL ORDER BY id",
            principal.as_str()
        ),
    )
}

fn revoked_rows(fixture: &Fixture, principal: &PrincipalId, role_id: &str) -> i64 {
    scalar(
        fixture,
        &format!(
            "SELECT count(*) FROM role_assignments WHERE principal_id = '{}' AND role_id = '{role_id}' AND revoked_at IS NOT NULL",
            principal.as_str()
        ),
    )
}

fn expiry_marker(fixture: &Fixture, table: &str, id: &str) -> Option<i64> {
    fixture
        .db
        .read(|connection| {
            Ok(connection.query_row(
                &format!("SELECT expiry_recorded_at FROM {table} WHERE id = '{id}'"),
                [],
                |row| row.get(0),
            )?)
        })
        .expect("read the expiry marker column")
}

fn authority_counts(fixture: &Fixture) -> Vec<i64> {
    [
        "principals",
        "devices",
        "roles",
        "role_capabilities",
        "role_assignments",
        "principal_overrides",
    ]
    .iter()
    .map(|table| fixture.count(table))
    .collect::<Vec<i64>>()
}

/// Deliberately plants a stored override whose capability id is not a known
/// identifier, which `Fixture::stored_override` cannot express.
fn plant_override(
    fixture: &Fixture,
    subject: &Subject,
    capability: &str,
    scope: &str,
    effect: &str,
) {
    let id = uuid::Uuid::new_v4().to_string();
    fixture.sql(&format!(
        "INSERT INTO principal_overrides (id, principal_id, capability, scope, effect, granted_by, created_at, expires_at) \
         VALUES ('{id}','{}','{capability}','{scope}','{effect}','fixture',{},NULL)",
        subject.principal.as_str(),
        fixture.clock.millis()
    ));
}

fn count_action(fixture: &Fixture, action: &str) -> usize {
    fixture
        .audit_actions()
        .iter()
        .filter(|recorded| recorded.as_str() == action)
        .count()
}

/// Every observable authority statement about a subject must be "nothing at all".
fn assert_no_authority(fixture: &Fixture, label: &str, subject: Option<&Subject>) {
    let policy = fixture
        .identities
        .current_policy(subject)
        .expect("current policy is observable");
    assert!(
        !policy.has_current_identity(),
        "{label}: has_current_identity"
    );
    assert_eq!(policy.principal(), None, "{label}: principal");
    assert_eq!(policy.device(), None, "{label}: device");
    assert_eq!(policy.rank(), 0, "{label}: rank");
    assert!(policy.roles().is_empty(), "{label}: roles");
    for builtin in BuiltinRole::ALL {
        assert!(!policy.holds_builtin(*builtin), "{label}: {builtin:?}");
    }
    for capability in Capability::ALL {
        // With and without a resource: no identity precedes every other rule.
        assert_eq!(
            policy.check(*capability, None),
            Decision::Deny(DenyReason::NoIdentity),
            "{label}: {} without a resource",
            capability.id()
        );
        assert_eq!(
            policy.check(*capability, Some(&Resource::ROOT)),
            Decision::Deny(DenyReason::NoIdentity),
            "{label}: {} with a resource",
            capability.id()
        );
        assert!(
            !policy.holds_entirely(*capability, &ResourceScope::ALL),
            "{label}: holds_entirely {}",
            capability.id()
        );
    }
}

/// Capabilities whose grant shape is global, so a stored scope is the only variable.
fn global_capabilities() -> Vec<Capability> {
    Capability::ALL
        .iter()
        .copied()
        .filter(|capability| !capability.scoped() && capability.delegable())
        .collect()
}

const RESERVED: [Capability; 6] = [
    Capability::BackupsRestore,
    Capability::RecoveryExport,
    Capability::CredentialsManage,
    Capability::SecurityManage,
    Capability::OwnerRecovery,
    Capability::AgentInstall,
];

// ------------------------------------------------- Java: unknownDisabledMismatched…

#[test]
fn unknown_disabled_mismatched_and_revoked_identities_resolve_no_authority() {
    let fixture = Fixture::open();
    let first = fixture.subject(builtin(BuiltinRole::Viewer));
    let second = fixture.subject(builtin(BuiltinRole::Moderator));
    assert!(snapshot_of(&fixture, &first).has_current_identity());
    assert!(snapshot_of(&fixture, &second).has_current_identity());

    // An absent lookup key is not an identity.
    assert_no_authority(&fixture, "no subject at all", None);
    assert_no_authority(
        &fixture,
        "unknown principal",
        Some(&Subject {
            principal: parse_principal("no-such-principal"),
            device: first.device.clone(),
        }),
    );
    assert_no_authority(
        &fixture,
        "unknown device",
        Some(&Subject {
            principal: first.principal.clone(),
            device: DeviceId::parse("no-such-device").expect("device id"),
        }),
    );
    // A principal and a device that do not belong together are not one identity.
    assert_no_authority(
        &fixture,
        "mismatched pair",
        Some(&Subject {
            principal: first.principal.clone(),
            device: second.device.clone(),
        }),
    );

    fixture.sql(&format!(
        "UPDATE principals SET status='disabled' WHERE id='{}'",
        first.principal.as_str()
    ));
    assert_no_authority(&fixture, "disabled principal", Some(&first));
    // Disabling a principal does not disable anyone else's device.
    assert!(snapshot_of(&fixture, &second).has_current_identity());

    fixture.sql(&format!(
        "UPDATE devices SET revoked_at={} WHERE id='{}'",
        fixture.clock.millis(),
        second.device.as_str()
    ));
    assert_no_authority(&fixture, "revoked device", Some(&second));

    // The subject record still exists and still reports the revocation: identity
    // resolution is what fails, not the stored history.
    let record = fixture
        .identities
        .subject_record(&second.principal)
        .expect("subject record")
        .expect("record exists");
    assert_eq!(record.principal.status, PrincipalStatus::Active);
    assert_eq!(record.principal.id, second.principal);
    assert_eq!(record.devices.len(), 1);
    assert!(record.devices[0].revoked());
    let disabled = fixture
        .identities
        .subject_record(&first.principal)
        .expect("subject record")
        .expect("record exists");
    assert_eq!(disabled.principal.status, PrincipalStatus::Disabled);
    assert!(
        !disabled.devices[0].revoked(),
        "the device row itself is intact"
    );
}

/// Plants the same damaged/imported row the Java reference plants: identity rows
/// bound to a server id that is not this server. Every identity table references
/// `server_identity`, so the row can only be written by a connection that does not
/// enforce references — exactly the external import the service must survive.
fn plant_foreign_server_identity(fixture: &Fixture, subject: &Subject) {
    let connection = rusqlite::Connection::open_with_flags(
        fixture.db.directory().database_file(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("open a raw fixture connection");
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .expect("disable reference enforcement");
    let enforced: i64 = connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .expect("read the fixture pragma");
    assert_eq!(
        enforced, 0,
        "a raw fixture connection must not enforce references"
    );
    connection
        .execute_batch(&format!(
            "UPDATE principals SET server_id='foreign-server' WHERE id='{principal}';\
             UPDATE devices SET server_id='foreign-server' WHERE id='{device}';",
            principal = subject.principal.as_str(),
            device = subject.device.as_str()
        ))
        .expect("plant a foreign server identity row");
    // The planted rows really do dangle; that damaged state is what is under test.
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .expect("prepare foreign key check");
    let mut rows = statement.query([]).expect("run foreign key check");
    let mut violations = 0;
    while let Some(row) = rows.next().expect("read foreign key violations") {
        let table = row.get::<_, String>(0).expect("read the violating table");
        if matches!(table.as_str(), "principals" | "devices") {
            violations += 1;
        }
    }
    assert_eq!(violations, 2, "both planted rows must dangle");
}

#[test]
fn a_principal_on_a_different_server_id_resolves_no_authority() {
    let fixture = Fixture::open();
    let home = fixture.subject(builtin(BuiltinRole::Maintainer));
    let stranger = fixture.subject(builtin(BuiltinRole::Administrator));
    assert!(snapshot_of(&fixture, &home).has_current_identity());

    plant_foreign_server_identity(&fixture, &stranger);
    let before = authority_counts(&fixture);

    // The control subject is untouched and still resolves.
    assert!(snapshot_of(&fixture, &home).has_current_identity());
    assert_no_authority(&fixture, "foreign server principal", Some(&stranger));

    let error = fixture
        .identities
        .apply_remote(&stranger, RemoteCommand::RevokeDevice(home.device.clone()))
        .expect_err("a foreign-server subject has no authority");
    assert_eq!(error.code(), "forbidden");
    assert_eq!(error.reason(), Some(DenyReason::NoIdentity));
    assert_eq!(
        authority_counts(&fixture),
        before,
        "a refused foreign-server command mutates nothing"
    );
    assert!(snapshot_of(&fixture, &home).has_current_identity());
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn every_remote_command_from_an_unresolved_subject_is_refused_and_mutates_nothing() {
    let fixture = Fixture::open();
    fixture.owner();
    let target = fixture.subject(builtin(BuiltinRole::Viewer));
    fixture.sql(&format!(
        "UPDATE principals SET status='disabled' WHERE id='{}'",
        target.principal.as_str()
    ));
    assert_no_authority(&fixture, "disabled principal", Some(&target));

    let before = authority_counts(&fixture);
    let custom = CustomRole::new(
        RoleName::parse("Refused role").expect("role name"),
        CustomRank::new(15).expect("rank"),
        vec![grant(Capability::FilesRead, "config/**")],
    )
    .expect("custom role");
    let grant = grant(Capability::FilesRead, "config/**");
    let commands: Vec<(&'static str, RemoteCommand)> = vec![
        (
            "authority.role.assign",
            RemoteCommand::AssignRole {
                principal: target.principal.clone(),
                role: builtin(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        ),
        (
            "authority.override.set",
            RemoteCommand::SetOverride {
                principal: target.principal.clone(),
                grant,
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        ),
        (
            "authority.role.revoke",
            RemoteCommand::RevokeAssignment(
                AssignmentId::parse("refused-assignment").expect("assignment id"),
            ),
        ),
        (
            "authority.override.revoke",
            RemoteCommand::RevokeOverride(
                OverrideId::parse("refused-override").expect("override id"),
            ),
        ),
        (
            "identity.device.revoke",
            RemoteCommand::RevokeDevice(target.device.clone()),
        ),
        ("authority.role.create", RemoteCommand::CreateRole(custom)),
    ];
    assert_eq!(
        commands.len(),
        6,
        "every remote command variant must be exercised"
    );

    for (action, command) in commands {
        let error = fixture
            .identities
            .apply_remote(&target, command)
            .expect_err("an unresolved subject may command nothing");
        assert_eq!(error.code(), "forbidden", "{action}");
        assert_eq!(error.reason(), Some(DenyReason::NoIdentity), "{action}");
        assert_eq!(
            fixture.audit_actions().last().map(String::as_str),
            Some(action),
            "{action} must be recorded"
        );
        assert_eq!(
            authority_counts(&fixture),
            before,
            "{action} mutated authority"
        );
    }

    let refusals = fixture
        .audit
        .page(0, 100)
        .expect("audit page")
        .into_iter()
        .filter(|entry| entry.outcome == "denied")
        .collect::<Vec<_>>();
    assert_eq!(refusals.len(), 6);
    for refusal in &refusals {
        assert_eq!(refusal.metadata_json, r#"{"reason":"no_identity"}"#);
    }
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// ------------------------------------------ Java: explicitDenyBeatsOwnerAndAllow…

#[test]
fn an_explicit_deny_beats_owner_and_an_individual_allow_cannot_undo_it() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let config = resource("config/public.txt");
    let secret = resource("config/private/token.txt");
    let shouted = resource("CONFIG/PRIVATE/token.txt");

    // Owner holds files.read at the whole scope until a deny says otherwise.
    let policy = snapshot_of(&fixture, &owner);
    assert!(policy.has_current_identity());
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&config)),
        Decision::Allow
    );

    fixture.stored_override(
        &owner,
        Capability::FilesRead,
        "config/private/**",
        Effect::Deny,
        None,
    );
    fixture.stored_override(
        &owner,
        Capability::FilesRead,
        "config/private/**",
        Effect::Allow,
        None,
    );
    let policy = snapshot_of(&fixture, &owner);
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&secret)),
        Decision::Deny(DenyReason::ExplicitDeny)
    );
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&secret)).reason(),
        "explicit_deny"
    );
    // Deny matching is ASCII case-insensitive even though allow matching is not.
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&shouted)),
        Decision::Deny(DenyReason::ExplicitDeny)
    );
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&config)),
        Decision::Allow
    );
    assert!(!policy.holds_entirely(Capability::FilesRead, &scope("config/**")));
    assert!(!policy.holds_entirely(Capability::FilesRead, &scope("config/private")));
    assert!(!policy.holds_entirely(Capability::FilesRead, &scope("config/private/**")));
    assert!(policy.holds_entirely(Capability::FilesRead, &scope("saves/**")));

    // An individual allow of the same capability and scope adds nothing.
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&secret)),
        Decision::Deny(DenyReason::ExplicitDeny)
    );

    // A deny on a global capability removes it from Owner too, and nothing else.
    fixture.stored_override(
        &owner,
        Capability::LifecycleForceKill,
        "*",
        Effect::Deny,
        None,
    );
    let policy = snapshot_of(&fixture, &owner);
    assert_eq!(
        policy.check(Capability::LifecycleForceKill, None),
        Decision::Deny(DenyReason::ExplicitDeny)
    );
    assert!(!policy.holds_entirely(Capability::LifecycleForceKill, &ResourceScope::ALL));
    assert_eq!(
        policy.check(Capability::LifecycleStop, None),
        Decision::Allow
    );
    assert_eq!(
        policy.check(Capability::LifecycleStart, None),
        Decision::Allow
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

// ------------------- Java: malformedStoredDeniesFailClosedAndReservedTiersCannot…

#[test]
fn an_unreadable_stored_deny_blocks_its_capability_and_is_never_silently_dropped() {
    let fixture = Fixture::open();
    let subject = fixture.subject(builtin(BuiltinRole::Viewer));
    let candidates = global_capabilities();
    // Empty, parent traversal, ambiguous separator, reserved device name, a colon
    // and a non-ASCII path. `*` is deliberately absent: it is the canonical whole
    // scope, not a malformed one, and is covered by
    // `a_stored_deny_of_the_whole_scope_denies_every_resource`.
    let unreadable = [
        "",
        "../etc",
        "a//b",
        "logs/CON",
        "config/a:stream",
        "\u{914d}\u{7f6e}/files",
    ];
    assert!(
        candidates.len() > unreadable.len(),
        "enough distinct global capabilities are needed"
    );

    for (capability, stored) in candidates.iter().copied().zip(unreadable) {
        assert!(
            ResourceScope::parse(stored).is_err(),
            "{stored:?} must be an unreadable stored scope"
        );
        // A whole-path allow exists, so a dropped deny would be directly observable.
        fixture.stored_override(&subject, capability, "*", Effect::Allow, None);
        fixture.stored_override(&subject, capability, stored, Effect::Deny, None);
    }

    let policy = snapshot_of(&fixture, &subject);
    assert!(policy.has_current_identity());
    for capability in candidates.iter().copied().take(unreadable.len()) {
        assert_eq!(
            policy.check(capability, None),
            Decision::Deny(DenyReason::ExplicitDeny),
            "{}: an unreadable deny must block its capability",
            capability.id()
        );
        assert!(
            !policy.holds_entirely(capability, &ResourceScope::ALL),
            "{}: an unreadable deny must block delegation of its whole scope",
            capability.id()
        );
    }
    // One unreadable deny blocks its capability, not the whole resolution.
    let untouched = candidates[unreadable.len()];
    assert_eq!(
        policy.check(untouched, None),
        Decision::Deny(DenyReason::NotGranted),
        "an unreadable stored scope must not fail unrelated capabilities"
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_stored_deny_of_the_whole_scope_denies_every_resource() {
    let fixture = Fixture::open();
    let subject = fixture.subject(builtin(BuiltinRole::Owner));
    assert_eq!(
        ResourceScope::parse("*")
            .expect("the whole scope is canonical")
            .text(),
        "*"
    );
    fixture.stored_override(&subject, Capability::StatusView, "*", Effect::Deny, None);
    fixture.stored_override(&subject, Capability::FilesRead, "*", Effect::Deny, None);
    let policy = snapshot_of(&fixture, &subject);
    assert_eq!(
        policy.check(Capability::StatusView, None),
        Decision::Deny(DenyReason::ExplicitDeny)
    );
    for text in ["", "config/options.txt", "logs/latest.log"] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(text))),
            Decision::Deny(DenyReason::ExplicitDeny),
            "{text:?} is covered by the whole-scope deny"
        );
    }
    assert!(!policy.holds_entirely(Capability::StatusView, &ResourceScope::ALL));
    assert!(!policy.holds_entirely(Capability::FilesRead, &ResourceScope::ALL));
    // The rest of Owner is untouched.
    assert_eq!(
        policy.check(Capability::LifecycleStop, None),
        Decision::Allow
    );
    assert_eq!(
        policy.check(
            Capability::FilesCreate,
            Some(&resource("config/options.txt"))
        ),
        Decision::Allow
    );
}

#[test]
fn an_unknown_capability_id_in_a_stored_deny_fails_the_whole_resolution_closed() {
    let fixture = Fixture::open();
    let subject = fixture.subject(builtin(BuiltinRole::Maintainer));
    assert!(snapshot_of(&fixture, &subject).has_current_identity());
    let audited = fixture.audit_actions().len();

    // An unreadable deny must never be silently dropped.
    plant_override(&fixture, &subject, "lifecycle.manage", "*", "deny");
    for capability in Capability::ALL {
        assert!(Capability::from_id(capability.id()).is_some());
    }
    assert!(Capability::from_id("lifecycle.manage").is_none());
    assert!(Capability::from_id("*").is_none());
    assert!(Capability::from_id("not-a-capability").is_none());

    let error = fixture
        .identities
        .current_policy(Some(&subject))
        .expect_err("an unreadable deny must fail resolution closed");
    assert_eq!(error.code(), "state_identity_invalid");
    assert!(error.reason().is_none(), "a corrupt row is not a decision");
    assert_eq!(
        fixture.audit_actions().len(),
        audited,
        "an infrastructure failure is never recorded as a denial"
    );

    // A remote command cannot run past the same corrupt row either, and is still
    // not reported as a policy decision.
    let error = fixture
        .identities
        .apply_remote(
            &subject,
            RemoteCommand::RevokeDevice(subject.device.clone()),
        )
        .expect_err("a corrupt stored deny must fail the command closed");
    assert_eq!(error.code(), "state_identity_invalid");
    assert!(error.reason().is_none());
    assert_eq!(fixture.audit_actions().len(), audited);

    // The unknown id in an ALLOW is merely unusable; it grants nothing and does
    // not block anything else.
    fixture.sql(&format!(
        "UPDATE principal_overrides SET effect='allow' WHERE capability='lifecycle.manage' AND principal_id='{}'",
        subject.principal.as_str()
    ));
    let policy = snapshot_of(&fixture, &subject);
    assert!(policy.has_current_identity());
    assert_eq!(policy.rank(), BuiltinRole::Maintainer.rank());
    // Capabilities the role really holds still resolve, so nothing was over-blocked.
    for capability in [Capability::LifecycleStart, Capability::LifecycleForceKill] {
        assert_eq!(
            policy.check(capability, None),
            Decision::Allow,
            "{}",
            capability.id()
        );
    }
    // Nothing that the role does not hold is granted, and an unknown stored
    // capability id never appears as a capability at all.
    for capability in [
        Capability::ConfigView,
        Capability::ConfigEdit,
        Capability::AuditView,
        Capability::RolesAssign,
    ] {
        assert_eq!(
            policy.check(capability, None),
            Decision::Deny(DenyReason::NotGranted),
            "{}",
            capability.id()
        );
        assert!(!policy.holds_entirely(capability, &ResourceScope::ALL));
    }
}

#[test]
fn a_stored_override_cannot_confer_a_reserved_tier_or_a_local_only_capability() {
    let fixture = Fixture::open();
    let viewer = fixture.subject(builtin(BuiltinRole::Viewer));
    let administrator = fixture.subject(builtin(BuiltinRole::Administrator));

    // Every reserved capability is planted as an explicit, unrevoked, unexpired
    // whole-path allow for a principal with no administrative membership at all.
    for capability in RESERVED {
        fixture.stored_override(&viewer, capability, "*", Effect::Allow, None);
    }
    let policy = snapshot_of(&fixture, &viewer);
    assert!(!policy.holds_builtin(BuiltinRole::Owner));
    assert!(!policy.holds_builtin(BuiltinRole::Administrator));
    assert_eq!(policy.rank(), BuiltinRole::Viewer.rank());
    for capability in RESERVED {
        assert!(
            !policy.check(capability, None).allowed(),
            "{} must not become effective",
            capability.id()
        );
        assert!(
            !policy.holds_entirely(capability, &ResourceScope::ALL),
            "{} must not become delegable",
            capability.id()
        );
    }
    // The exact reasons: a local-only capability is refused as local-only, and the
    // tier-reserved ones are refused because no active built-in tier is held.
    for capability in [Capability::OwnerRecovery, Capability::AgentInstall] {
        assert_eq!(
            policy.check(capability, None),
            Decision::Deny(DenyReason::LocalOnly),
            "{}",
            capability.id()
        );
    }
    for capability in [
        Capability::BackupsRestore,
        Capability::RecoveryExport,
        Capability::CredentialsManage,
        Capability::SecurityManage,
    ] {
        assert_eq!(
            policy.check(capability, None),
            Decision::Deny(DenyReason::Reserved),
            "{}",
            capability.id()
        );
    }
    // The positive control: the tier comes from active built-in membership, not
    // from a numeric rank and not from an override.
    let administrator_policy = snapshot_of(&fixture, &administrator);
    assert!(administrator_policy.holds_builtin(BuiltinRole::Administrator));
    assert_eq!(
        administrator_policy.check(Capability::SecurityManage, None),
        Decision::Allow
    );
    assert_eq!(
        administrator_policy.check(Capability::CredentialsManage, None),
        Decision::Allow
    );
    assert_eq!(
        administrator_policy.check(Capability::BackupsRestore, None),
        Decision::Deny(DenyReason::Reserved),
        "administrator is not the owner tier"
    );
    for capability in [Capability::OwnerRecovery, Capability::AgentInstall] {
        assert_eq!(
            administrator_policy.check(capability, None),
            Decision::Deny(DenyReason::LocalOnly)
        );
    }
}

#[test]
fn a_remote_set_override_of_a_local_only_or_reserved_capability_is_refused() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let target = fixture.subject(builtin(BuiltinRole::Maintainer));
    let before = fixture.count("principal_overrides");

    for (capability, reason) in [
        (Capability::OwnerRecovery, DenyReason::LocalOnly),
        (Capability::AgentInstall, DenyReason::LocalOnly),
        (Capability::BackupsRestore, DenyReason::OwnerReserved),
        (Capability::RecoveryExport, DenyReason::OwnerReserved),
        (Capability::CredentialsManage, DenyReason::AdminReserved),
        (Capability::SecurityManage, DenyReason::AdminReserved),
    ] {
        let error = fixture
            .identities
            .apply_remote(
                &owner,
                RemoteCommand::SetOverride {
                    principal: target.principal.clone(),
                    grant: RoleGrant::new(capability, ResourceScope::ALL).expect("global grant"),
                    effect: Effect::Allow,
                    expiry: Expiry::Never,
                },
            )
            .expect_err("a reserved capability is never remotely grantable");
        assert_eq!(error.code(), "forbidden", "{}", capability.id());
        assert_eq!(error.reason(), Some(reason), "{}", capability.id());
        assert_eq!(
            fixture.count("principal_overrides"),
            before,
            "{}: nothing was inserted",
            capability.id()
        );
    }

    // A delegable capability through the same path does succeed, so the refusals
    // above are about the capability and not about the command.
    fixture
        .identities
        .apply_remote(
            &owner,
            RemoteCommand::SetOverride {
                principal: target.principal.clone(),
                grant: grant(Capability::FilesRead, "config/**"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        )
        .expect("a delegable override is allowed");
    assert_eq!(fixture.count("principal_overrides"), before + 1);
    assert_eq!(
        snapshot_of(&fixture, &target)
            .check(Capability::FilesRead, Some(&resource("config/a.txt"))),
        Decision::Allow
    );
}

// -------------------------- Java: primaryRoleReplacementAndExpiryApplyImmediately…

#[test]
fn a_local_role_assignment_replaces_every_previous_unrevoked_row() {
    let fixture = Fixture::open();
    let subject = fixture.subject(builtin(BuiltinRole::Viewer));
    assert_eq!(
        unrevoked_role_ids(&fixture, &subject.principal),
        vec!["viewer".to_owned()]
    );

    let LocalOutcome::Assignment(created) = fixture
        .identities
        .apply_local(
            Some(&fixture.proof(Purpose::LocalRoleGrant)),
            LocalCommand::AssignRole {
                principal: subject.principal.clone(),
                role: builtin(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        )
        .expect("assign moderator locally")
    else {
        panic!("a local role assignment returns its assignment id")
    };

    // The previous unrevoked assignment is revoked, not deleted or reused.
    assert_eq!(revoked_rows(&fixture, &subject.principal, "viewer"), 1);
    assert_eq!(
        unrevoked_assignment_ids(&fixture, &subject.principal),
        vec![assignment(created)]
    );
    assert_eq!(
        unrevoked_role_ids(&fixture, &subject.principal),
        vec!["moderator".to_owned()]
    );

    // Authority changed immediately, with no sweep at all.
    let policy = snapshot_of(&fixture, &subject);
    assert_eq!(policy.roles(), &[builtin(BuiltinRole::Moderator)]);
    assert_eq!(policy.rank(), BuiltinRole::Moderator.rank());
    assert_eq!(
        policy.check(Capability::PlayersModerate, None),
        Decision::Allow
    );
    assert_eq!(
        policy.check(Capability::StatusDetailed, None),
        Decision::Deny(DenyReason::NotGranted),
        "the viewer capability list no longer applies"
    );
    assert_eq!(fixture.identities.sweep_expired().expect("sweep"), 0);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_remote_role_assignment_replaces_the_primary_role_immediately() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let subject = fixture.subject(builtin(BuiltinRole::Viewer));
    assert!(fixture.identities.owner_exists().expect("owner lookup"));

    let change = fixture
        .identities
        .apply_remote(
            &owner,
            RemoteCommand::AssignRole {
                principal: subject.principal.clone(),
                role: builtin(BuiltinRole::Moderator),
                expiry: Expiry::Never,
            },
        )
        .expect("owner delegates moderator");
    let AuthorityChange::Assignment(created) = change else {
        panic!("a role assignment returns its assignment id")
    };
    // Owner + the fixture viewer row + the replacement row: replacement revokes,
    // it never deletes or reuses.
    assert_eq!(fixture.count("role_assignments"), 3);
    assert_eq!(revoked_rows(&fixture, &subject.principal, "viewer"), 1);
    assert_eq!(
        unrevoked_assignment_ids(&fixture, &subject.principal),
        vec![assignment(created.clone())]
    );

    // Authority changed with no sweep at all.
    let policy = snapshot_of(&fixture, &subject);
    assert_eq!(policy.roles(), &[builtin(BuiltinRole::Moderator)]);
    assert_eq!(policy.rank(), BuiltinRole::Moderator.rank());
    assert_eq!(
        policy.check(Capability::PlayersModerate, None),
        Decision::Allow
    );
    assert_eq!(
        policy.check(Capability::StatusDetailed, None),
        Decision::Deny(DenyReason::NotGranted)
    );
    assert_eq!(fixture.identities.sweep_expired().expect("sweep"), 0);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn an_expired_but_unrevoked_assignment_is_replaced_rather_than_resurrected() {
    let fixture = Fixture::open();
    let subject = fixture.subject(builtin(BuiltinRole::Viewer));
    fixture
        .identities
        .apply_local(
            Some(&fixture.proof(Purpose::LocalRoleGrant)),
            LocalCommand::AssignRole {
                principal: subject.principal.clone(),
                role: builtin(BuiltinRole::Moderator),
                expiry: Expiry::After(Duration::from_secs(1)),
            },
        )
        .expect("assign an expiring moderator role");
    fixture.clock.advance(1_000);
    assert_eq!(
        snapshot_of(&fixture, &subject).check(Capability::PlayersModerate, None),
        Decision::Deny(DenyReason::NotGranted),
        "the deadline already passed"
    );
    // Expired but not yet revoked: the row is still there, it simply grants nothing.
    assert_eq!(
        unrevoked_role_ids(&fixture, &subject.principal),
        vec!["moderator".to_owned()]
    );
    assert!(snapshot_of(&fixture, &subject).roles().is_empty());

    let LocalOutcome::Assignment(created) = fixture
        .identities
        .apply_local(
            Some(&fixture.proof(Purpose::LocalRoleGrant)),
            LocalCommand::AssignRole {
                principal: subject.principal.clone(),
                role: builtin(BuiltinRole::Viewer),
                expiry: Expiry::Never,
            },
        )
        .expect("assign viewer again")
    else {
        panic!("a role assignment returns its assignment id")
    };

    assert_eq!(revoked_rows(&fixture, &subject.principal, "moderator"), 1);
    assert_eq!(revoked_rows(&fixture, &subject.principal, "viewer"), 1);
    assert_eq!(
        unrevoked_assignment_ids(&fixture, &subject.principal),
        vec![assignment(created)]
    );
    let policy = snapshot_of(&fixture, &subject);
    assert_eq!(policy.roles(), &[builtin(BuiltinRole::Viewer)]);
    assert_eq!(policy.check(Capability::StatusView, None), Decision::Allow);
}

#[test]
fn authority_expires_at_the_exact_deadline_before_any_sweep_runs() {
    let fixture = Fixture::open();
    let subject = fixture.subject(builtin(BuiltinRole::Viewer));
    fixture
        .identities
        .apply_local(
            Some(&fixture.proof(Purpose::LocalRoleGrant)),
            LocalCommand::AssignRole {
                principal: subject.principal.clone(),
                role: builtin(BuiltinRole::Moderator),
                expiry: Expiry::After(Duration::from_secs(5)),
            },
        )
        .expect("assign an expiring moderator role");
    let deadline = INITIAL_TIME_MS + 5_000;
    let override_id = fixture.stored_override(
        &subject,
        Capability::FilesRead,
        "config/**",
        Effect::Allow,
        Some(deadline),
    );

    let options = resource("config/options.txt");
    let policy = snapshot_of(&fixture, &subject);
    assert_eq!(
        policy.check(Capability::PlayersModerate, None),
        Decision::Allow
    );
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&options)),
        Decision::Allow
    );

    fixture.clock.advance(4_999);
    assert_eq!(
        snapshot_of(&fixture, &subject).check(Capability::PlayersModerate, None),
        Decision::Allow,
        "one millisecond before the deadline the role still holds"
    );
    assert_eq!(
        snapshot_of(&fixture, &subject).check(Capability::FilesRead, Some(&options)),
        Decision::Allow
    );

    fixture.clock.advance(1);
    let expired = snapshot_of(&fixture, &subject);
    assert_eq!(
        expired.check(Capability::PlayersModerate, None),
        Decision::Deny(DenyReason::NotGranted),
        "the exact deadline is already expired"
    );
    assert_eq!(
        expired.check(Capability::FilesRead, Some(&options)),
        Decision::Deny(DenyReason::NotGranted),
        "the exact deadline is already expired"
    );
    assert_eq!(expired.rank(), 0);
    assert!(expired.roles().is_empty());
    assert!(expired.has_current_identity());
    assert_eq!(expired.principal(), Some(&subject.principal));
    assert!(!expired.holds_builtin(BuiltinRole::Moderator));
    assert_eq!(
        expiry_marker(&fixture, "principal_overrides", &override_id),
        None
    );

    // The sweep only records markers and evidence; it is never what expires anything.
    assert_eq!(fixture.identities.sweep_expired().expect("sweep"), 2);
    assert_eq!(fixture.identities.sweep_expired().expect("sweep"), 0);
    assert_eq!(count_action(&fixture, "authority.expired"), 2);
    assert_eq!(
        expiry_marker(&fixture, "principal_overrides", &override_id),
        Some(deadline)
    );
    let still = snapshot_of(&fixture, &subject);
    assert_eq!(still.rank(), 0);
    assert_eq!(
        still.check(Capability::FilesRead, Some(&options)),
        Decision::Deny(DenyReason::NotGranted)
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

#[test]
fn a_revoked_or_expired_override_has_no_effect() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let subject = fixture.subject(builtin(BuiltinRole::Viewer));
    let options = resource("config/options.txt");
    let permanent = fixture.stored_override(
        &subject,
        Capability::FilesRead,
        "config/**",
        Effect::Allow,
        None,
    );
    assert_eq!(
        snapshot_of(&fixture, &subject).check(Capability::FilesRead, Some(&options)),
        Decision::Allow
    );

    // Revoked through the production command, not by editing state.
    let change = fixture
        .identities
        .apply_remote(
            &owner,
            RemoteCommand::RevokeOverride(OverrideId::parse(&permanent).expect("override id")),
        )
        .expect("owner revokes the override");
    assert_eq!(change, AuthorityChange::Revoked);

    let policy = snapshot_of(&fixture, &subject);
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&options)),
        Decision::Deny(DenyReason::NotGranted)
    );
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&Resource::ROOT)),
        Decision::Deny(DenyReason::NotGranted)
    );
    assert!(!policy.holds_entirely(Capability::FilesRead, &ResourceScope::ALL));
    assert_eq!(fixture.identities.sweep_expired().expect("sweep"), 0);

    // The expiry deadline is exclusive, one millisecond at a time.
    let deadline = INITIAL_TIME_MS + 1_000;
    fixture.stored_override(
        &subject,
        Capability::FilesRead,
        "config/**",
        Effect::Allow,
        Some(deadline),
    );
    fixture.clock.advance(999);
    assert_eq!(
        snapshot_of(&fixture, &subject).check(Capability::FilesRead, Some(&options)),
        Decision::Allow,
        "still effective one millisecond before the deadline"
    );
    fixture.clock.advance(1);
    assert_eq!(
        snapshot_of(&fixture, &subject).check(Capability::FilesRead, Some(&options)),
        Decision::Deny(DenyReason::NotGranted),
        "the exact deadline is already expired"
    );
}

// ------------------------- Java: builtinRoleRowsAndCapabilityUpdateTargetsAreImmut…

#[test]
fn builtin_role_rows_and_capability_targets_are_immutable() {
    let fixture = Fixture::open();
    let administrator = fixture.subject(builtin(BuiltinRole::Administrator));
    let AuthorityChange::Role(custom) = fixture
        .identities
        .apply_remote(
            &administrator,
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse("Custom read").expect("role name"),
                    CustomRank::new(15).expect("rank"),
                    vec![grant(Capability::FilesRead, "config/**")],
                )
                .expect("custom role"),
            ),
        )
        .expect("administrator creates a custom role")
    else {
        panic!("role creation returns a role id")
    };
    let custom = custom.as_str().to_owned();
    let owner_capabilities = scalar::<i64>(
        &fixture,
        "SELECT count(*) FROM role_capabilities WHERE role_id = 'owner'",
    );

    // There is no role-edit command; every attempt to rewrite a built-in preset or
    // a built-in capability target is refused by the schema itself.
    for statement in [
        "UPDATE roles SET rank=1 WHERE id='owner'".to_owned(),
        "UPDATE roles SET name='Hijacked' WHERE id='owner'".to_owned(),
        "UPDATE roles SET created_by='fixture' WHERE id='owner'".to_owned(),
        "DELETE FROM roles WHERE id='owner'".to_owned(),
        "INSERT INTO roles (id, name, rank, builtin, created_by, created_at) \
         VALUES ('fake-owner', 'Fake Owner', 100, 0, 'fixture', 0)".to_owned(),
        "INSERT INTO roles (id, name, rank, builtin, created_by, created_at) \
         VALUES ('fake-builtin', 'Fake Builtin', 10, 1, 'fixture', 0)".to_owned(),
        format!("UPDATE roles SET builtin=1 WHERE id='{custom}'"),
        "UPDATE role_capabilities SET scope='config/**' WHERE role_id='owner' AND capability='files.read'"
            .to_owned(),
        format!("UPDATE role_capabilities SET role_id='owner' WHERE role_id='{custom}'"),
        "DELETE FROM role_capabilities WHERE role_id='owner'".to_owned(),
        "INSERT INTO role_capabilities (role_id, capability, scope) VALUES ('owner','fake.capability','*')"
            .to_owned(),
        "DELETE FROM role_capabilities WHERE role_id='administrator'".to_owned(),
    ] {
        let outcome = fixture.db.write(|connection| {
            connection.execute_batch(&statement)?;
            Ok(())
        });
        assert!(outcome.is_err(), "must be refused: {statement}");
    }

    // Nothing changed: the presets and their targets are exactly as migrated.
    assert_eq!(
        scalar::<String>(&fixture, "SELECT name FROM roles WHERE id='owner'"),
        "Owner".to_owned()
    );
    assert_eq!(
        scalar::<i32>(&fixture, "SELECT rank FROM roles WHERE id='owner'"),
        100
    );
    assert_eq!(
        scalar::<i32>(&fixture, "SELECT builtin FROM roles WHERE id='owner'"),
        1
    );
    assert_eq!(
        scalar::<i32>(
            &fixture,
            &format!("SELECT builtin FROM roles WHERE id='{custom}'")
        ),
        0
    );
    assert_eq!(
        scalar::<i64>(
            &fixture,
            "SELECT count(*) FROM role_capabilities WHERE role_id='owner'"
        ),
        owner_capabilities
    );
    assert_eq!(
        scalar::<String>(
            &fixture,
            "SELECT scope FROM role_capabilities WHERE role_id='owner' AND capability='files.read'"
        ),
        "*".to_owned()
    );
    assert_eq!(
        fixture.count("roles"),
        9,
        "six presets, two safe access defaults, and the one custom role"
    );
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}

/// Reads the independent constants fixture. `serde_json` is not a dependency of this
/// crate and this fixture has one fixed shape, so the fields are read directly.
const CONSTANTS_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../protocol/identity-v1/fixtures/capabilities-and-roles.json"
);

struct FixtureCapability {
    ordinal: i32,
    id: String,
    scoped: bool,
    local_only: bool,
    owner_reserved: bool,
    admin_reserved: bool,
    delegable: bool,
}

struct FixtureRole {
    id: String,
    name: String,
    rank: i32,
    local_grant_only: bool,
    capabilities: Vec<String>,
}

fn json_string(line: &str, key: &str) -> String {
    let needle = format!("\"{key}\": \"");
    let start = line
        .find(&needle)
        .unwrap_or_else(|| panic!("string field {key} missing from {line}"))
        + needle.len();
    let end = start
        + line[start..]
            .find('"')
            .unwrap_or_else(|| panic!("unterminated {key} in {line}"));
    line[start..end].to_owned()
}

fn json_int(line: &str, key: &str) -> i32 {
    let needle = format!("\"{key}\": ");
    let start = line
        .find(&needle)
        .unwrap_or_else(|| panic!("integer field {key} missing from {line}"))
        + needle.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    rest[..end]
        .parse()
        .unwrap_or_else(|_| panic!("{key} is not an integer in {line}"))
}

fn json_bool(line: &str, key: &str) -> bool {
    let needle = format!("\"{key}\": ");
    let start = line
        .find(&needle)
        .unwrap_or_else(|| panic!("boolean field {key} missing from {line}"))
        + needle.len();
    match line[start..]
        .split([',', '}'])
        .next()
        .unwrap_or_default()
        .trim()
    {
        "true" => true,
        "false" => false,
        other => panic!("{key} is not a boolean in {line}: {other}"),
    }
}

fn json_strings(line: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\": [");
    let start = line
        .find(&needle)
        .unwrap_or_else(|| panic!("array field {key} missing from {line}"))
        + needle.len();
    let rest = &line[start..];
    let end = rest
        .find(']')
        .unwrap_or_else(|| panic!("unterminated {key} in {line}"));
    rest[..end]
        .split(',')
        .map(|item| {
            let item = item.trim();
            item.strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or_else(|| panic!("{item} is not a JSON string in {line}"))
                .to_owned()
        })
        .collect()
}

fn read_constants_fixture() -> (Vec<FixtureCapability>, Vec<FixtureRole>) {
    let text = std::fs::read_to_string(CONSTANTS_FIXTURE)
        .unwrap_or_else(|error| panic!("read {CONSTANTS_FIXTURE}: {error}"));
    let mut capabilities = Vec::new();
    let mut roles = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with("{\"ordinal\":") {
            capabilities.push(FixtureCapability {
                ordinal: json_int(line, "ordinal"),
                id: json_string(line, "id"),
                scoped: json_bool(line, "scoped"),
                local_only: json_bool(line, "localOnly"),
                owner_reserved: json_bool(line, "ownerReserved"),
                admin_reserved: json_bool(line, "adminReserved"),
                delegable: json_bool(line, "delegable"),
            });
        } else if line.starts_with("{\"id\":") {
            roles.push(FixtureRole {
                id: json_string(line, "id"),
                name: json_string(line, "name"),
                rank: json_int(line, "rank"),
                local_grant_only: json_bool(line, "localGrantOnly"),
                capabilities: json_strings(line, "capabilities"),
            });
        }
    }
    (capabilities, roles)
}

#[test]
fn capability_and_builtin_role_constants_match_the_protocol_fixture() {
    let fixture = Fixture::open();
    let (fixture_capabilities, fixture_roles) = read_constants_fixture();

    // 51 capability ids, in persisted ordinal order, with the persisted flags.
    assert_eq!(fixture_capabilities.len(), 51);
    assert_eq!(Capability::ALL.len(), 57);
    assert_eq!(
        &Capability::ALL[51..],
        &[
            Capability::PublicInfo,
            Capability::PackDownload,
            Capability::PackRealtime,
            Capability::RolesManage,
            Capability::AccessSourcesManage,
            Capability::InvitationsCreate
        ]
    );
    for (index, expected) in fixture_capabilities.iter().enumerate() {
        let capability = Capability::ALL[index];
        assert_eq!(expected.ordinal, index as i32, "ordinal {index}");
        assert_eq!(expected.id, capability.id(), "capability {index}");
        assert_eq!(capability.id(), Capability::ALL[index].id());
        assert_eq!(
            Capability::from_id(&expected.id),
            Some(capability),
            "{} must resolve back to its own variant",
            expected.id
        );
        assert_eq!(expected.scoped, capability.scoped(), "{}", expected.id);
        assert_eq!(
            expected.local_only,
            capability.local_only(),
            "{}",
            expected.id
        );
        assert_eq!(
            expected.owner_reserved,
            capability.owner_reserved(),
            "{}",
            expected.id
        );
        assert_eq!(
            expected.admin_reserved,
            capability.admin_reserved(),
            "{}",
            expected.id
        );
        assert_eq!(
            expected.delegable,
            capability.delegable(),
            "{}",
            expected.id
        );
    }
    // The persisted reserved sets are exactly the ones the policy relies on.
    let persisted_ids = fixture_capabilities
        .iter()
        .map(|c| c.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        Capability::ALL
            .iter()
            .filter(|c| c.local_only())
            .map(|c| c.id())
            .collect::<Vec<_>>(),
        vec!["owner.recovery", "agent.install"]
    );
    assert_eq!(
        Capability::ALL
            .iter()
            .filter(|c| c.owner_reserved())
            .map(|c| c.id())
            .collect::<Vec<_>>(),
        vec!["backups.restore", "recovery.export"]
    );
    assert_eq!(
        Capability::ALL
            .iter()
            .filter(|c| c.admin_reserved())
            .map(|c| c.id())
            .collect::<Vec<_>>(),
        vec!["credentials.manage", "security.manage"]
    );

    // 6 built-in role ids, names, ranks, local-grant-only flags and capability
    // lists, both in code and as the rows this database actually migrated.
    assert_eq!(fixture_roles.len(), 6);
    assert_eq!(BuiltinRole::ALL.len(), 6);
    for (index, expected) in fixture_roles.iter().enumerate() {
        let role = BuiltinRole::ALL[index];
        assert_eq!(expected.id, role.id(), "role {index}");
        assert_eq!(expected.name, role.name(), "role {}", expected.id);
        assert_eq!(expected.rank, role.rank(), "role {}", expected.id);
        assert_eq!(
            expected.local_grant_only,
            role.local_grant_only(),
            "role {}",
            expected.id
        );
        assert_eq!(BuiltinRole::from_id(&expected.id), Some(role));
        let declared = role
            .capabilities()
            .iter()
            .map(|capability| capability.id())
            .collect::<Vec<_>>();
        // Keep the historical oracle fixture intact; V12 explicitly extends
        // the three pack-bearing presets with these unified capabilities.
        let mut expected_capabilities = expected.capabilities.clone();
        if let Some(at) = expected_capabilities
            .iter()
            .position(|c| c == "pack.access")
        {
            expected_capabilities.splice(
                at..at,
                [
                    "public.info".to_owned(),
                    "pack.download".to_owned(),
                    "pack.realtime".to_owned(),
                ],
            );
        }
        // V16 extends Owner and Administrator with remote permission management.
        if matches!(role, BuiltinRole::Owner | BuiltinRole::Administrator)
            && let Some(at) = expected_capabilities
                .iter()
                .position(|c| c == "pack.force_update")
        {
            expected_capabilities.splice(
                at + 1..at + 1,
                [
                    "roles.manage".to_owned(),
                    "access-sources.manage".to_owned(),
                    "invitations.create".to_owned(),
                ],
            );
        }
        assert_eq!(expected_capabilities, declared, "role {}", expected.id);

        // The migrated rows carry the same ids, name, rank and whole-scope grants.
        assert_eq!(
            scalar::<String>(
                &fixture,
                &format!("SELECT id FROM roles WHERE id='{}'", expected.id)
            ),
            expected.id
        );
        assert_eq!(
            scalar::<String>(
                &fixture,
                &format!("SELECT name FROM roles WHERE id='{}'", expected.id)
            ),
            expected.name
        );
        assert_eq!(
            scalar::<i32>(
                &fixture,
                &format!("SELECT rank FROM roles WHERE id='{}'", expected.id)
            ),
            expected.rank
        );
        assert_eq!(
            scalar::<i32>(
                &fixture,
                &format!("SELECT builtin FROM roles WHERE id='{}'", expected.id)
            ),
            1
        );
        let mut stored = text_column(
            &fixture,
            &format!(
                "SELECT capability || '|' || scope FROM role_capabilities WHERE role_id = '{}' ORDER BY capability",
                expected.id
            ),
        );
        let mut migrated = declared
            .iter()
            .map(|id| format!("{id}|*"))
            .collect::<Vec<_>>();
        stored.sort();
        migrated.sort();
        assert_eq!(
            stored, migrated,
            "persisted capabilities of {}",
            expected.id
        );
    }
    assert_eq!(
        scalar::<i64>(&fixture, "SELECT count(*) FROM roles WHERE builtin = 1"),
        6
    );
    assert_eq!(
        Capability::ALL.len(),
        persisted_ids.len() + 6,
        "legacy fixture ids, the three unified pack additions and the three V16 permission-management additions were compared"
    );
}

// ------------- Java: reservedCapabilitiesInCustomRoleRowsDoNotBecomeEffective…

#[test]
fn a_custom_role_row_with_a_reserved_capability_never_becomes_effective() {
    let fixture = Fixture::open();
    let administrator = fixture.subject(builtin(BuiltinRole::Administrator));
    let viewer = fixture.subject(builtin(BuiltinRole::Viewer));
    let AuthorityChange::Role(custom) = fixture
        .identities
        .apply_remote(
            &administrator,
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse("Near admin").expect("role name"),
                    CustomRank::new(79).expect("79 is the highest custom rank"),
                    vec![grant(Capability::StatusView, "*")],
                )
                .expect("custom role"),
            ),
        )
        .expect("administrator creates a near-admin custom role")
    else {
        panic!("role creation returns a role id")
    };
    fixture
        .identities
        .apply_remote(
            &administrator,
            RemoteCommand::AssignRole {
                principal: viewer.principal.clone(),
                role: custom.clone(),
                expiry: Expiry::Never,
            },
        )
        .expect("administrator assigns the custom role");

    // Plant the reserved rows directly: a custom role has no schema barrier, only
    // the policy's tier check at evaluation time.
    for capability in [
        Capability::BackupsRestore,
        Capability::RecoveryExport,
        Capability::SecurityManage,
        Capability::CredentialsManage,
        Capability::OwnerRecovery,
        Capability::DevicesRevoke,
    ] {
        fixture.sql(&format!(
            "INSERT INTO role_capabilities (role_id, capability, scope) VALUES ('{}','{}','*')",
            custom.as_str(),
            capability.id()
        ));
    }

    let policy = snapshot_of(&fixture, &viewer);
    assert_eq!(policy.rank(), 79, "the numeric rank alone");
    assert!(!policy.holds_builtin(BuiltinRole::Owner));
    assert!(!policy.holds_builtin(BuiltinRole::Administrator));
    for capability in [
        Capability::BackupsRestore,
        Capability::RecoveryExport,
        Capability::SecurityManage,
        Capability::CredentialsManage,
        Capability::OwnerRecovery,
    ] {
        assert!(
            !policy.check(capability, None).allowed(),
            "{} must not become effective",
            capability.id()
        );
        assert!(
            !policy.holds_entirely(capability, &ResourceScope::ALL),
            "{} must not become delegable",
            capability.id()
        );
    }
    // A delegable capability in the same row still works, so the rows are read.
    assert_eq!(
        policy.check(Capability::DevicesRevoke, None),
        Decision::Allow
    );
    assert_eq!(policy.check(Capability::StatusView, None), Decision::Allow);

    // Rank 79 is still strictly below an administrator: it can revoke a viewer but
    // not another administrator.
    let error = fixture
        .identities
        .apply_remote(
            &viewer,
            RemoteCommand::RevokeDevice(administrator.device.clone()),
        )
        .expect_err("79 is not above the administrator tier");
    assert_eq!(error.code(), "forbidden");
    assert_eq!(error.reason(), Some(DenyReason::Rank));
    let lower = fixture.subject(builtin(BuiltinRole::Moderator));
    assert_eq!(
        fixture
            .identities
            .apply_remote(&viewer, RemoteCommand::RevokeDevice(lower.device.clone()))
            .expect("79 is above a moderator"),
        AuthorityChange::Revoked
    );
    assert!(
        !snapshot_of(&fixture, &lower).has_current_identity(),
        "the device really was revoked"
    );
}

// ---- Java: malformedStoredAllowDoesNotGrantAndInvalidResourceNeverMatchesAGrant

#[test]
fn a_malformed_stored_allow_never_grants() {
    let fixture = Fixture::open();
    let subject = fixture.subject(builtin(BuiltinRole::Viewer));
    let unreadable = ["", "../invalid", "a//b", "logs\\x", "config/CON", "logs/x/"];
    for stored in unreadable {
        assert!(
            ResourceScope::parse(stored).is_err(),
            "{stored:?} must be an unreadable stored scope"
        );
        fixture.stored_override(&subject, Capability::FilesRead, stored, Effect::Allow, None);
    }
    assert_eq!(
        fixture.count("principal_overrides"),
        unreadable.len() as i64
    );

    let policy = snapshot_of(&fixture, &subject);
    for text in ["config/options.txt", "logs/latest.log", "a/b"] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(text))),
            Decision::Deny(DenyReason::NotGranted),
            "{text}: no grant may come from an unreadable row"
        );
    }
    assert!(!policy.holds_entirely(Capability::FilesRead, &ResourceScope::ALL));

    // A later, readable row proves the resolution itself is healthy and that the
    // unreadable rows contributed nothing beyond their own absence.
    fixture.stored_override(
        &subject,
        Capability::FilesRead,
        "config/**",
        Effect::Allow,
        None,
    );
    let policy = snapshot_of(&fixture, &subject);
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("config/options.txt"))),
        Decision::Allow
    );
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("logs/latest.log"))),
        Decision::Deny(DenyReason::NotGranted),
        "an unreadable row cannot have widened the readable one"
    );
}

#[test]
fn an_unreadable_resource_can_never_match_a_whole_path_grant() {
    let fixture = Fixture::open();
    let subject = fixture.subject(builtin(BuiltinRole::Viewer));
    fixture.stored_override(&subject, Capability::FilesRead, "*", Effect::Allow, None);
    let policy = snapshot_of(&fixture, &subject);
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&Resource::ROOT)),
        Decision::Allow,
        "the whole-scope grant is readable"
    );

    // Each of these has no valid Resource value at all, so no grant can match it.
    for text in [
        "../outside",
        "C:/root",
        "//host/share",
        "config/a:stream",
        "logs/CON",
        "config/./x",
        "config//x",
        "config/*/x",
        "config/",
    ] {
        assert!(Resource::parse(text).is_err(), "{text} must be rejected");
        assert!(
            ResourceScope::parse(text).is_err(),
            "{text} must be rejected as a scope"
        );
    }
}

#[test]
fn a_scoped_check_requires_a_resource_and_a_global_check_forbids_one() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let policy = snapshot_of(&fixture, &owner);

    assert_eq!(
        policy.check(Capability::FilesRead, Some(&Resource::ROOT)),
        Decision::Allow
    );
    assert_eq!(
        policy.check(Capability::FilesRead, None),
        Decision::Deny(DenyReason::InvalidResource)
    );
    assert_eq!(policy.check(Capability::StatusView, None), Decision::Allow);
    assert_eq!(
        policy.check(Capability::StatusView, Some(&Resource::ROOT)),
        Decision::Deny(DenyReason::InvalidResource)
    );
    // The reason is reported in the same shape as every other decision.
    assert_eq!(
        policy
            .check(Capability::StatusView, Some(&Resource::ROOT))
            .reason(),
        "invalid_resource"
    );
    assert!(policy.holds_entirely(Capability::FilesRead, &ResourceScope::ALL));
    assert!(policy.holds_entirely(Capability::StatusView, &ResourceScope::ALL));
    assert!(!policy.holds_entirely(Capability::StatusView, &scope("config/**")));
}

#[test]
fn a_subtree_scope_never_matches_a_whole_unrelated_path() {
    let fixture = Fixture::open();
    let subject = fixture.subject(builtin(BuiltinRole::Viewer));
    fixture.stored_override(
        &subject,
        Capability::FilesRead,
        "config/**",
        Effect::Allow,
        None,
    );
    let policy = snapshot_of(&fixture, &subject);

    for granted in ["config", "config/options.txt", "config/nested/a.txt"] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(granted))),
            Decision::Allow,
            "{granted}"
        );
    }
    for denied in [
        "",
        "configuration/a.txt",
        "configx/a.txt",
        "confi",
        "CONFIG/a.txt",
        "Config",
        "logs/latest.log",
        "other/config/a.txt",
    ] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(denied))),
            Decision::Deny(DenyReason::NotGranted),
            "{denied:?} is outside the subtree"
        );
    }

    // The lexical rules behind it: segment-bounded, case-sensitive allows,
    // case-insensitive denies, and no partial-scope union.
    let subtree = scope("config/**");
    assert!(subtree.allows(&resource("config")));
    assert!(subtree.allows(&resource("config/nested/a.txt")));
    assert!(!subtree.allows(&resource("configuration/a.txt")));
    assert!(!subtree.allows(&resource("CONFIG/a.txt")));
    assert!(subtree.denies(&resource("CONFIG/a.txt")));
    assert!(subtree.covers(&scope("config/nested/**")));
    assert!(!scope("config/a.txt").covers(&scope("config/a.txt/**")));
    assert!(!subtree.covers(&ResourceScope::ALL));
    assert!(ResourceScope::ALL.covers(&subtree));
}

// ---------------- Java: expiryAuditFailureDoesNotMarkExpiredAuthorityAsReported

#[test]
fn an_expiry_audit_failure_leaves_the_marker_unrecorded_and_a_later_sweep_records_it_once() {
    let fixture = Fixture::open();
    let subject = fixture.subject(builtin(BuiltinRole::Viewer));
    let options = resource("config/options.txt");
    let deadline = INITIAL_TIME_MS + 1_000;
    let id = fixture.stored_override(
        &subject,
        Capability::FilesRead,
        "config/**",
        Effect::Allow,
        Some(deadline),
    );
    fixture.clock.advance(1_000);

    // Expiry is already effective, before any sweep is asked to run.
    assert_eq!(
        snapshot_of(&fixture, &subject).check(Capability::FilesRead, Some(&options)),
        Decision::Deny(DenyReason::NotGranted)
    );

    fixture.audit_failure(true);
    let error = fixture
        .identities
        .sweep_expired()
        .expect_err("the audit append must fail the sweep");
    assert!(
        matches!(error, crate::ControlError::State(_)),
        "an infrastructure failure, not a decision: {error:?}"
    );
    assert!(error.reason().is_none());
    // The marker and its audit share one transaction, so a failed audit cannot
    // mark the expiry as already reported.
    assert_eq!(expiry_marker(&fixture, "principal_overrides", &id), None);
    assert_eq!(count_action(&fixture, "authority.expired"), 0);

    // Still not effective, exactly as before the failed sweep.
    assert_eq!(
        snapshot_of(&fixture, &subject).check(Capability::FilesRead, Some(&options)),
        Decision::Deny(DenyReason::NotGranted)
    );

    fixture.audit_failure(false);
    assert_eq!(fixture.identities.sweep_expired().expect("sweep"), 1);
    assert_eq!(fixture.identities.sweep_expired().expect("sweep"), 0);
    assert_eq!(
        expiry_marker(&fixture, "principal_overrides", &id),
        Some(deadline)
    );
    assert_eq!(count_action(&fixture, "authority.expired"), 1);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
}
